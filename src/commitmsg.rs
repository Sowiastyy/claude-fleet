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

use crate::{config, git, session};

const SYSTEM: &str = "You write git commit messages. Reply with the message only: \
a subject line under 60 characters in the imperative mood, then, only when the \
change needs explaining, a blank line and a short body wrapped at 72 columns. \
Follow the style of the recent subjects when there are some. No quotes, no code \
fences, no preamble.";

/// Ask the configured model for a message describing what a commit in `root`
/// would contain now. Blocks for as long as the model takes.
pub fn generate(root: &Path) -> Result<String, String> {
    let context = git::message_context(root);
    let model = config::commit_model();

    let mut cmd = Command::new(session::claude_binary());
    cmd.current_dir(root)
        .args(["-p", "--model", &model, "--tools", "", "--no-session-persistence"])
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
    fn fences_and_quotes_around_a_message_are_dropped() {
        assert_eq!(clean("```\nFix the thing\n```\n"), "Fix the thing");
        assert_eq!(clean("\"Fix the thing\""), "Fix the thing");
        assert_eq!(clean("Subject\n\nBody line"), "Subject\n\nBody line");
    }
}
