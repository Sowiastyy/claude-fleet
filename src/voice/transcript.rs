//! One session's transcript, followed from where it stood when voice started
//! watching it.
//!
//! Claude Code appends a line per message block as each one completes, so
//! reading the new bytes every so often is enough to hear a turn as it goes:
//! each new block of assistant text is something to say, and each tool call
//! is remembered, so a permission prompt can say what it is about.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
};

use serde_json::Value;

/// The most read in one poll. A session that wrote more than this since the
/// last look was not being listened to anyway; the rest waits for the next.
const MAX_READ: u64 = 4 * 1024 * 1024;

pub struct Follower {
    path: PathBuf,
    offset: u64,
    /// A line whose end has not been written yet.
    partial: Vec<u8>,
    /// The latest tool call, as "Bash: cargo test".
    pub last_tool: Option<String>,
    /// The latest question Claude asked with options (`AskUserQuestion`).
    pub last_question: Option<String>,
}

impl Follower {
    /// Start at the end of what is there: what was said before listening
    /// began is not news.
    pub fn from_end(path: PathBuf) -> Self {
        let offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self::at(path, offset)
    }

    /// Start at the top, for a transcript that appeared after listening
    /// began: all of it is new.
    pub fn from_start(path: PathBuf) -> Self {
        Self::at(path, 0)
    }

    fn at(path: PathBuf, offset: u64) -> Self {
        Self {
            path,
            offset,
            partial: Vec::new(),
            last_tool: None,
            last_question: None,
        }
    }

    /// The assistant texts completed since the last call, oldest first.
    pub fn poll(&mut self) -> Vec<String> {
        let Ok(mut file) = File::open(&self.path) else {
            return Vec::new();
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            // Rewritten from scratch; nothing to do but start over at its end.
            self.offset = len;
            self.partial.clear();
            return Vec::new();
        }
        if len == self.offset || file.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut chunk = Vec::new();
        let n = file
            .take(MAX_READ)
            .read_to_end(&mut chunk)
            .unwrap_or_default();
        self.offset += n as u64;
        self.partial.extend_from_slice(&chunk[..n]);

        let mut texts = Vec::new();
        while let Some(nl) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=nl).collect();
            let Ok(v) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            let (said, tool, question) = read_entry(&v);
            texts.extend(said);
            if tool.is_some() {
                self.last_tool = tool;
                self.last_question = question;
            }
        }
        texts
    }
}

/// The assistant text in one transcript line, the last tool it calls, and
/// the question that tool asks, if it is one.
fn read_entry(v: &Value) -> (Vec<String>, Option<String>, Option<String>) {
    if v["type"].as_str() != Some("assistant") || v["isSidechain"].as_bool() == Some(true) {
        return (Vec::new(), None, None);
    }
    let mut texts = Vec::new();
    let mut tool = None;
    let mut question = None;
    for c in v["message"]["content"].as_array().into_iter().flatten() {
        match c["type"].as_str() {
            Some("text") => {
                let t = c["text"].as_str().unwrap_or_default().trim();
                if !t.is_empty() {
                    texts.push(t.to_string());
                }
            }
            Some("tool_use") => {
                let name = c["name"].as_str().unwrap_or("?");
                tool = Some(match tool_target(&c["input"]) {
                    Some(t) => format!("{name}: {t}"),
                    None => name.to_string(),
                });
                question = c["input"]["questions"][0]["question"]
                    .as_str()
                    .map(str::to_string);
            }
            _ => {}
        }
    }
    (texts, tool, question)
}

/// The one field of a tool call that says what it touches.
fn tool_target(input: &Value) -> Option<String> {
    ["command", "file_path", "path", "pattern", "url", "description"]
        .iter()
        .find_map(|k| input[*k].as_str())
        .map(|s| {
            let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
            if s.chars().count() > 80 {
                s.chars().take(80).collect::<String>() + "…"
            } else {
                s
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn only_what_is_written_after_the_start_is_heard() {
        let dir = std::env::temp_dir().join(format!("fleet-voice-t-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let old = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Old news."}]}}"#;
        std::fs::write(&path, format!("{old}\n")).unwrap();

        let mut f = Follower::from_end(path.clone());
        assert!(f.poll().is_empty());

        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Sprawdzam."}},{{"type":"tool_use","name":"Bash","input":{{"command":"cargo test"}}}}]}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","isSidechain":true,"message":{{"content":[{{"type":"text","text":"A subagent's aside."}}]}}}}"#
        )
        .unwrap();
        // Half a line: not heard until the rest of it is written.
        write!(file, r#"{{"type":"assistant","message":{{"content":[{{"type":"text","#).unwrap();
        file.flush().unwrap();
        assert_eq!(f.poll(), vec!["Sprawdzam.".to_string()]);
        assert_eq!(f.last_tool.as_deref(), Some("Bash: cargo test"));

        writeln!(file, r#""text":"Gotowe."}}]}}}}"#).unwrap();
        file.flush().unwrap();
        assert_eq!(f.poll(), vec!["Gotowe.".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
