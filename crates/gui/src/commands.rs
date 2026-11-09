//! Claude Code's slash commands, for the message box to offer after `/`.
//!
//! The list is Claude Code's own: a `claude -p` speaking stream-json answers
//! an `initialize` control request with every command it can run that way
//! (built-ins, custom commands, skills, plugin and MCP commands), each with
//! its description and argument hint. Asking costs a process start, a couple
//! of seconds, so each folder's answer is kept for a while.
//!
//! Some built-ins only exist in the interactive terminal (`/login`,
//! `/resume`, `/permissions`): `-p` does not list them. They are added from a
//! list here, marked `terminal`, and the window runs them in the session's
//! terminal instead of the chat.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use fleet_core::claude::claude_binary;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    pub aliases: Vec<String>,
    /// Only the interactive terminal runs it.
    pub terminal: bool,
}

/// Interactive-only built-ins worth offering, as Claude Code describes them.
const TERMINAL: &[(&str, &str, &str)] = &[
    ("login", "Sign in to your Anthropic account", ""),
    ("logout", "Sign out from your Anthropic account", ""),
    (
        "resume",
        "Resume a previous conversation",
        "[conversation id or search term]",
    ),
    (
        "rewind",
        "Restore the code and/or conversation to an earlier point",
        "",
    ),
    (
        "permissions",
        "Manage allow and deny tool permission rules",
        "",
    ),
    (
        "status",
        "Show Claude Code status including version, model, account, API connectivity, and tool statuses",
        "",
    ),
    ("hooks", "View hook configurations for tool events", ""),
    ("memory", "Edit CLAUDE.md files and memory settings", ""),
    ("ide", "Manage IDE integrations and show status", ""),
    ("theme", "Change the theme", ""),
    (
        "export",
        "Export the current conversation to a file or clipboard",
        "[filename]",
    ),
    ("add-dir", "Add a new working directory", "<path>"),
    ("plugin", "Manage Claude Code plugins", ""),
    ("skills", "List available skills", ""),
    (
        "tasks",
        "View and manage everything running in the background",
        "",
    ),
    ("rename", "Rename the current conversation", "[name]"),
    (
        "copy",
        "Copy Claude's last response to clipboard (or /copy N for the Nth-latest)",
        "[N]",
    ),
    (
        "btw",
        "Ask a quick side question without interrupting the main conversation",
        "<question>",
    ),
    (
        "branch",
        "Create a branch of the current conversation at this point",
        "",
    ),
    (
        "teleport",
        "Send this session to the cloud, or resume one from claude.ai",
        "",
    ),
    ("session", "Show cloud session URL and QR code", ""),
    (
        "remote-env",
        "Choose the default environment for cloud agents",
        "",
    ),
    (
        "web-setup",
        "Set up cloud sessions with your GitHub account",
        "",
    ),
    (
        "install-github-app",
        "Set up Claude GitHub Actions for a repository",
        "",
    ),
    (
        "privacy-settings",
        "View and update your privacy settings",
        "",
    ),
    ("feedback", "Send feedback to Anthropic or report a bug", ""),
    ("help", "Show help and available commands", ""),
];

/// How long a folder's list is good for.
const FRESH: Duration = Duration::from_secs(10 * 60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Default)]
pub struct Cache {
    lists: Mutex<HashMap<String, (Instant, Vec<SlashCommand>)>>,
}

impl Cache {
    /// The commands for `cwd`: kept ones while fresh, asked for otherwise.
    /// When asking fails the terminal-only ones still come back, so the menu
    /// is never empty.
    pub fn get(&self, cwd: &str) -> Vec<SlashCommand> {
        if let Some((at, list)) = self.lists.lock().unwrap().get(cwd)
            && at.elapsed() < FRESH
        {
            return list.clone();
        }
        match probe(Path::new(cwd)) {
            Ok(found) => {
                let list = with_terminal(found);
                self.lists
                    .lock()
                    .unwrap()
                    .insert(cwd.to_string(), (Instant::now(), list.clone()));
                list
            }
            Err(_) => with_terminal(Vec::new()),
        }
    }
}

fn with_terminal(mut list: Vec<SlashCommand>) -> Vec<SlashCommand> {
    let known = |n: &str, list: &[SlashCommand]| {
        list.iter()
            .any(|c| c.name == n || c.aliases.iter().any(|a| a == n))
    };
    for (name, description, hint) in TERMINAL {
        if !known(name, &list) {
            list.push(SlashCommand {
                name: name.to_string(),
                description: description.to_string(),
                argument_hint: hint.to_string(),
                aliases: Vec::new(),
                terminal: true,
            });
        }
    }
    list
}

/// Ask a short-lived `claude -p` for its commands. Hooks are switched off for
/// it: it is not a session anyone works in, and a SessionStart hook has no
/// business running for it.
fn probe(cwd: &Path) -> Result<Vec<SlashCommand>> {
    if !cwd.is_dir() {
        bail!("no such folder");
    }
    let mut cmd = Command::new(claude_binary());
    cmd.current_dir(cwd)
        .args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
            "--settings",
            r#"{"disableAllHooks":true}"#,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().context("could not start claude")?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let req = json!({
        "type": "control_request",
        "request_id": "fleet-commands",
        "request": { "subtype": "initialize" }
    });
    writeln!(stdin, "{req}")?;
    stdin.flush()?;

    let (tx, rx) = mpsc::channel::<Value>();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v.get("type").and_then(Value::as_str) == Some("control_response")
                && v["response"]["request_id"].as_str() == Some("fleet-commands")
            {
                let _ = tx.send(v);
                return;
            }
        }
    });
    let answer = rx.recv_timeout(PROBE_TIMEOUT);
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let answer = answer.context("claude did not list its commands")?;
    if answer["response"]["subtype"].as_str() != Some("success") {
        bail!("claude refused to list its commands");
    }
    let list = answer["response"]["response"]["commands"]
        .as_array()
        .context("no commands in the answer")?;
    Ok(list.iter().filter_map(parse).collect())
}

fn parse(v: &Value) -> Option<SlashCommand> {
    let name = v.get("name")?.as_str()?.trim_start_matches('/').to_string();
    if name.is_empty() || name.starts_with("__") {
        return None;
    }
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Some(SlashCommand {
        name,
        description: text("description"),
        argument_hint: text("argumentHint"),
        aliases: v
            .get("aliases")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        terminal: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_command_and_skips_internal_ones() {
        let c = parse(&json!({
            "name": "compact",
            "description": "Free up context",
            "argumentHint": "<instructions>",
            "aliases": ["c"]
        }))
        .unwrap();
        assert_eq!(c.name, "compact");
        assert_eq!(c.argument_hint, "<instructions>");
        assert_eq!(c.aliases, vec!["c"]);
        assert!(!c.terminal);
        assert!(parse(&json!({ "name": "__remote-workflow" })).is_none());
        assert!(parse(&json!({ "description": "no name" })).is_none());
    }

    /// Asks the `claude` on this machine: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn lists_this_machines_commands() {
        let found = probe(&std::env::temp_dir()).expect("probe");
        assert!(
            found
                .iter()
                .any(|c| c.name == "compact" && !c.description.is_empty())
        );
        assert!(found.iter().all(|c| !c.terminal));
    }

    #[test]
    fn terminal_commands_do_not_shadow_listed_ones() {
        let listed = vec![SlashCommand {
            name: "usage".into(),
            description: String::new(),
            argument_hint: String::new(),
            aliases: vec!["help".into()],
            terminal: false,
        }];
        let all = with_terminal(listed);
        assert!(all.iter().any(|c| c.name == "login" && c.terminal));
        // `help` is an alias of a listed command, so the chat runs it.
        assert!(!all.iter().any(|c| c.name == "help"));
    }
}
