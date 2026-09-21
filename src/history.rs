//! Past conversations, read out of the transcripts Claude Code keeps.
//!
//! Every session writes `~/.claude/projects/<slug>/<session-id>.jsonl`, one
//! JSON object per line, where the slug is the working directory with every
//! character a path separator could be replaced by a dash. The file name is
//! the session id — the thing `claude --resume` takes — and the first ordinary
//! user message in it is the closest thing a conversation has to a title.
//!
//! Fleet only reads here, as it does for the session registry and the usage
//! cache. Resuming is spawning `claude --resume <id>`; the transcript itself
//! belongs to Claude Code.

use std::{
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde_json::Value;

/// How far into a transcript to look for the message that names it.
///
/// The first user message is within the first few lines of every transcript
/// there is: hooks, mode markers and attachments come before it, and little
/// else. A cap keeps a pathological file from being read to its end for a
/// title nobody would have read anyway.
const HEAD_LINES: usize = 400;

/// How many transcripts to open to fill a list of `n`. Some hold no user
/// message at all — a session opened and closed again — and those are skipped,
/// so the candidate list has to be longer than the answer.
const OVERSCAN: usize = 3;

/// One resumable conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    /// What `claude --resume` takes: the transcript's file name.
    pub id: String,
    /// Where it was held, as the transcript itself records it.
    pub cwd: PathBuf,
    /// Its first user message, which is what it is about.
    pub summary: String,
    /// When it was last written to, which is when it was last alive.
    pub modified: SystemTime,
}

impl Conversation {
    /// The directory's own name, for a list that has no room for the path.
    pub fn cwd_label(&self) -> String {
        self.cwd
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.cwd.display().to_string())
    }
}

fn projects_dir() -> Option<PathBuf> {
    crate::registry::claude_dir().map(|c| c.join("projects"))
}

/// The directory name Claude Code files a working directory's transcripts
/// under: every character that is not a letter or a digit becomes a dash, so
/// `C:\Users\p\Desktop\fleet` is `C--Users-p-Desktop-fleet`.
fn slug(cwd: &Path) -> String {
    cwd.display()
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The most recent conversations across every project, newest first.
pub fn recent(limit: usize) -> Vec<Conversation> {
    let Some(root) = projects_dir() else {
        return Vec::new();
    };
    let Ok(projects) = fs::read_dir(&root) else {
        return Vec::new();
    };

    let mut files: Vec<(SystemTime, PathBuf)> = Vec::new();
    for project in projects.flatten() {
        if project.file_type().is_ok_and(|t| t.is_dir()) {
            files.extend(transcripts(&project.path()));
        }
    }
    newest_first(&mut files);
    read_upto(files, limit)
}

/// Whether a transcript with this id exists in any project.
///
/// A session that never got a message has an id in the registry but nothing
/// on disk, and `claude --resume` refuses an id it cannot find.
pub fn exists(id: &str) -> bool {
    let Some(root) = projects_dir() else {
        return false;
    };
    let Ok(projects) = fs::read_dir(&root) else {
        return false;
    };
    let name = format!("{id}.jsonl");
    projects
        .flatten()
        .any(|p| p.path().join(&name).is_file())
}

/// Where the transcript with this id lives, in whichever project holds it.
pub fn transcript_path(id: &str) -> Option<PathBuf> {
    let root = projects_dir()?;
    let name = format!("{id}.jsonl");
    fs::read_dir(&root)
        .ok()?
        .flatten()
        .map(|p| p.path().join(&name))
        .find(|p| p.is_file())
}

/// The conversations held in one directory, newest first.
///
/// This is the restart path: a session that is about to be killed is the one
/// whose transcript was written to last, so the ids come back in the order the
/// sessions were last alive in.
pub fn latest_in(cwd: &Path, limit: usize) -> Vec<Conversation> {
    let Some(root) = projects_dir() else {
        return Vec::new();
    };
    let mut files = transcripts(&root.join(slug(cwd)));
    newest_first(&mut files);
    read_upto(files, limit)
        .into_iter()
        // The slug is a guess at Claude Code's naming, the recorded cwd is not.
        .filter(|c| same_dir(&c.cwd, cwd))
        .collect()
}

/// Windows paths differ in case without differing at all.
fn same_dir(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.display()
            .to_string()
            .eq_ignore_ascii_case(&b.display().to_string())
    } else {
        a == b
    }
}

fn transcripts(dir: &Path) -> Vec<(SystemTime, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| {
            let stamp = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((stamp, e.path()))
        })
        .collect()
}

fn newest_first(files: &mut [(SystemTime, PathBuf)]) {
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
}

fn read_upto(files: Vec<(SystemTime, PathBuf)>, limit: usize) -> Vec<Conversation> {
    let mut out = Vec::new();
    for (stamp, path) in files.into_iter().take(limit.saturating_mul(OVERSCAN)) {
        if out.len() == limit {
            break;
        }
        if let Some(c) = read_one(&path, stamp) {
            out.push(c);
        }
    }
    out
}

fn read_one(path: &Path, modified: SystemTime) -> Option<Conversation> {
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let file = File::open(path).ok()?;

    for line in BufReader::new(file)
        .lines()
        .take(HEAD_LINES)
        .map_while(Result::ok)
    {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("user") {
            continue;
        }
        // A sidechain is a subagent talking to itself inside someone else's
        // conversation; resuming one is not a thing, and its first message
        // would describe the errand rather than the session.
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        // Tool results are user messages too, structurally. A real one is a
        // string, and the first of those is what the session was asked for.
        let Some(text) = v.pointer("/message/content").and_then(Value::as_str) else {
            continue;
        };
        let summary = clean(text);
        if summary.is_empty() {
            continue;
        }
        let cwd = v.get("cwd").and_then(Value::as_str)?;
        return Some(Conversation {
            id,
            cwd: PathBuf::from(cwd),
            summary,
            modified,
        });
    }
    None
}

/// A prompt as it reads in a list: one line, with the machinery taken out.
///
/// Hooks and the harness wrap things around what was typed — reminders, the
/// name of a slash command, pasted file contents. None of it identifies the
/// conversation, and all of it comes first, so a title made of it would be the
/// same title on every row.
pub fn clean(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        // A tag we know is dropped with its contents; anything else is text
        // that merely starts with a bracket.
        let tag = [
            "system-reminder",
            "command-name",
            "command-message",
            "command-args",
            "local-command-stdout",
        ]
        .iter()
        .find(|t| after.starts_with(&format!("<{t}")));
        let Some(tag) = tag else {
            out.push('<');
            rest = &after[1..];
            continue;
        };
        let close = format!("</{tag}>");
        rest = match after.find(&close) {
            Some(at) => &after[at + close.len()..],
            // An unterminated block runs to the end; nothing after it survives.
            None => "",
        };
    }
    out.push_str(rest);

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_working_directory_becomes_the_name_claude_files_it_under() {
        assert_eq!(
            slug(Path::new(r"C:\Users\piotr\Desktop\claude-fleet")),
            "C--Users-piotr-Desktop-claude-fleet"
        );
        // Dots and spaces are separators like any other.
        assert_eq!(slug(Path::new(r"C:\a b\c.d")), "C--a-b-c-d");
    }

    #[test]
    fn a_title_is_what_was_typed_not_what_was_wrapped_around_it() {
        let wrapped =
            "<system-reminder>\nrules and more rules\n</system-reminder>fix   the bar\nchart";
        assert_eq!(clean(wrapped), "fix the bar chart");
        assert_eq!(clean("<command-name>/loop</command-name>"), "");
        // A bracket that opens nothing known is ordinary text.
        assert_eq!(clean("a < b"), "a < b");
    }

    #[test]
    fn a_transcript_is_named_by_its_first_real_user_message() {
        let dir = std::env::temp_dir().join(format!("fleet-history-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("11111111-2222-3333-4444-555555555555.jsonl");
        let body = [
            r#"{"type":"mode","mode":"normal"}"#,
            // A subagent's opening line, which is not the session's.
            r#"{"type":"user","isSidechain":true,"cwd":"C:\\x","message":{"content":"search the files"}}"#,
            // A tool result: a user message with no user in it.
            r#"{"type":"user","cwd":"C:\\x","message":{"content":[{"type":"tool_result"}]}}"#,
            r#"{"type":"user","cwd":"C:\\x","message":{"content":"<system-reminder>x</system-reminder>make the bar bigger"}}"#,
            r#"{"type":"user","cwd":"C:\\x","message":{"content":"and one more thing"}}"#,
        ]
        .join("\n");
        fs::write(&path, body).unwrap();

        let c = read_one(&path, SystemTime::UNIX_EPOCH).expect("transkrypt ma sie czytac");
        assert_eq!(c.id, "11111111-2222-3333-4444-555555555555");
        assert_eq!(c.summary, "make the bar bigger");
        assert_eq!(c.cwd, PathBuf::from(r"C:\x"));

        // A transcript nobody said anything in names nothing.
        let empty = dir.join("00000000-0000-0000-0000-000000000000.jsonl");
        fs::write(&empty, r#"{"type":"mode","mode":"normal"}"#).unwrap();
        assert!(read_one(&empty, SystemTime::UNIX_EPOCH).is_none());

        let _ = fs::remove_dir_all(&dir);
    }
}
