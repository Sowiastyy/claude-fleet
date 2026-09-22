//! BIG BROTHER: a Claude Code session that oversees the others.
//!
//! A Big Brother is an ordinary session under our PTY, started with a system
//! prompt that makes it an overseer and with a way to reach the fleet: the
//! `fleet` command, which is this very binary run as `claude-fleet bb …`. That
//! command talks to the running fleet over a TCP socket on `127.0.0.1`, and the
//! fleet answers from what it already has — the sessions, their screens, the
//! registry — or acts on them: types into one, clears it, kills it, starts a
//! new one.
//!
//! A Big Brother watches a scope: one group of sessions (`a`-`z`, set with
//! `t` on a card) or all of them. It never sees or touches sessions outside
//! that scope, nor any other Big Brother. What it finds worth telling comes
//! back as a report (`fleet alert`), which the fleet keeps in a list (`A`) and
//! announces on the status line.
//!
//! Every request carries the token its Big Brother was started with, and the
//! token is what decides the scope — so a request is always answered for the
//! session that made it, whatever else it claims.

use std::{
    collections::hash_map::DefaultHasher,
    env, fs,
    hash::{Hash, Hasher},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{config, history};

/// Where the `fleet` command finds the fleet it belongs to.
pub const ADDR_VAR: &str = "FLEET_BB_ADDR";
/// The Big Brother's own token.
pub const TOKEN_VAR: &str = "FLEET_BB_TOKEN";
/// The binary behind `fleet`, for a shell that did not pick the shim up.
pub const EXE_VAR: &str = "FLEET_BB_EXE";

/// How long `fleet wait` blocks when not told otherwise: under the ten
/// minutes a Bash call may take at most.
const WAIT_DEFAULT: u64 = 540;
/// How often `fleet wait` asks whether anything happened.
const WAIT_POLL: Duration = Duration::from_secs(2);
/// How many transcript entries `fleet log` prints by default.
const LOG_DEFAULT: usize = 40;
/// How much of the end of a transcript `fleet log` reads. Transcripts run to
/// megabytes, and only the tail is ever asked about.
const LOG_TAIL_BYTES: u64 = 2 * 1024 * 1024;

/// What a Big Brother watches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    All,
    Group(char),
}

impl Scope {
    pub fn covers(self, group: Option<char>) -> bool {
        match self {
            Scope::All => true,
            Scope::Group(g) => group == Some(g),
        }
    }

    /// `*` or the group's letter, as the restore file and the card write it.
    pub fn name(self) -> String {
        match self {
            Scope::All => "*".to_string(),
            Scope::Group(g) => g.to_string(),
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s == "*" {
            return Some(Scope::All);
        }
        let mut chars = s.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_lowercase() => Some(Scope::Group(c)),
            _ => None,
        }
    }

    pub fn describe(self) -> String {
        match self {
            Scope::All => "every session in the fleet".to_string(),
            Scope::Group(g) => format!("the sessions in group {g}"),
        }
    }

    /// The session label a Big Brother of this scope goes by.
    pub fn label(self) -> String {
        match self {
            Scope::All => "BIG-BROTHER".to_string(),
            Scope::Group(g) => format!("BIG-BROTHER-{g}"),
        }
    }
}

/// Set on the session that is a Big Brother.
#[derive(Clone, Debug)]
pub struct Watch {
    pub scope: Scope,
    pub token: String,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Info,
    Warn,
    Alarm,
}

impl Level {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "info" => Some(Level::Info),
            "warn" | "warning" => Some(Level::Warn),
            "alarm" | "alert" | "critical" => Some(Level::Alarm),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Alarm => "ALARM",
        }
    }
}

/// Something a Big Brother told the user.
#[derive(Clone, Debug)]
pub struct Report {
    pub from: String,
    pub level: Level,
    pub text: String,
    pub at: Instant,
}

/// A change in one watched session, as `fleet wait` hands it over.
#[derive(Clone, Debug)]
pub struct Event {
    pub seq: u64,
    pub group: Option<char>,
    pub text: String,
    pub at: Instant,
}

/// One request from a `fleet` command, waiting for the UI thread to answer.
pub struct Request {
    pub token: String,
    pub cmd: String,
    pub args: Vec<String>,
    reply: Sender<Value>,
}

impl Request {
    pub fn answer(self, v: Value) {
        let _ = self.reply.send(v);
    }
}

pub fn ok(text: impl Into<String>) -> Value {
    json!({ "ok": true, "text": text.into() })
}

pub fn err(text: impl Into<String>) -> Value {
    json!({ "ok": false, "text": text.into() })
}

/// The socket the `fleet` commands reach this fleet on.
///
/// Requests are read on threads of their own and handed to the UI thread,
/// which owns every session; the connection waits for its answer there.
pub struct Server {
    pub addr: String,
    rx: Receiver<Request>,
}

impl Server {
    pub fn start(dirty: Arc<AtomicBool>) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").context("could not open a local socket")?;
        let addr = listener.local_addr()?.to_string();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                let dirty = Arc::clone(&dirty);
                thread::spawn(move || serve_one(stream, tx, dirty));
            }
        });
        Ok(Self { addr, rx })
    }

    pub fn try_recv(&self) -> Option<Request> {
        self.rx.try_recv().ok()
    }
}

fn serve_one(stream: TcpStream, tx: Sender<Request>, dirty: Arc<AtomicBool>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut line = String::new();
    if BufReader::new(stream).read_line(&mut line).is_err() {
        return;
    }
    let answer = match serde_json::from_str::<Value>(&line) {
        Ok(v) => {
            let (reply, back) = mpsc::channel();
            let req = Request {
                token: v["token"].as_str().unwrap_or_default().to_string(),
                cmd: v["cmd"].as_str().unwrap_or_default().to_string(),
                args: v["args"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
                reply,
            };
            if tx.send(req).is_err() {
                err("the fleet is shutting down")
            } else {
                dirty.store(true, Ordering::Relaxed);
                back.recv_timeout(Duration::from_secs(15))
                    .unwrap_or_else(|_| err("the fleet did not answer"))
            }
        }
        Err(e) => err(format!("bad request: {e}")),
    };
    let _ = writeln!(out, "{answer}");
    let _ = out.flush();
    let _ = out.shutdown(Shutdown::Both);
}

/// A token nobody else can guess from outside this machine's processes.
pub fn new_token() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let mut h = DefaultHasher::new();
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut h);
    std::process::id().hash(&mut h);
    N.fetch_add(1, Ordering::Relaxed).hash(&mut h);
    let a = h.finish();
    a.rotate_left(17).hash(&mut h);
    format!("{a:016x}{:016x}", h.finish())
}

/// Write the `fleet` command where a Big Brother's shell finds it: a `sh`
/// script for Git Bash and a `.cmd` for PowerShell and cmd, both running this
/// binary as `bb`.
pub fn write_shim(exe: &Path) -> Result<PathBuf> {
    let dir = env::temp_dir()
        .join("claude-fleet")
        .join(format!("bb-{}", std::process::id()));
    fs::create_dir_all(&dir).with_context(|| format!("cannot use {}", dir.display()))?;
    let sh_path = exe.display().to_string().replace('\\', "/");
    let sh = dir.join("fleet");
    fs::write(&sh, format!("#!/bin/sh\nexec \"{sh_path}\" bb \"$@\"\n"))?;
    fs::write(
        dir.join("fleet.cmd"),
        format!("@\"{}\" bb %*\r\n", exe.display()),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&sh, fs::Permissions::from_mode(0o755))?;
    }
    Ok(dir)
}

/// The variables a Big Brother is started with.
pub fn child_env(addr: &str, token: &str, shim: &Path, exe: &Path) -> Vec<(String, String)> {
    let mut paths = vec![shim.to_path_buf()];
    if let Some(p) = env::var_os("PATH") {
        paths.extend(env::split_paths(&p));
    }
    let path = env::join_paths(paths)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    vec![
        (ADDR_VAR.to_string(), addr.to_string()),
        (TOKEN_VAR.to_string(), token.to_string()),
        (EXE_VAR.to_string(), exe.display().to_string()),
        ("PATH".to_string(), path),
    ]
}

/// The `claude` arguments that make a session a Big Brother.
pub fn claude_args(scope: Scope) -> Vec<String> {
    let cfg = config::bigbrother();
    let mut args = vec![
        "--append-system-prompt".to_string(),
        system_prompt(scope, &cfg.instructions),
    ];
    if !cfg.model.trim().is_empty() {
        args.push("--model".to_string());
        args.push(cfg.model.trim().to_string());
    }
    args.push("--name".to_string());
    args.push(scope.label());
    // Last: the flag takes every argument after it.
    args.push("--allowedTools".to_string());
    for tool in [
        "Bash(fleet:*)",
        "PowerShell(fleet:*)",
        "Read",
        "Grep",
        "Glob",
    ] {
        args.push(tool.to_string());
    }
    args
}

/// The first message a fresh Big Brother gets, so it starts watching without
/// anyone having to tell it to.
pub const KICKOFF: &str =
    "Start watching now: run `fleet list`, then keep looping on `fleet wait`.";

pub fn system_prompt(scope: Scope, extra: &str) -> String {
    let mut p = format!(
        "You are BIG BROTHER, the overseer of the Claude Code sessions running in claude-fleet. \
You watch {scope}. You do not write code yourself: you watch, judge, report to the user and, \
when the user tells you to, act on the sessions.

Your interface to the fleet is the `fleet` command, run through Bash (or PowerShell). If the \
shell does not find it, use \"$FLEET_BB_EXE\" bb <command> instead.
  fleet list                     the sessions you watch: name, group, state, directory
  fleet wait [secs]              block until something changes (a session finishes a turn, stops \
on a question, starts or dies) and print the events; default {wait} s, so give the Bash call a \
timeout of 600000 ms
  fleet peek <name> [lines]      what is on that session's screen right now
  fleet log <name> [n]           its last n transcript entries: prompts, replies, tool calls, errors
  fleet alert <info|warn|alarm> <text>   report to the user; warn and alarm ring a bell and stay \
in the fleet's report list
  fleet send <name> <text>       type a message into the session and press Enter
  fleet key <name> <key>...      press keys: enter esc tab up down left right space backspace \
ctrl-c shift-tab, or any single character (this is how a permission prompt is answered)
  fleet clear <name>             run /clear in the session, wiping its context
  fleet kill <name>              stop the session's process
  fleet spawn <dir> [prompt]     start a new session in <dir>, in your scope, optionally with a \
first prompt
  fleet whoami                   your name and scope

How to work:
1. Start with `fleet list`, then loop on `fleet wait`. For each event decide whether it deserves \
a closer look with `fleet log` or `fleet peek` - a session that just finished a turn or stopped \
on a question usually does.
2. Report with `fleet alert` only what the user should know:
   - alarm: destructive or dangerous actions (rm -rf outside build output, git push --force, \
git reset --hard or checkout over uncommitted work, dropping or rewriting databases, touching \
production, deploys or publishes nobody asked for), leaking secrets or credentials, disabling \
tests, hooks or checks to get a green result, working outside its own project directory.
   - warn: a session stuck - the same error again and again, looping on one fix, waiting on a \
permission prompt or a question for minutes, drifting off the task it was given, claiming \
success while tests fail, burning time on something irrelevant.
   - info: a session finished its task (one line: what it did), or reached a notable milestone.
   One or two sentences per alert: name the session, say what happened, say what you suggest.
3. Do not act on sessions by yourself (send, key, clear, kill, spawn) unless the user told you \
to, in this conversation or in the standing orders below. When in doubt, alert instead.
4. Stay quiet when nothing matters. Do not narrate each wait, do not alert about sessions merely \
working or idle, do not repeat an alert for the same thing.
5. The user may talk to you directly in this terminal. Answer, give status reports when asked \
(\"what is everyone doing?\" - one line per session, from `fleet list` and `fleet log`), and \
carry out their orders with the commands above. Then go back to watching.
6. Write alerts and replies in the language the user speaks to you in.",
        scope = scope.describe(),
        wait = WAIT_DEFAULT,
    );
    if !extra.trim().is_empty() {
        p.push_str("\n\nStanding orders from the user:\n");
        p.push_str(extra.trim());
    }
    p
}

/// The bytes a named key sends, for `fleet key`.
pub fn key_bytes(name: &str) -> Option<Vec<u8>> {
    let b: &[u8] = match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => b"\r",
        "esc" | "escape" => b"\x1b",
        "tab" => b"\t",
        "shift-tab" | "backtab" => b"\x1b[Z",
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        "space" => b" ",
        "backspace" => b"\x7f",
        "ctrl-c" => b"\x03",
        "ctrl-d" => b"\x04",
        _ => {
            let mut chars = name.chars();
            return match (chars.next(), chars.next()) {
                (Some(c), None) => Some(c.to_string().into_bytes()),
                _ => None,
            };
        }
    };
    Some(b.to_vec())
}

// ---------------------------------------------------------------------------
// The `fleet` command: the client side, run inside a Big Brother's shell.
// ---------------------------------------------------------------------------

const HELP: &str = "fleet — BIG BROTHER's remote control for claude-fleet

  fleet list
  fleet wait [secs]
  fleet peek <name> [lines]
  fleet log <name> [n]
  fleet alert <info|warn|alarm> <text>
  fleet send <name> <text>
  fleet key <name> <key>...
  fleet clear <name>
  fleet kill <name>
  fleet spawn <dir> [prompt]
  fleet whoami";

/// `claude-fleet bb <command> …`.
pub fn cli(args: &[String]) -> Result<()> {
    let Some(cmd) = args.first().map(String::as_str) else {
        println!("{HELP}");
        return Ok(());
    };
    match cmd {
        "help" | "--help" | "-h" => {
            println!("{HELP}");
            Ok(())
        }
        "wait" => {
            let secs = args
                .get(1)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(WAIT_DEFAULT);
            wait(Duration::from_secs(secs))
        }
        "log" => {
            let name = args.get(1).context("usage: fleet log <name> [n]")?;
            let n = args
                .get(2)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(LOG_DEFAULT);
            let v = expect_ok(call("resolve", std::slice::from_ref(name))?)?;
            let id = v["session_id"].as_str().unwrap_or_default();
            let path = history::transcript_path(id)
                .with_context(|| format!("{name} has no transcript yet"))?;
            let lines = transcript_tail(&path, n)?;
            if lines.is_empty() {
                println!("{name}: nothing in the transcript yet");
            }
            for l in lines {
                println!("{l}");
            }
            Ok(())
        }
        _ => {
            let v = expect_ok(call(cmd, &args[1..])?)?;
            let text = v["text"].as_str().unwrap_or_default();
            if !text.is_empty() {
                println!("{text}");
            }
            Ok(())
        }
    }
}

fn wait(limit: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        let v = expect_ok(call("events", &[])?)?;
        let text = v["text"].as_str().unwrap_or_default();
        if !text.is_empty() {
            println!("{text}");
            return Ok(());
        }
        if start.elapsed() >= limit {
            println!("no events in {} s - all quiet", limit.as_secs());
            return Ok(());
        }
        thread::sleep(WAIT_POLL);
    }
}

fn expect_ok(v: Value) -> Result<Value> {
    if v["ok"].as_bool() == Some(true) {
        Ok(v)
    } else {
        bail!("{}", v["text"].as_str().unwrap_or("the fleet refused"))
    }
}

fn call(cmd: &str, args: &[String]) -> Result<Value> {
    let addr = env::var(ADDR_VAR)
        .context("not inside a BIG BROTHER session (FLEET_BB_ADDR is not set)")?;
    let token = env::var(TOKEN_VAR).unwrap_or_default();
    let sock = addr
        .parse()
        .with_context(|| format!("bad address in {ADDR_VAR}: {addr}"))?;
    let mut stream = TcpStream::connect_timeout(&sock, Duration::from_secs(5))
        .context("the fleet is not reachable - was it closed or restarted?")?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    let req = json!({ "token": token, "cmd": cmd, "args": args });
    writeln!(stream, "{req}")?;
    stream.flush()?;
    let mut body = String::new();
    stream.read_to_string(&mut body)?;
    serde_json::from_str(body.trim()).context("the fleet sent back something unreadable")
}

/// The last `n` things that happened in a transcript, one line each.
pub fn transcript_tail(path: &Path, n: usize) -> Result<Vec<String>> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let skip_partial = len > LOG_TAIL_BYTES;
    if skip_partial {
        file.seek(SeekFrom::Start(len - LOG_TAIL_BYTES))?;
    }
    let mut reader = BufReader::new(file);
    if skip_partial {
        let mut junk = Vec::new();
        reader.read_until(b'\n', &mut junk)?;
    }
    let mut out: Vec<String> = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        out.extend(entry_lines(&v));
    }
    let start = out.len().saturating_sub(n);
    Ok(out.split_off(start))
}

fn entry_lines(v: &Value) -> Vec<String> {
    if v["isSidechain"].as_bool() == Some(true) {
        return Vec::new();
    }
    let content = &v["message"]["content"];
    match v["type"].as_str() {
        Some("user") => {
            if let Some(text) = content.as_str() {
                let t = history::clean(text);
                return if t.is_empty() {
                    Vec::new()
                } else {
                    vec![format!("USER: {}", clip(&t, 500))]
                };
            }
            content
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| match c["type"].as_str() {
                    Some("text") => {
                        let t = history::clean(c["text"].as_str().unwrap_or_default());
                        (!t.is_empty()).then(|| format!("USER: {}", clip(&t, 500)))
                    }
                    Some("tool_result") => {
                        let body = result_text(&c["content"]);
                        if c["is_error"].as_bool() == Some(true) {
                            Some(format!("  ! error: {}", clip(&body, 300)))
                        } else if body.is_empty() {
                            None
                        } else {
                            Some(format!("  = {}", clip(&body, 160)))
                        }
                    }
                    _ => None,
                })
                .collect()
        }
        Some("assistant") => content
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| match c["type"].as_str() {
                Some("text") => {
                    let t = one_line(c["text"].as_str().unwrap_or_default());
                    (!t.is_empty()).then(|| format!("CLAUDE: {}", clip(&t, 600)))
                }
                Some("tool_use") => Some(format!(
                    "TOOL {}: {}",
                    c["name"].as_str().unwrap_or("?"),
                    clip(&tool_summary(&c["input"]), 300)
                )),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn tool_summary(input: &Value) -> String {
    for key in [
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "description",
        "prompt",
    ] {
        if let Some(s) = input[key].as_str() {
            return one_line(s);
        }
    }
    one_line(&input.to_string())
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => one_line(s),
        Value::Array(items) => one_line(
            &items
                .iter()
                .filter_map(|i| i["text"].as_str())
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => String::new(),
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_group_scope_covers_only_its_group() {
        let a = Scope::Group('a');
        assert!(a.covers(Some('a')));
        assert!(!a.covers(Some('b')));
        assert!(!a.covers(None));
        assert!(Scope::All.covers(None));
        assert!(Scope::All.covers(Some('z')));
    }

    #[test]
    fn scopes_survive_their_written_form() {
        for s in [Scope::All, Scope::Group('q')] {
            assert_eq!(Scope::parse(&s.name()), Some(s));
        }
        assert_eq!(Scope::parse("ab"), None);
        assert_eq!(Scope::parse("A"), None);
    }

    #[test]
    fn tokens_differ() {
        assert_ne!(new_token(), new_token());
        assert_eq!(new_token().len(), 32);
    }

    #[test]
    fn named_keys_and_single_characters_are_keys() {
        assert_eq!(key_bytes("enter").unwrap(), b"\r");
        assert_eq!(key_bytes("ESC").unwrap(), b"\x1b");
        assert_eq!(key_bytes("2").unwrap(), b"2");
        assert!(key_bytes("nonsense").is_none());
    }

    #[test]
    fn a_transcript_reads_as_what_happened() {
        let dir = env::temp_dir().join(format!("fleet-bb-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let body = [
            r#"{"type":"user","message":{"content":"fix the tests"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"On it."},{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":"3 failed"}]}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"subagent noise"}}"#,
        ]
        .join("\n");
        fs::write(&path, body).unwrap();

        let lines = transcript_tail(&path, 10).unwrap();
        assert_eq!(
            lines,
            vec![
                "USER: fix the tests",
                "CLAUDE: On it.",
                "TOOL Bash: cargo test",
                "  ! error: 3 failed",
            ]
        );
        assert_eq!(
            transcript_tail(&path, 1).unwrap(),
            vec!["  ! error: 3 failed"]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_request_goes_round_the_socket_with_its_token() {
        let server = Server::start(Arc::new(AtomicBool::new(false))).unwrap();
        // SAFETY: the variables are this test's own; nothing else reads them.
        unsafe {
            env::set_var(ADDR_VAR, &server.addr);
            env::set_var(TOKEN_VAR, "tok");
        }
        let answerer = thread::spawn(move || {
            loop {
                if let Some(req) = server.try_recv() {
                    let text = format!("{} {} {}", req.token, req.cmd, req.args.join(","));
                    req.answer(ok(text));
                    return;
                }
                thread::sleep(Duration::from_millis(5));
            }
        });
        let v = call("peek", &["web".to_string(), "5".to_string()]).unwrap();
        answerer.join().unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["text"], "tok peek web,5");
    }

    #[test]
    fn standing_orders_reach_the_prompt() {
        let p = system_prompt(Scope::Group('a'), "report in Polish");
        assert!(p.contains("group a"));
        assert!(p.ends_with("report in Polish"));
    }
}
