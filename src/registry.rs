//! Reads Claude Code's local session registry (`~/.claude/sessions/*.json`).
//!
//! Every running `claude` process writes a JSON descriptor there containing its
//! name, cwd, busy/idle status and the named pipe it listens on. We use it for
//! two things: enriching the sessions we spawned ourselves (matched by child
//! PID) and listing foreign sessions we cannot attach to.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

#[derive(Clone, Debug)]
pub struct RegistryEntry {
    pub pid: u32,
    pub cwd: String,
    pub name: String,
    /// `busy`, `idle` or `waiting`. A session that stopped on a permission
    /// prompt or any other dialog reports `waiting`, not `idle`: nothing is
    /// running, but nothing moves until someone answers it either.
    pub status: String,
    /// What a `waiting` session is waiting for, as Claude Code words it —
    /// `dialog open`, `input needed`, `permission prompt`.
    pub waiting_for: String,
    pub started_at: u64,
    /// The conversation the process is holding — the transcript's file name,
    /// and what `claude --resume` takes. Empty until the process has one.
    pub session_id: String,
}

impl RegistryEntry {
    /// Stopped on a question only the user can answer.
    pub fn is_waiting(&self) -> bool {
        self.status == "waiting"
    }

    pub fn cwd_label(&self) -> String {
        Path::new(&self.cwd)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.cwd.clone())
    }
}

pub fn claude_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude"))
}

fn sessions_dir() -> Option<PathBuf> {
    claude_dir().map(|c| c.join("sessions"))
}

/// Names of the `cc-msg-*` named pipes currently open.
///
/// A registry file is only removed on a clean exit, so a crashed session can
/// leave a stale descriptor behind. The pipe disappears with the process, which
/// makes it the reliable liveness signal. On non-Windows this returns `None`
/// and every entry is reported as live.
fn live_pipes() -> Option<HashSet<String>> {
    if !cfg!(windows) {
        return None;
    }
    let entries = fs::read_dir(r"\\.\pipe\").ok()?;
    Some(
        entries
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .collect(),
    )
}

/// The key a pipe is listed under, derived from a recorded socket path.
///
/// Pipe names are a flat namespace in which `\` is an ordinary character, so
/// `\\.\pipe\LOCAL\cc-msg-abc` is listed as the single name `LOCAL\cc-msg-abc`.
/// Only the `\\.\pipe\` prefix may be stripped, never the `LOCAL\` part.
fn pipe_key(socket_path: &str) -> Option<String> {
    let normalized = socket_path.replace('/', "\\");
    let key = normalized
        .strip_prefix(r"\\.\pipe\")
        .unwrap_or(normalized.as_str());
    (!key.is_empty()).then(|| key.to_owned())
}

pub fn read_all() -> Vec<RegistryEntry> {
    let Some(dir) = sessions_dir() else {
        return Vec::new();
    };
    let pipes = live_pipes();

    let mut out: Vec<RegistryEntry> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let raw = fs::read_to_string(e.path()).ok()?;
            let v: Value = serde_json::from_str(&raw).ok()?;

            let socket = v["messagingSocketPath"].as_str().unwrap_or_default();
            let live = match (&pipes, pipe_key(socket)) {
                (Some(set), Some(key)) => {
                    set.contains(&key)
                        // Fall back to the bare name in case a Windows build
                        // ever reports the namespace without the LOCAL prefix.
                        || key.rsplit('\\').next().is_some_and(|tail| set.contains(tail))
                }
                // No pipe listing available, or no socket recorded: assume live.
                _ => true,
            };

            live.then(|| RegistryEntry {
                pid: v["pid"].as_u64().unwrap_or(0) as u32,
                cwd: v["cwd"].as_str().unwrap_or_default().to_owned(),
                name: v["name"].as_str().unwrap_or("?").to_owned(),
                status: v["status"].as_str().unwrap_or("unknown").to_owned(),
                waiting_for: v["waitingFor"].as_str().unwrap_or_default().to_owned(),
                started_at: v["startedAt"].as_u64().unwrap_or(0),
                session_id: v["sessionId"].as_str().unwrap_or_default().to_owned(),
            })
        })
        .filter(|e| e.pid != 0)
        .collect();

    out.sort_by_key(|e| e.started_at);
    out
}

pub fn find_by_pid(entries: &[RegistryEntry], pid: u32) -> Option<&RegistryEntry> {
    entries.iter().find(|e| e.pid == pid)
}

/// Working directories Claude Code has been run in, newest first.
///
/// The directory names under `projects/` are a lossy slug of the path, so the
/// real cwd is recovered from the `cwd` field of the newest transcript instead
/// of by decoding the slug.
pub fn recent_cwds(limit: usize) -> Vec<PathBuf> {
    let Some(projects) = claude_dir().map(|c| c.join("projects")) else {
        return Vec::new();
    };

    let mut found: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(&projects)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|proj| {
            let newest = fs::read_dir(proj.path())
                .ok()?
                .flatten()
                .filter(|f| f.path().extension().is_some_and(|x| x == "jsonl"))
                .filter_map(|f| Some((f.metadata().ok()?.modified().ok()?, f.path())))
                .max_by_key(|(t, _)| *t)?;

            let cwd = cwd_from_transcript(&newest.1)?;
            Some((newest.0, PathBuf::from(cwd)))
        })
        .collect();

    found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));

    let mut seen = HashSet::new();
    found
        .into_iter()
        .map(|(_, p)| p)
        .filter(|p| p.is_dir() && seen.insert(p.clone()))
        .take(limit)
        .collect()
}

fn cwd_from_transcript(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let file = fs::File::open(path).ok()?;
    // The cwd is recorded on every record; the first line is enough.
    for line in BufReader::new(file).lines().take(8).map_while(Result::ok) {
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(cwd) = v["cwd"].as_str()
            && !cwd.is_empty()
        {
            return Some(cwd.to_owned());
        }
    }
    None
}

/// Debug helper: the raw pipe names as `read_dir` reports them.
pub fn debug_pipe_names() -> Vec<String> {
    match live_pipes() {
        Some(set) => {
            let mut v: Vec<String> = set.into_iter().collect();
            v.sort();
            v
        }
        None => vec!["<read_dir on the pipe namespace failed>".to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_key_keeps_the_local_segment() {
        let key = pipe_key(r"\\.\pipe\LOCAL\cc-msg-abc123").unwrap();
        assert_eq!(key, r"LOCAL\cc-msg-abc123");
    }

    #[test]
    fn pipe_key_passes_through_a_bare_name() {
        let key = pipe_key("cc-msg-abc123").unwrap();
        assert_eq!(key, "cc-msg-abc123");
    }

    #[test]
    fn pipe_key_rejects_an_empty_path() {
        assert!(pipe_key("").is_none());
    }
}
