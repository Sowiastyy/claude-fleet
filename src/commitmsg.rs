//! A commit message written by a model, for the git panel's Generate button.
//!
//! It goes through `claude -p` rather than the API directly: that is already
//! installed and signed in wherever the fleet runs, so there is no key to ask
//! for. Tools are off and nothing is saved, so the child only reads the diff it
//! is handed and answers.

use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use crate::{git, registry, session};

/// What `M` and the model button step through, cheapest first. The model the
/// config names goes in front when it is none of these.
pub const MODELS: &[&str] = &["claude-haiku-4-5", "claude-sonnet-5", "claude-opus-5"];

const SYSTEM: &str = "You write git commit messages. Reply with the message only: \
a subject line under 60 characters in the imperative mood, then, only when the \
change needs explaining, a blank line and a short body wrapped at 72 columns. \
Follow the style of the recent subjects when there are some. No quotes, no code \
fences, no preamble.";

/// Ask `model` for a message describing what a commit in `root` would contain
/// now. Blocks for as long as the model takes.
pub fn generate(root: &Path, model: &str) -> Result<String, String> {
    let context = git::message_context(root);

    let mut cmd = Command::new(session::claude_binary());
    cmd.current_dir(root)
        .args([
            "-p",
            "--model",
            model,
            "--tools",
            "",
            "--no-session-persistence",
        ])
        // Most of the wait was the child starting up rather than the model:
        // connecting every MCP server, loading skills, plugins and hooks, the
        // browser extension. A message needs none of them, and a hook that
        // rewrites replies has no business in one. Sign-in is not a setting,
        // so it still works without them.
        .args([
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--no-chrome",
        ])
        .args(["--setting-sources", ""])
        .args(["--system-prompt", SYSTEM])
        .arg("Write the commit message for the change on stdin.")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("claude could not be run: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(context.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("claude could not be run: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let why = err
            .lines()
            .chain(text.lines())
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("no reason given");
        return Err(format!("claude failed: {why}"));
    }
    let message = clean(&text);
    if message.is_empty() {
        return Err("claude answered with nothing".to_string());
    }
    Ok(message)
}

/// The model after `current` in the list, going round.
pub fn next_model(current: &str, configured: &str) -> String {
    let mut list: Vec<&str> = MODELS.to_vec();
    if !list.contains(&configured) {
        list.insert(0, configured);
    }
    let at = list.iter().position(|m| *m == current);
    let next = at.map_or(0, |i| (i + 1) % list.len());
    list[next].to_string()
}

/// A model's name short enough for a button: `claude-sonnet-5` is `sonnet`.
pub fn short_name(model: &str) -> &str {
    let bare = model.strip_prefix("claude-").unwrap_or(model);
    match bare.split('-').next() {
        Some(family @ ("haiku" | "sonnet" | "opus" | "fable")) => family,
        _ => model,
    }
}

fn model_path() -> Option<std::path::PathBuf> {
    Some(registry::claude_dir()?.join("fleet-commit-model"))
}

/// The model picked in the panel last time, which stands over the config's.
pub fn saved_model() -> Option<String> {
    let raw = std::fs::read_to_string(model_path()?).ok()?;
    let model = raw.trim();
    (!model.is_empty()).then(|| model.to_string())
}

/// Remember the pick. Losing it only means the config's model next start.
pub fn save_model(model: &str) {
    if let Some(p) = model_path() {
        let _ = std::fs::write(p, format!("{model}\n"));
    }
}

/// Strip what a model wraps a message in despite being asked not to.
fn clean(text: &str) -> String {
    let lines: Vec<&str> = text
        .trim()
        .lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .collect();
    lines.join("\n").trim().trim_matches('"').trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_go_round_with_the_configured_one_in_front() {
        assert_eq!(
            next_model("claude-haiku-4-5", "claude-haiku-4-5"),
            "claude-sonnet-5"
        );
        assert_eq!(
            next_model("claude-opus-5", "claude-haiku-4-5"),
            "claude-haiku-4-5"
        );
        assert_eq!(next_model("claude-opus-5", "my-model"), "my-model");
        assert_eq!(next_model("my-model", "my-model"), "claude-haiku-4-5");
        assert_eq!(next_model("gone", "claude-haiku-4-5"), "claude-haiku-4-5");
    }

    #[test]
    fn buttons_get_the_family_name() {
        assert_eq!(short_name("claude-haiku-4-5"), "haiku");
        assert_eq!(short_name("claude-sonnet-5"), "sonnet");
        assert_eq!(short_name("my-model"), "my-model");
    }

    #[test]
    fn fences_and_quotes_around_a_message_are_dropped() {
        assert_eq!(clean("```\nFix the thing\n```\n"), "Fix the thing");
        assert_eq!(clean("\"Fix the thing\""), "Fix the thing");
        assert_eq!(clean("Subject\n\nBody line"), "Subject\n\nBody line");
    }
}
