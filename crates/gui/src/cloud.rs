//! Cloud sessions: Claude Code sessions that run on Anthropic's machines,
//! the ones claude.ai/code shows.
//!
//! The app starts one with `claude --cloud "<task>"` under a pseudoterminal
//! (a new cloud session is interactive only: `-p` can only message one that
//! exists) and attaches to an existing one with `claude --cloud <id>`. The id
//! is read off what Claude Code prints, the `claude.ai/code/...` link, and
//! the sessions seen are kept here so they can be attached again later.

use std::path::Path;

use serde::{Deserialize, Serialize};

const LINK: &str = "claude.ai/code/";
const KEEP: usize = 20;

/// A cloud session's id (`cse_...` or `session_...`) from the id itself or
/// a claude.ai/code link to it.
pub fn id_of(text: &str) -> Option<String> {
    let t = text.trim();
    let tail = match t.find(LINK) {
        Some(at) => &t[at + LINK.len()..],
        None => t,
    };
    let id: String = tail
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    let rest = &tail[id.len()..];
    let whole = t.contains(LINK) || rest.is_empty();
    let shaped = ["cse_", "session_"]
        .iter()
        .any(|p| id.len() > p.len() + 3 && id.starts_with(p));
    (whole && shaped).then_some(id)
}

/// The first claude.ai/code link to a cloud session in a stretch of terminal
/// output, escape sequences and all. Returns the id and the link.
pub fn find_link(output: &str) -> Option<(String, String)> {
    let plain = strip_escapes(output);
    for text in [output, plain.as_str()] {
        let mut from = 0;
        while let Some(at) = text[from..].find(LINK) {
            let start = from + at;
            if let Some(id) = id_of(&text[start..]) {
                return Some((id.clone(), url_of(&id)));
            }
            from = start + LINK.len();
        }
    }
    None
}

pub fn url_of(id: &str) -> String {
    format!("https://claude.ai/code/{id}")
}

/// Terminal output without its escape sequences (CSI, OSC and the two-byte
/// ones), so a link the screen drew in pieces reads as one.
fn strip_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// A cloud session the app has seen, for attaching to it again.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Recent {
    pub id: String,
    pub url: String,
    /// What it was started to do, when it was started here.
    #[serde(default)]
    pub task: String,
    pub cwd: String,
    /// Milliseconds since the epoch.
    pub at: u64,
}

pub fn recent(path: &Path) -> Vec<Recent> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Put a session at the top of the list, keeping the task it was started
/// with when the new sighting does not know it.
pub fn remember(path: &Path, mut entry: Recent) {
    let mut list = recent(path);
    if let Some(old) = list.iter().find(|r| r.id == entry.id)
        && entry.task.is_empty()
    {
        entry.task = old.task.clone();
    }
    list.retain(|r| r.id != entry.id);
    list.insert(0, entry);
    list.truncate(KEEP);
    save(path, &list);
}

pub fn forget(path: &Path, id: &str) {
    let mut list = recent(path);
    list.retain(|r| r.id != id);
    save(path, &list);
}

fn save(path: &Path, list: &[Recent]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(raw) = serde_json::to_string_pretty(list) {
        let _ = std::fs::write(path, raw);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ids_and_links() {
        assert_eq!(id_of("cse_01AbCdEf").as_deref(), Some("cse_01AbCdEf"));
        assert_eq!(
            id_of("https://claude.ai/code/session_011CUx9?from=cli").as_deref(),
            Some("session_011CUx9")
        );
        assert_eq!(
            id_of("  claude.ai/code/cse_abcdef/  ").as_deref(),
            Some("cse_abcdef")
        );
        assert_eq!(id_of("fix the login page"), None);
        assert_eq!(id_of("cse_"), None);
        assert_eq!(id_of("cse_abcd and more"), None);
        assert_eq!(id_of("3f1c2a9e-1b2c-4d5e-8f90-123456789abc"), None);
    }

    #[test]
    fn finds_a_link_drawn_with_escapes() {
        let out = "\x1b[2mCloud session:\x1b[0m \x1b]8;;https://claude.ai/code/cse_0123abcd\x07https://claude.ai/\x1b[1mcode/cse_0123abcd\x1b]8;;\x07";
        assert_eq!(
            find_link(out),
            Some((
                "cse_0123abcd".to_string(),
                "https://claude.ai/code/cse_0123abcd".to_string()
            ))
        );
        let split = "https://claude.ai/\x1b[38;5;12mcode/session_9xyzw\x1b[0m";
        assert_eq!(
            find_link(split).map(|(id, _)| id).as_deref(),
            Some("session_9xyzw")
        );
        assert_eq!(find_link("visit https://claude.ai/code to see"), None);
    }

    #[test]
    fn remembers_newest_first_and_keeps_the_task() {
        let dir = std::env::temp_dir().join(format!("fleet-cloud-{}", std::process::id()));
        let path = dir.join("cloud.json");
        let _ = std::fs::remove_file(&path);
        let entry = |id: &str, task: &str, at| Recent {
            id: id.into(),
            url: url_of(id),
            task: task.into(),
            cwd: "C:\\code".into(),
            at,
        };
        remember(&path, entry("cse_aaaa1", "fix tests", 1));
        remember(&path, entry("cse_bbbb2", "docs", 2));
        remember(&path, entry("cse_aaaa1", "", 3));
        let list = recent(&path);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "cse_aaaa1");
        assert_eq!(list[0].task, "fix tests");
        assert_eq!(list[0].at, 3);
        forget(&path, "cse_aaaa1");
        assert_eq!(recent(&path).len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
