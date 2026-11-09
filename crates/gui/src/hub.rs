//! The sessions the GUI runs, and the processes behind them.
//!
//! A session is one Claude Code conversation in one directory. It shows either
//! as a chat or as Claude Code's own terminal, and the two take turns rather
//! than run side by side: switching kills one process and starts the other on
//! the same conversation id (`--session-id` the first time, `--resume` after),
//! which Claude Code keeps in its transcript.
//!
//! The chat is `claude -p` speaking stream-json both ways: one JSON object per
//! line in each direction, kept alive between turns. Every object it prints is
//! kept in the session's log and sent on to the window as it arrives.

use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use base64::Engine;
use fleet_core::{claude::claude_binary, history, registry};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter};

pub type Shared = Arc<Mutex<Hub>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum View {
    Chat,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Starting,
    Working,
    Idle,
    Waiting,
}

/// What the window lists for one session.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub conversation: String,
    pub view: View,
    pub status: Status,
    pub waiting_for: String,
    pub model: String,
    pub permission_mode: String,
    pub started_at: u64,
}

struct ChatProc {
    child: Child,
    stdin: ChildStdin,
}

struct TermProc {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    pid: Option<u32>,
}

pub struct Session {
    pub id: String,
    pub name: String,
    pub cwd: PathBuf,
    /// The conversation id, chosen here so both views can find it.
    pub conversation: String,
    /// Whether Claude Code has a transcript under that id yet: the first
    /// process gets `--session-id`, every later one `--resume`.
    started: bool,
    pub model: String,
    pub permission_mode: String,
    allowed_tools: Vec<String>,
    pub view: View,
    pub status: Status,
    pub waiting_for: String,
    pub log: Vec<Value>,
    started_at: u64,
    /// Bumped whenever a process is replaced, so a reader thread of the old
    /// one can tell that what it reads no longer counts.
    generation: u64,
    chat: Option<ChatProc>,
    term: Option<TermProc>,
}

impl Session {
    fn view(&self) -> SessionView {
        SessionView {
            id: self.id.clone(),
            name: self.name.clone(),
            cwd: self.cwd.display().to_string(),
            conversation: self.conversation.clone(),
            view: self.view,
            status: self.status,
            waiting_for: self.waiting_for.clone(),
            model: self.model.clone(),
            permission_mode: self.permission_mode.clone(),
            started_at: self.started_at,
        }
    }

    /// The flags every process of this session starts with.
    fn claude_args(&mut self) -> Vec<String> {
        let mut args = Vec::new();
        if self.started || history::exists(&self.conversation) {
            self.started = true;
            args.extend(["--resume".to_string(), self.conversation.clone()]);
        } else {
            args.extend(["--session-id".to_string(), self.conversation.clone()]);
        }
        if !self.model.is_empty() {
            args.extend(["--model".to_string(), self.model.clone()]);
        }
        if !self.permission_mode.is_empty() && self.permission_mode != "default" {
            args.extend(["--permission-mode".to_string(), self.permission_mode.clone()]);
        }
        for tool in &self.allowed_tools {
            args.extend(["--allowedTools".to_string(), tool.clone()]);
        }
        args
    }

    fn stop_chat(&mut self) {
        if let Some(mut c) = self.chat.take() {
            drop(c.stdin);
            let _ = c.child.kill();
            let _ = c.child.wait();
        }
    }

    fn stop_term(&mut self) {
        if let Some(mut t) = self.term.take() {
            let _ = t.child.kill();
        }
    }

    pub fn pids(&self) -> Vec<u32> {
        let mut pids = Vec::new();
        if let Some(c) = &self.chat {
            pids.push(c.child.id());
        }
        if let Some(pid) = self.term.as_ref().and_then(|t| t.pid) {
            pids.push(pid);
        }
        pids
    }
}

/// What a new session starts as.
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NewSession {
    pub cwd: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub resume: Option<String>,
    #[serde(default)]
    pub terminal: bool,
    /// Typed into the terminal once Claude Code is up, e.g. `/login`.
    #[serde(default)]
    pub type_in: Option<String>,
}

pub struct Hub {
    pub sessions: Vec<Session>,
    next: u64,
    app: AppHandle,
}

impl Hub {
    pub fn new(app: AppHandle) -> Shared {
        Arc::new(Mutex::new(Self {
            sessions: Vec::new(),
            next: 1,
            app,
        }))
    }

    pub fn list(&self) -> Vec<SessionView> {
        self.sessions.iter().map(Session::view).collect()
    }

    pub fn log(&self, id: &str) -> Vec<Value> {
        self.find(id).map(|s| s.log.clone()).unwrap_or_default()
    }

    fn find(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    fn find_mut(&mut self, id: &str) -> Result<&mut Session> {
        self.sessions
            .iter_mut()
            .find(|s| s.id == id)
            .context("no such session")
    }

    pub fn our_pids(&self) -> Vec<u32> {
        self.sessions.iter().flat_map(Session::pids).collect()
    }

    fn emit_sessions(&self) {
        let _ = self.app.emit("fleet://sessions", self.list());
    }

    fn emit_event(&self, id: &str, event: &Value) {
        let _ = self
            .app
            .emit("fleet://event", json!({ "id": id, "event": event }));
    }

    fn free_name(&self, cwd: &Path) -> String {
        let base = cwd
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "session".to_string());
        let taken = |n: &str| self.sessions.iter().any(|s| s.name == n);
        if !taken(&base) {
            return base;
        }
        (2..)
            .map(|n| format!("{base}-{n}"))
            .find(|n| !taken(n))
            .expect("an unbounded range always finds a free name")
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Start a session. Returns its id.
pub fn create(shared: &Shared, req: NewSession) -> Result<String> {
    let cwd = PathBuf::from(&req.cwd);
    if !cwd.is_dir() {
        bail!("no such folder: {}", cwd.display());
    }
    let id = {
        let mut hub = shared.lock().unwrap();
        let id = format!("s{}", hub.next);
        hub.next += 1;
        let (conversation, started, log) = match &req.resume {
            Some(conv) => (conv.clone(), true, load_transcript(conv)),
            None => (uuid::Uuid::new_v4().to_string(), false, Vec::new()),
        };
        let name = hub.free_name(&cwd);
        hub.sessions.push(Session {
            id: id.clone(),
            name,
            cwd,
            conversation,
            started,
            model: req.model.clone().unwrap_or_default(),
            permission_mode: req.permission_mode.clone().unwrap_or_default(),
            allowed_tools: Vec::new(),
            view: if req.terminal {
                View::Terminal
            } else {
                View::Chat
            },
            status: if req.terminal {
                Status::Starting
            } else {
                Status::Idle
            },
            waiting_for: String::new(),
            log,
            started_at: now_ms(),
            generation: 0,
            chat: None,
            term: None,
        });
        hub.emit_sessions();
        id
    };
    if req.terminal {
        start_term(shared, &id, 30, 100)?;
        if let Some(text) = req.type_in {
            type_when_ready(shared.clone(), id.clone(), text);
        }
    } else if let Some(prompt) = req.prompt.filter(|p| !p.trim().is_empty()) {
        send(shared, &id, &prompt)?;
    }
    Ok(id)
}

/// Send a message to the chat, starting its process when there is none.
pub fn send(shared: &Shared, id: &str, text: &str) -> Result<()> {
    let mut hub = shared.lock().unwrap();
    let app = hub.app.clone();
    let s = hub.find_mut(id)?;
    if s.view != View::Chat {
        bail!("the session is showing its terminal");
    }
    if s.chat.is_none() {
        start_chat(shared, s, app)?;
    }
    let line = json!({
        "type": "user",
        "message": { "role": "user", "content": [{ "type": "text", "text": text }] }
    });
    let chat = s.chat.as_mut().expect("just started");
    writeln!(chat.stdin, "{line}").context("the chat process is gone")?;
    chat.stdin.flush()?;
    let echo = json!({ "type": "fleet_user", "text": text });
    s.log.push(echo.clone());
    s.status = Status::Working;
    s.waiting_for.clear();
    let id = s.id.clone();
    hub.emit_event(&id, &echo);
    hub.emit_sessions();
    Ok(())
}

/// Stop the turn in progress, the way Esc does.
pub fn interrupt(shared: &Shared, id: &str) -> Result<()> {
    let mut hub = shared.lock().unwrap();
    let s = hub.find_mut(id)?;
    if let Some(chat) = s.chat.as_mut() {
        let req = json!({
            "type": "control_request",
            "request_id": format!("fleet-{}", now_ms()),
            "request": { "subtype": "interrupt" }
        });
        let _ = writeln!(chat.stdin, "{req}");
        let _ = chat.stdin.flush();
    }
    Ok(())
}

fn start_chat(shared: &Shared, s: &mut Session, app: AppHandle) -> Result<()> {
    let mut cmd = Command::new(claude_binary());
    cmd.current_dir(&s.cwd)
        .args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
        ])
        .args(s.claude_args())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window flashing up behind the app for every session.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().context("could not start claude")?;
    let stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let stderr = child.stderr.take().context("no stderr")?;
    s.generation += 1;
    let generation = s.generation;
    let id = s.id.clone();
    s.chat = Some(ChatProc { child, stdin });

    let errors = Arc::new(Mutex::new(String::new()));
    {
        let errors = errors.clone();
        thread::spawn(move || {
            let mut buf = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut buf);
            *errors.lock().unwrap() = buf;
        });
    }
    let shared = shared.clone();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(event) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            on_chat_event(&shared, &id, generation, event);
        }
        // The process is gone. The next message starts another on the same
        // conversation, so this is only worth saying when it died mid-turn.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut hub = shared.lock().unwrap();
        let Ok(s) = hub.find_mut(&id) else { return };
        if s.generation != generation {
            return;
        }
        let mid_turn = s.status == Status::Working;
        if let Some(mut c) = s.chat.take() {
            let _ = c.child.wait();
        }
        s.status = Status::Idle;
        if mid_turn {
            let text = errors.lock().unwrap().trim().to_string();
            let event = json!({
                "type": "fleet_error",
                "text": if text.is_empty() { "Claude Code stopped".to_string() } else { last_lines(&text, 6) },
            });
            s.log.push(event.clone());
            hub.emit_event(&id, &event);
        }
        hub.emit_sessions();
        let _ = &app;
    });
    Ok(())
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Bookkeeping hooks and command lists are Claude Code talking to itself.
fn worth_keeping(event: &Value) -> bool {
    match event.get("type").and_then(Value::as_str) {
        Some("system") => event.get("subtype").and_then(Value::as_str) == Some("init"),
        Some("stream_event") => false,
        _ => true,
    }
}

fn on_chat_event(shared: &Shared, id: &str, generation: u64, event: Value) {
    let mut hub = shared.lock().unwrap();
    let Ok(s) = hub.find_mut(id) else { return };
    if s.generation != generation {
        return;
    }
    match event.get("type").and_then(Value::as_str) {
        Some("system") if event.get("subtype").and_then(Value::as_str) == Some("init") => {
            s.started = true;
            if let Some(model) = event.get("model").and_then(Value::as_str) {
                s.model = model.to_string();
            }
        }
        Some("assistant") | Some("user") => {
            s.started = true;
            s.status = Status::Working;
        }
        Some("result") => {
            let denied = event
                .get("permission_denials")
                .and_then(Value::as_array)
                .is_some_and(|d| !d.is_empty());
            if denied {
                s.status = Status::Waiting;
                s.waiting_for = "permission".to_string();
            } else {
                s.status = Status::Idle;
                s.waiting_for.clear();
            }
        }
        Some("rate_limit_event") => {
            let _ = hub.app.emit("fleet://limits", &event);
            return;
        }
        _ => {}
    }
    if !worth_keeping(&event) {
        return;
    }
    let Ok(s) = hub.find_mut(id) else { return };
    s.log.push(event.clone());
    hub.emit_event(id, &event);
    hub.emit_sessions();
}

/// Show the session as a chat or as its terminal.
pub fn set_view(shared: &Shared, id: &str, view: View, rows: u16, cols: u16) -> Result<()> {
    {
        let mut hub = shared.lock().unwrap();
        let s = hub.find_mut(id)?;
        if s.view == view {
            return Ok(());
        }
        s.view = view;
        match view {
            View::Terminal => s.stop_chat(),
            View::Chat => {
                s.stop_term();
                // Whatever happened in the terminal is in the transcript.
                if history::exists(&s.conversation) {
                    s.started = true;
                    s.log = load_transcript(&s.conversation);
                }
                if s.status == Status::Starting {
                    s.status = Status::Idle;
                }
            }
        }
        hub.emit_sessions();
    }
    if view == View::Terminal {
        start_term(shared, id, rows, cols)?;
    }
    Ok(())
}

fn start_term(shared: &Shared, id: &str, rows: u16, cols: u16) -> Result<()> {
    let mut hub = shared.lock().unwrap();
    let app = hub.app.clone();
    let s = hub.find_mut(id)?;
    s.stop_term();
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: rows.max(4),
            cols: cols.max(20),
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("no pseudoterminal")?;
    let mut cmd = CommandBuilder::new(claude_binary());
    cmd.args(s.claude_args());
    cmd.cwd(&s.cwd);
    cmd.env("TERM", "xterm-256color");
    let child = pair.slave.spawn_command(cmd).context("could not start claude")?;
    drop(pair.slave);
    let pid = child.process_id();
    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    s.generation += 1;
    let generation = s.generation;
    s.status = Status::Starting;
    s.term = Some(TermProc {
        master: pair.master,
        writer,
        child,
        pid,
    });
    let id = id.to_string();
    let shared = shared.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        let b64 = base64::engine::general_purpose::STANDARD;
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = app.emit(
                        "fleet://term",
                        json!({ "id": id, "data": b64.encode(&buf[..n]) }),
                    );
                }
            }
        }
        let mut hub = shared.lock().unwrap();
        let Ok(s) = hub.find_mut(&id) else { return };
        if s.generation == generation {
            s.term = None;
            s.status = Status::Idle;
            hub.emit_sessions();
            let _ = app.emit("fleet://term-exit", json!({ "id": id }));
        }
    });
    Ok(())
}

/// Type `text` and Enter into a terminal once Claude Code has drawn its
/// prompt; a fixed wait, since a fresh process drops what comes too early.
fn type_when_ready(shared: Shared, id: String, text: String) {
    thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(3500));
        let _ = term_write(&shared, &id, &text);
        thread::sleep(std::time::Duration::from_millis(300));
        let _ = term_write(&shared, &id, "\r");
    });
}

pub fn term_write(shared: &Shared, id: &str, data: &str) -> Result<()> {
    let mut hub = shared.lock().unwrap();
    let s = hub.find_mut(id)?;
    let term = s.term.as_mut().context("no terminal")?;
    term.writer.write_all(data.as_bytes())?;
    term.writer.flush()?;
    Ok(())
}

pub fn term_resize(shared: &Shared, id: &str, rows: u16, cols: u16) -> Result<()> {
    let mut hub = shared.lock().unwrap();
    let s = hub.find_mut(id)?;
    if let Some(term) = s.term.as_ref() {
        term.master.resize(PtySize {
            rows: rows.max(4),
            cols: cols.max(20),
            pixel_width: 0,
            pixel_height: 0,
        })?;
    }
    Ok(())
}

/// Let the chat use a tool it was refused, and have it try again.
pub fn allow_tool(shared: &Shared, id: &str, tool: &str) -> Result<()> {
    {
        let mut hub = shared.lock().unwrap();
        let s = hub.find_mut(id)?;
        if !s.allowed_tools.iter().any(|t| t == tool) {
            s.allowed_tools.push(tool.to_string());
        }
        // Allowed tools are a start-up flag: the next message starts a new
        // process with it.
        s.stop_chat();
    }
    send(shared, id, &format!("{tool} is allowed now. Carry on."))
}

/// Change the model or the permission mode. Both are start-up flags, so the
/// chat process is replaced on the next message.
pub fn configure(shared: &Shared, id: &str, model: Option<String>, mode: Option<String>) -> Result<()> {
    let mut hub = shared.lock().unwrap();
    let s = hub.find_mut(id)?;
    if let Some(m) = model {
        s.model = m;
    }
    if let Some(m) = mode {
        s.permission_mode = m;
    }
    s.stop_chat();
    hub.emit_sessions();
    Ok(())
}

pub fn close(shared: &Shared, id: &str) -> Result<()> {
    let mut hub = shared.lock().unwrap();
    if let Some(i) = hub.sessions.iter().position(|s| s.id == id) {
        let mut s = hub.sessions.remove(i);
        s.generation += 1;
        s.stop_chat();
        s.stop_term();
    }
    hub.emit_sessions();
    Ok(())
}

/// After an account switch: every process holds the old account's tokens,
/// so each goes. A chat comes back with the next message; a terminal comes
/// back at once, on the same conversation. Returns how many were running.
pub fn restart_all(shared: &Shared) -> usize {
    let terminals: Vec<String> = {
        let mut hub = shared.lock().unwrap();
        let mut terms = Vec::new();
        for s in hub.sessions.iter_mut() {
            s.generation += 1;
            s.stop_chat();
            if s.term.is_some() {
                s.stop_term();
                terms.push(s.id.clone());
            } else if s.status == Status::Working {
                s.status = Status::Idle;
            }
        }
        hub.emit_sessions();
        terms
    };
    let n = shared.lock().unwrap().sessions.len();
    for id in terminals {
        let _ = start_term(shared, &id, 30, 100);
    }
    n
}

/// Keep terminal sessions' status in step with what Claude Code reports.
pub fn sync_registry(shared: &Shared, entries: &[registry::RegistryEntry]) {
    let mut hub = shared.lock().unwrap();
    let mut changed = false;
    for s in hub.sessions.iter_mut() {
        let Some(pid) = s.term.as_ref().and_then(|t| t.pid) else {
            continue;
        };
        let Some(e) = registry::find_by_pid(entries, pid) else {
            continue;
        };
        let status = match e.status.as_str() {
            "busy" => Status::Working,
            "waiting" => Status::Waiting,
            _ => Status::Idle,
        };
        if status != s.status || e.waiting_for != s.waiting_for {
            s.status = status;
            s.waiting_for = e.waiting_for.clone();
            changed = true;
        }
    }
    if changed {
        hub.emit_sessions();
    }
}

/// A transcript as chat events: what was said, the tool calls and their
/// results. Side conversations of subagents and Claude Code's own notes are
/// left out.
pub fn load_transcript(conversation: &str) -> Vec<Value> {
    let Some(path) = history::transcript_path(conversation) else {
        return Vec::new();
    };
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || v.get("isMeta").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => out.push(json!({ "type": "assistant", "message": v["message"] })),
            Some("user") => {
                let content = &v["message"]["content"];
                if let Some(text) = content.as_str() {
                    if let Some(text) = said(text) {
                        out.push(json!({ "type": "fleet_user", "text": text }));
                    }
                    continue;
                }
                let Some(items) = content.as_array() else {
                    continue;
                };
                let results: Vec<&Value> = items
                    .iter()
                    .filter(|i| i.get("type").and_then(Value::as_str) == Some("tool_result"))
                    .collect();
                if !results.is_empty() {
                    out.push(json!({ "type": "user", "message": { "content": results } }));
                }
                for item in items {
                    if item.get("type").and_then(Value::as_str) == Some("text")
                        && let Some(text) = item.get("text").and_then(Value::as_str).and_then(said)
                    {
                        out.push(json!({ "type": "fleet_user", "text": text }));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// What a person typed, or `None` for what Claude Code wrote in their name:
/// command echoes, reminders, interruption notes.
fn said(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() || t.starts_with('<') || t.starts_with("[Request interrupted") {
        return None;
    }
    Some(t.to_string())
}
