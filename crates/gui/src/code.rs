//! The code view: files opened for editing, and git history down to one
//! file's diff in one commit.
//!
//! Every path the page sends is relative to `base` — the top of the work tree,
//! or the session's folder outside one — with forward slashes, the way git
//! and `files_list` write them.

use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use fleet_core::git::{self, DiffSource};
use serde::{Deserialize, Serialize};

use crate::Res;

/// Bigger than this is not opened: an editor in a web page slows to a crawl
/// long before, and nobody edits a 5 MB file by hand.
const OPEN_LIMIT: u64 = 5 * 1024 * 1024;

/// `base` joined with `rel`, refused when `rel` would climb out of it.
fn resolve(base: &str, rel: &str) -> Res<PathBuf> {
    let rel = Path::new(rel);
    if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) {
        return Err(format!("{} is not inside the project", rel.display()));
    }
    Ok(Path::new(base).join(rel))
}

/// When the file last changed, as a number the page hands back on save so a
/// change made meanwhile — by a session, say — is not written over.
fn stamp(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedFile {
    text: String,
    stamp: u64,
    /// Not text: nothing to show.
    binary: bool,
    /// Not valid UTF-8, shown with stand-ins, so saving would break it.
    lossy: bool,
}

#[tauri::command]
pub async fn file_read(base: String, path: String) -> Res<OpenedFile> {
    let full = resolve(&base, &path)?;
    let meta = fs::metadata(&full).map_err(|e| format!("{path}: {e}"))?;
    if meta.is_dir() {
        return Err(format!("{path} is a folder"));
    }
    if meta.len() > OPEN_LIMIT {
        return Err(format!("{path} is {} MB, too big to open here", meta.len() / (1024 * 1024)));
    }
    let bytes = fs::read(&full).map_err(|e| format!("{path}: {e}"))?;
    let stamp = stamp(&full);
    if bytes.iter().take(8000).any(|&b| b == 0) {
        return Ok(OpenedFile {
            text: String::new(),
            stamp,
            binary: true,
            lossy: false,
        });
    }
    let (text, lossy) = match String::from_utf8(bytes) {
        Ok(t) => (t, false),
        Err(e) => (String::from_utf8_lossy(e.as_bytes()).into_owned(), true),
    };
    Ok(OpenedFile {
        text,
        stamp,
        binary: false,
        lossy,
    })
}

/// When the file last changed; 0 once it is gone.
#[tauri::command]
pub async fn file_stamp(base: String, path: String) -> Res<u64> {
    Ok(stamp(&resolve(&base, &path)?))
}

/// Write the file, unless it changed on disk since `expected` was read —
/// then the page asks whether to overwrite, and calls again without it.
#[tauri::command]
pub async fn file_write(base: String, path: String, text: String, expected: Option<u64>) -> Res<u64> {
    let full = resolve(&base, &path)?;
    if let Some(expected) = expected {
        let now = stamp(&full);
        if now != 0 && now != expected {
            return Err("changed on disk".into());
        }
    }
    if let Some(dir) = full.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{path}: {e}"))?;
    }
    fs::write(&full, text).map_err(|e| format!("{path}: {e}"))?;
    Ok(stamp(&full))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    hash: String,
    subject: String,
    ago_secs: u64,
    author: String,
    /// The file's name in this commit, for a file's own history.
    path: Option<String>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A page of history: every commit, or with `path` the ones that touched it.
#[tauri::command]
pub async fn git_history(base: String, path: Option<String>, skip: usize, limit: usize) -> Vec<HistoryEntry> {
    let root = Path::new(&base);
    let now = now_secs();
    let entry = |c: git::Commit, path: Option<String>| HistoryEntry {
        hash: c.hash,
        subject: c.subject,
        ago_secs: now.saturating_sub(c.time),
        author: c.author,
        path,
    };
    match path {
        Some(p) => git::file_history(root, &p, skip, limit)
            .into_iter()
            .map(|(c, p)| entry(c, Some(p)))
            .collect(),
        None => git::history(root, skip, limit)
            .into_iter()
            .map(|c| entry(c, None))
            .collect(),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitFile {
    path: String,
    added: Option<u32>,
    removed: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitInfo {
    author: String,
    email: String,
    date: String,
    message: String,
    files: Vec<CommitFile>,
}

#[tauri::command]
pub async fn git_commit_info(base: String, hash: String) -> Res<CommitInfo> {
    let d = git::commit_detail(Path::new(&base), &hash).ok_or_else(|| format!("no commit {hash}"))?;
    Ok(CommitInfo {
        author: d.author,
        email: d.email,
        date: d.date,
        message: d.message,
        files: d
            .files
            .into_iter()
            .map(|f| CommitFile {
                path: f.path,
                added: f.added,
                removed: f.removed,
            })
            .collect(),
    })
}

/// What a diff is of, as the page names it.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DiffOf {
    /// Uncommitted changes to a file, against HEAD.
    Worktree { path: String, untracked: bool },
    /// What one commit did to a file.
    Commit { hash: String, path: String },
}

#[tauri::command]
pub async fn git_diff(base: String, of: DiffOf) -> Vec<String> {
    let root = Path::new(&base);
    let src = match of {
        DiffOf::Worktree { path, untracked } => DiffSource::Worktree {
            path,
            untracked,
            has_head: git::has_head(root),
        },
        DiffOf::Commit { hash, path } => DiffSource::Commit { hash, path },
    };
    git::diff(root, &src)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_cannot_climb_out_of_the_project() {
        assert!(resolve("C:/p", "src/a.rs").is_ok());
        assert!(resolve("C:/p", "./src/a.rs").is_ok());
        assert!(resolve("C:/p", "../secret").is_err());
        assert!(resolve("C:/p", "src/../../x").is_err());
        assert!(resolve("/p", "/etc/passwd").is_err());
    }
}
