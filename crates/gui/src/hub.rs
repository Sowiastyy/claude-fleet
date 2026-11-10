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
//!
//! A conversation another live process holds (a background job, another
//! window) cannot be resumed: Claude Code refuses it. Such a session forks
//! instead, `--resume <old> --fork-session --session-id <new>`, and carries on
//! as the copy.
//!
//! A cloud session (see `cloud`) runs on Anthropic's machines; here it is a
//! terminal running `claude --cloud`, which has no chat view. A teleported
//! one is the other way round: a cloud session pulled into a local process
//! with `claude --teleport <id>`, which becomes an ordinary session once
//! Claude Code reports the conversation it made for it.

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
use crate::cloud;
use fleet_core::{accounts::Accounts, claude::claude_binary, history, registry};
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
    /// Runs in the cloud, through `claude --cloud`.
    pub cloud: bool,
    /// The cloud session's id and link, once Claude Code has said them.
    pub cloud_id: String,
    pub cloud_url: String,
    /// What the cloud session was started to do.
    pub cloud_task: String,
    /// Whether its terminal process is running (a chat starts its own with
    /// the next message).
    pub alive: bool,
    /// The account it runs as; empty for whatever `~/.claude` is signed in as.
    pub account: String,
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
    /// Set while the conversation is a fork of this one that Claude Code has
    /// not written yet: the next process starts the fork, not a plain resume.
    fork_from: Option<String>,
    /// The processes this session ran, so its own just-killed one does not
    /// count as someone else holding the conversation.
    last_pids: Vec<u32>,
    /// The last message written to the chat, kept until its turn ends so a
    /// process that refuses to start can be replaced and sent it again.
    pending: Option<String>,
    /// Bumped whenever a process is replaced, so a reader thread of the old
    /// one can tell that what it reads no longer counts.
    generation: u64,
    chat: Option<ChatProc>,
    term: Option<TermProc>,
    pub cloud: bool,
    pub cloud_id: String,
    pub cloud_url: String,
    pub cloud_task: String,
    /// The cloud session a local process is pulling in with `--teleport`,
    /// until the registry tells which conversation it became.
    teleport: Option<String>,
    /// The account every process of this session runs as, in that account's
    /// own config dir (see `Accounts::config_dir_for`), so switching who is
    /// signed in leaves it alone. Empty: `~/.claude` as it is, for `/login`.
    pub account: String,
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
            cloud: self.cloud,
            cloud_id: self.cloud_id.clone(),
            cloud_url: self.cloud_url.clone(),
            cloud_task: self.cloud_task.clone(),
            alive: self.term.is_some() || self.chat.is_some(),
            account: self.account.clone(),
        }
    }

    /// The flags every process of this session starts with, and the note to
    /// show when it had to fork.
    fn claude_args(&mut self) -> (Vec<String>, Option<Value>) {
        let mut args = Vec::new();
        let mut note = None;
        if self.cloud {
            // The cloud keeps the conversation; a new one is started from the
            // task, and only a new one takes the model and mode chosen here.
            if self.cloud_id.is_empty() {
                args.extend(["--cloud".to_string(), self.cloud_task.clone()]);
                self.push_choices(&mut args);
            } else {
                args.extend(["--cloud".to_string(), self.cloud_id.clone()]);
            }
            return (args, None);
        }
        if let Some(id) = self.teleport.clone() {
            args.extend(["--teleport".to_string(), id]);
            self.push_choices(&mut args);
            return (args, None);
        }
        if let Some(src) = self.fork_from.clone().filter(|_| !history::exists(&self.conversation)) {
            args.extend(fork_args(&src, &self.conversation));
        } else if self.started || history::exists(&self.conversation) {
            self.started = true;
            self.fork_from = None;
            if held_elsewhere(&self.conversation, &self.last_pids) {
                note = Some(self.fork());
                let src = self.fork_from.clone().expect("just set");
                args.extend(fork_args(&src, &self.conversation));
            } else {
                args.extend(["--resume".to_string(), self.conversation.clone()]);
            }
        } else {
            args.extend(["--session-id".to_string(), self.conversation.clone()]);
        }
        self.push_choices(&mut args);
        for tool in &self.allowed_tools {
            args.extend(["--allowedTools".to_string(), tool.clone()]);
        }
        (args, note)
    }

    fn push_choices(&self, args: &mut Vec<String>) {
        if !self.model.is_empty() {
            args.extend(["--model".to_string(), self.model.clone()]);
        }
        if !self.permission_mode.is_empty() && self.permission_mode != "default" {
            args.extend(["--permission-mode".to_string(), self.permission_mode.clone()]);
        }
    }

    /// Carry on as a copy of the conversation under a new id. Returns the
    /// note the thread shows for it.
    fn fork(&mut self) -> Value {
        let src = std::mem::replace(&mut self.conversation, uuid::Uuid::new_v4().to_string());
        let short: String = src.chars().take(8).collect();
        self.fork_from = Some(src);
        self.started = true;
        let note = json!({ "type": "fleet_note", "text": format!("Forked from {short}, running elsewhere") });
        self.log.push(note.clone());
        note
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
    #[serde(default)]
    pub images: Vec<Image>,
    /// Run in the cloud: a new cloud session for `prompt`, or, with
    /// `resume`, the existing one that id or claude.ai/code link names.
    #[serde(default)]
    pub cloud: bool,
    /// Pull this cloud session into a local terminal session.
    #[serde(default)]
    pub teleport: Option<String>,
    /// The account to run as: the one signed in when left out, and
    /// `~/.claude` as it is when empty.
    #[serde(default)]
    pub account: Option<String>,
}

pub struct Hub {
    pub sessions: Vec<Session>,
    next: u64,
    app: AppHandle,
    /// Where the cloud sessions seen are kept (see `cloud::remember`).
    pub cloud_path: PathBuf,
    /// Locked after the hub, never before it.
    accounts: Arc<Mutex<Accounts>>,
}

impl Hub {
    pub fn new(app: AppHandle, cloud_path: PathBuf, accounts: Arc<Mutex<Accounts>>) -> Shared {
        Arc::new(Mutex::new(Self {
            sessions: Vec::new(),
            next: 1,
            app,
            cloud_path,
            accounts,
        }))
    }

    /// How many sessions run as `uuid`.
    pub fn on_account(&self, uuid: &str) -> usize {
        self.sessions.iter().filter(|s| s.account == uuid).count()
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

fn fork_args(src: &str, new: &str) -> [String; 5] {
    [
        "--resume".to_string(),
        src.to_string(),
        "--fork-session".to_string(),
        "--session-id".to_string(),
        new.to_string(),
    ]
}

/// Whether a live Claude Code process other than `ours` has the
/// conversation open.
fn held_elsewhere(conversation: &str, ours: &[u32]) -> bool {
    registry::read_all()
        .iter()
        .any(|e| e.session_id == conversation && !ours.contains(&e.pid))
}

/// What Claude Code says when the conversation is someone else's to resume.
fn refused_as_busy(text: &str) -> bool {
    text.contains("running in the background") || text.contains("--fork-session")
}

/// The config dir a process of a session on `account` runs in, if not
/// `~/.claude`.
fn config_dir(accounts: &Arc<Mutex<Accounts>>, account: &str) -> Result<Option<PathBuf>> {
    if account.is_empty() {
        return Ok(None);
    }
    let mut a = accounts.lock().unwrap();
    let email = a
        .list
        .iter()
        .find(|x| x.uuid == account)
        .map(|x| x.email.clone())
        .unwrap_or_else(|| "a forgotten account".to_string());
    a.config_dir_for(account)
        .map(Some)
        .with_context(|| format!("could not start as {email}"))
}

fn emit_to(app: &AppHandle, id: &str, event: &Value) {
    let _ = app.emit("fleet://event", json!({ "id": id, "event": event }));
}

/// An image attached to a message, as the window read it.
#[derive(serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    pub media_type: String,
    /// Base64, no `data:` prefix.
    pub data: String,
}

/// Start a session. Returns its id.
pub fn create(shared: &Shared, mut req: NewSession) -> Result<String> {
    let cwd = PathBuf::from(&req.cwd);
    if !cwd.is_dir() {
        bail!("no such folder: {}", cwd.display());
    }
    let not_cloud = "not a cloud session id or claude.ai/code link";
    let teleport = match req.teleport.as_deref() {
        Some(t) => Some(cloud::id_of(t).context(not_cloud)?),
        None => None,
    };
    // A cloud session, or one pulled out of the cloud, lives in its terminal.
    let (cloud_id, cloud_task) = if req.cloud {
        req.terminal = true;
        match req.resume.take() {
            Some(r) => (cloud::id_of(&r).context(not_cloud)?, String::new()),
            None => {
                let task = req.prompt.take().unwrap_or_default().trim().to_string();
                if task.is_empty() {
                    bail!("describe the task for the cloud session");
                }
                (String::new(), task)
            }
        }
    } else {
        (String::new(), String::new())
    };
    if teleport.is_some() {
        req.terminal = true;
        req.resume = None;
    }
    let id = {
        let mut hub = shared.lock().unwrap();
        let id = format!("s{}", hub.next);
        hub.next += 1;
        let (conversation, started, log) = match &req.resume {
            Some(conv) => (conv.clone(), true, load_transcript(conv)),
            None => (uuid::Uuid::new_v4().to_string(), false, Vec::new()),
        };
        // An attached cloud session takes back the task it was started with.
        let cloud_task = if req.cloud && cloud_task.is_empty() {
            cloud::recent(&hub.cloud_path)
                .into_iter()
                .find(|r| r.id == cloud_id)
                .map(|r| r.task)
                .unwrap_or_default()
        } else {
            cloud_task
        };
        if !cloud_id.is_empty() {
            cloud::remember(
                &hub.cloud_path,
                cloud::Recent {
                    id: cloud_id.clone(),
                    url: cloud::url_of(&cloud_id),
                    task: cloud_task.clone(),
                    cwd: cwd.display().to_string(),
                    at: now_ms(),
                },
            );
        }
        let cloud_url = if cloud_id.is_empty() {
            String::new()
        } else {
            cloud::url_of(&cloud_id)
        };
        let name = hub.free_name(&cwd);
        let account = match req.account.take() {
            Some(a) => a,
            None => hub.accounts.lock().unwrap().current.clone().unwrap_or_default(),
        };
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
            fork_from: None,
            last_pids: Vec::new(),
            pending: None,
            generation: 0,
            chat: None,
            term: None,
            cloud: req.cloud,
            cloud_id,
            cloud_url,
            cloud_task,
            teleport,
            account,
        });
        hub.emit_sessions();
        id
    };
    if req.terminal {
        if let Err(e) = start_term(shared, &id, 30, 100) {
            let _ = close(shared, &id);
            return Err(e);
        }
        if let Some(text) = req.type_in {
            type_when_ready(shared.clone(), id.clone(), text, STARTUP_WAIT_MS);
        }
    } else {
        let prompt = req.prompt.unwrap_or_default();
        if !prompt.trim().is_empty() || !req.images.is_empty() {
            send(shared, &id, &prompt, &req.images)?;
        }
    }
    Ok(id)
}

/// Send a message to the chat, starting its process when there is none.
pub fn send(shared: &Shared, id: &str, text: &str, images: &[Image]) -> Result<()> {
    let text = text.trim();
    if text.is_empty() && images.is_empty() {
        bail!("nothing to send");
    }
    let mut hub = shared.lock().unwrap();
    let app = hub.app.clone();
    let accounts = hub.accounts.clone();
    let s = hub.find_mut(id)?;
    if s.view != View::Chat {
        bail!("the session is showing its terminal");
    }
    if s.chat.is_none() {
        start_chat(shared, &accounts, s, app)?;
    }
    let mut content: Vec<Value> = images
        .iter()
        .map(|i| {
            json!({
                "type": "image",
                "source": { "type": "base64", "media_type": i.media_type, "data": i.data }
            })
        })
        .collect();
    if !text.is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    let line = json!({
        "type": "user",
        "message": { "role": "user", "content": content }
    })
    .to_string();
    let chat = s.chat.as_mut().expect("just started");
    writeln!(chat.stdin, "{line}").context("the chat process is gone")?;
    chat.stdin.flush()?;
    s.pending = Some(line);
    let echo = json!({
        "type": "fleet_user",
        "text": text,
        "ts": now_ms(),
        "images": images
            .iter()
            .map(|i| format!("data:{};base64,{}", i.media_type, i.data))
            .collect::<Vec<_>>(),
    });
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

fn start_chat(shared: &Shared, accounts: &Arc<Mutex<Accounts>>, s: &mut Session, app: AppHandle) -> Result<()> {
    let home = config_dir(accounts, &s.account)?;
    let (args, note) = s.claude_args();
    if let Some(note) = &note {
        emit_to(&app, &s.id, note);
    }
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
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(home) = &home {
        cmd.env("CLAUDE_CONFIG_DIR", home);
    }
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
    s.last_pids = vec![child.id()];
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
        let text = errors.lock().unwrap().trim().to_string();
        // Refused because another process holds the conversation: fork it
        // and hand the copy the message the refusal swallowed.
        if mid_turn
            && s.fork_from.is_none()
            && refused_as_busy(&text)
            && let Some(line) = s.pending.clone()
        {
            let note = s.fork();
            hub.emit_event(&id, &note);
            let accounts = hub.accounts.clone();
            let s = hub.find_mut(&id).expect("still there");
            let retried = start_chat(&shared, &accounts, s, app.clone()).and_then(|()| {
                let chat = s.chat.as_mut().context("no chat")?;
                writeln!(chat.stdin, "{line}")?;
                chat.stdin.flush()?;
                Ok(())
            });
            if retried.is_ok() {
                hub.emit_sessions();
                return;
            }
        }
        let Ok(s) = hub.find_mut(&id) else { return };
        s.status = Status::Idle;
        s.pending = None;
        if mid_turn {
            let event = json!({
                "type": "fleet_error",
                "text": if text.is_empty() { "Claude Code stopped".to_string() } else { last_lines(&text, 6) },
            });
            s.log.push(event.clone());
            hub.emit_event(&id, &event);
        }
        hub.emit_sessions();
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
        Some("system") => matches!(
            event.get("subtype").and_then(Value::as_str),
            Some("init") | Some("compact_boundary")
        ),
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
            s.pending = None;
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
        if view == View::Chat && s.cloud {
            bail!("a cloud session runs in its terminal; open it on claude.ai for the web view");
        }
        if view == View::Chat && s.teleport.is_some() {
            bail!("the teleport has not finished yet; the chat opens once Claude Code has the conversation");
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
    let accounts = hub.accounts.clone();
    let s = hub.find_mut(id)?;
    s.stop_term();
    let home = config_dir(&accounts, &s.account)?;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: rows.max(4),
            cols: cols.max(20),
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("no pseudoterminal")?;
    let (args, note) = s.claude_args();
    if let Some(note) = &note {
        emit_to(&app, id, note);
    }
    let mut cmd = CommandBuilder::new(claude_binary());
    cmd.args(args);
    cmd.cwd(&s.cwd);
    cmd.env("TERM", "xterm-256color");
    if let Some(home) = &home {
        cmd.env("CLAUDE_CONFIG_DIR", home);
    }
    let child = pair.slave.spawn_command(cmd).context("could not start claude")?;
    drop(pair.slave);
    let pid = child.process_id();
    s.last_pids = pid.into_iter().collect();
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
    let note_sent = note.is_some();
    let ordinary = !s.cloud && s.teleport.is_none();
    let mut watch_link = s.cloud && s.cloud_id.is_empty();
    thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        let b64 = base64::engine::general_purpose::STANDARD;
        // The start of what it printed, to tell a refusal from a real exit.
        let mut head: Vec<u8> = Vec::new();
        // The latest stretch of output, for a new cloud session's link.
        let mut tail = String::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if head.len() < 8 * 1024 {
                        head.extend_from_slice(&buf[..n]);
                    }
                    let _ = app.emit(
                        "fleet://term",
                        json!({ "id": id, "data": b64.encode(&buf[..n]) }),
                    );
                    if watch_link {
                        tail.push_str(&String::from_utf8_lossy(&buf[..n]));
                        if let Some((cloud_id, url)) = cloud::find_link(&tail) {
                            watch_link = false;
                            adopt_cloud_id(&shared, &id, generation, cloud_id, url);
                        } else if tail.len() > 16 * 1024 {
                            let mut cut = tail.len() - 4 * 1024;
                            while !tail.is_char_boundary(cut) {
                                cut += 1;
                            }
                            tail.drain(..cut);
                        }
                    }
                }
            }
        }
        let mut hub = shared.lock().unwrap();
        let Ok(s) = hub.find_mut(&id) else { return };
        if s.generation != generation {
            return;
        }
        s.term = None;
        s.status = Status::Idle;
        if ordinary && !note_sent && s.fork_from.is_none() && refused_as_busy(&String::from_utf8_lossy(&head)) {
            let note = s.fork();
            hub.emit_event(&id, &note);
            hub.emit_sessions();
            drop(hub);
            let _ = app.emit("fleet://term-reset", json!({ "id": id }));
            if start_term(&shared, &id, rows, cols).is_ok() {
                return;
            }
            hub = shared.lock().unwrap();
        }
        hub.emit_sessions();
        let _ = app.emit("fleet://term-exit", json!({ "id": id }));
    });
    Ok(())
}

/// A new cloud session's link, read off its terminal: kept on the session
/// and in the list of cloud sessions.
fn adopt_cloud_id(shared: &Shared, id: &str, generation: u64, cloud_id: String, url: String) {
    let mut hub = shared.lock().unwrap();
    let path = hub.cloud_path.clone();
    let Ok(s) = hub.find_mut(id) else { return };
    if s.generation != generation || !s.cloud_id.is_empty() {
        return;
    }
    s.cloud_id = cloud_id.clone();
    s.cloud_url = url.clone();
    let entry = cloud::Recent {
        id: cloud_id,
        url,
        task: s.cloud_task.clone(),
        cwd: s.cwd.display().to_string(),
        at: now_ms(),
    };
    cloud::remember(&path, entry);
    hub.emit_sessions();
}

/// How long a fresh Claude Code takes to draw its prompt: what is typed
/// before that is dropped.
const STARTUP_WAIT_MS: u64 = 3500;

/// Type `text` and Enter into a terminal after `wait_ms`.
fn type_when_ready(shared: Shared, id: String, text: String, wait_ms: u64) {
    thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(wait_ms));
        let _ = term_write(&shared, &id, &text);
        thread::sleep(std::time::Duration::from_millis(300));
        let _ = term_write(&shared, &id, "\r");
    });
}

/// Run a command only the interactive terminal has (`/login`, `/resume`):
/// show the session's terminal, starting it when it is not running, and
/// type the command into it.
pub fn terminal_command(shared: &Shared, id: &str, text: &str, rows: u16, cols: u16) -> Result<()> {
    let (view, running) = {
        let hub = shared.lock().unwrap();
        let s = hub.find(id).context("no such session")?;
        (s.view, s.term.is_some())
    };
    let wait = if view == View::Terminal && running {
        0
    } else {
        if view == View::Terminal {
            start_term(shared, id, rows, cols)?;
        } else {
            set_view(shared, id, View::Terminal, rows, cols)?;
        }
        STARTUP_WAIT_MS
    };
    type_when_ready(shared.clone(), id.to_string(), text.to_string(), wait);
    Ok(())
}

/// Start a terminal session's process again after it exited.
pub fn restart_term(shared: &Shared, id: &str, rows: u16, cols: u16) -> Result<()> {
    {
        let hub = shared.lock().unwrap();
        let s = hub.find(id).context("no such session")?;
        if s.view != View::Terminal {
            bail!("the session is showing its chat");
        }
        if s.term.is_some() {
            return Ok(());
        }
    }
    start_term(shared, id, rows, cols)
}

/// Start over on a new, empty conversation in the same session, as `/clear`
/// does. The old one stays in Claude Code's history.
pub fn clear(shared: &Shared, id: &str) -> Result<()> {
    let mut hub = shared.lock().unwrap();
    let s = hub.find_mut(id)?;
    if s.cloud {
        bail!("a cloud session cannot be cleared from here");
    }
    s.generation += 1;
    s.stop_chat();
    s.conversation = uuid::Uuid::new_v4().to_string();
    s.started = false;
    s.fork_from = None;
    s.teleport = None;
    s.pending = None;
    s.last_pids.clear();
    s.status = Status::Idle;
    s.waiting_for.clear();
    s.log = vec![json!({ "type": "fleet_note", "text": "New conversation" })];
    hub.emit_sessions();
    Ok(())
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
    send(shared, id, &format!("{tool} is allowed now. Carry on."), &[])
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

/// Run the session as another account from now on. Its process is replaced
/// on the same conversation: a chat's with the next message, a terminal's at
/// once. Not in the middle of a turn, which would be lost.
pub fn set_account(shared: &Shared, id: &str, account: &str, rows: u16, cols: u16) -> Result<()> {
    let restart = {
        let mut hub = shared.lock().unwrap();
        let s = hub.find_mut(id)?;
        if s.account == account {
            return Ok(());
        }
        if s.status == Status::Working {
            bail!("{} is working; move it once the turn is over", s.name);
        }
        if s.cloud && s.cloud_id.is_empty() {
            bail!("the cloud session has not started yet");
        }
        s.account = account.to_string();
        s.generation += 1;
        s.stop_chat();
        let restart = s.term.is_some();
        if restart {
            s.stop_term();
        }
        hub.emit_sessions();
        restart
    };
    if restart {
        start_term(shared, id, rows, cols)?;
    }
    Ok(())
}

/// "Move every session over" after a switch: each runs as `account` from
/// now on, its process replaced on the same conversation (see
/// `set_account`). A session in the middle of a turn is left where it is.
/// Returns how many moved.
pub fn move_all(shared: &Shared, account: &str) -> usize {
    let mut moved = 0;
    let terminals: Vec<String> = {
        let mut hub = shared.lock().unwrap();
        let mut terms = Vec::new();
        for s in hub.sessions.iter_mut() {
            // Mid-turn or on a question: killing it would lose that.
            let busy = matches!(s.status, Status::Working | Status::Waiting);
            // A new cloud session whose link never showed would start a
            // second one on the same task.
            let unborn = s.cloud && s.cloud_id.is_empty();
            if s.account == account || busy || unborn {
                continue;
            }
            moved += 1;
            s.account = account.to_string();
            s.generation += 1;
            s.stop_chat();
            if s.term.is_some() {
                s.stop_term();
                terms.push(s.id.clone());
            }
        }
        hub.emit_sessions();
        terms
    };
    for id in terminals {
        let _ = start_term(shared, &id, 30, 100);
    }
    moved
}

/// Keep terminal sessions' status in step with what Claude Code reports.
pub fn sync_registry(shared: &Shared, entries: &[registry::RegistryEntry]) {
    let mut hub = shared.lock().unwrap();
    let mut changed = false;
    let mut seen: Vec<cloud::Recent> = Vec::new();
    for s in hub.sessions.iter_mut() {
        let Some(pid) = s.term.as_ref().and_then(|t| t.pid) else {
            continue;
        };
        let Some(e) = registry::find_by_pid(entries, pid) else {
            continue;
        };
        // A teleported session becomes the local conversation it was
        // pulled into.
        if s.teleport.is_some() && !e.session_id.is_empty() && cloud::id_of(&e.session_id).is_none() {
            s.teleport = None;
            s.conversation = e.session_id.clone();
            s.started = true;
            changed = true;
        }
        // A cloud session's process may name the cloud session before its
        // screen shows the link.
        if s.cloud
            && s.cloud_id.is_empty()
            && let Some(cloud_id) = cloud::id_of(&e.session_id)
        {
            s.cloud_url = cloud::url_of(&cloud_id);
            s.cloud_id = cloud_id.clone();
            seen.push(cloud::Recent {
                id: cloud_id,
                url: s.cloud_url.clone(),
                task: s.cloud_task.clone(),
                cwd: s.cwd.display().to_string(),
                at: now_ms(),
            });
            changed = true;
        }
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
    for entry in seen {
        cloud::remember(&hub.cloud_path, entry);
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
            Some("system") if v.get("subtype").and_then(Value::as_str) == Some("compact_boundary") => {
                out.push(json!({ "type": "fleet_note", "text": "Conversation compacted" }));
            }
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
                let texts: Vec<String> = items
                    .iter()
                    .filter(|i| i.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|i| i.get("text").and_then(Value::as_str).and_then(said))
                    .collect();
                let images: Vec<String> = items.iter().filter_map(image_url).collect();
                if !texts.is_empty() || !images.is_empty() {
                    out.push(json!({ "type": "fleet_user", "text": texts.join("\n\n"), "images": images }));
                }
            }
            _ => {}
        }
    }
    out
}

/// An image block of a message as a data URL.
fn image_url(item: &Value) -> Option<String> {
    if item.get("type").and_then(Value::as_str) != Some("image") {
        return None;
    }
    let src = item.get("source")?;
    if src.get("type").and_then(Value::as_str) != Some("base64") {
        return None;
    }
    let mime = src.get("media_type").and_then(Value::as_str)?;
    let data = src.get("data").and_then(Value::as_str)?;
    Some(format!("data:{mime};base64,{data}"))
}

/// What a person typed, or `None` for what Claude Code wrote in their name:
/// reminders, command output, interruption notes. A slash command comes back
/// as it was typed.
fn said(text: &str) -> Option<String> {
    let t = text.trim();
    if let Some(name) = tag(t, "command-name") {
        let args = tag(t, "command-args").unwrap_or_default();
        let name = if name.starts_with('/') { name } else { format!("/{name}") };
        return Some(if args.is_empty() { name } else { format!("{name} {args}") });
    }
    if t.is_empty() || t.starts_with('<') || t.starts_with("[Request interrupted") {
        return None;
    }
    Some(t.to_string())
}

/// The trimmed text inside `<name>...</name>`, when there is any.
fn tag(text: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    let inner = text[start..end].trim();
    (!inner.is_empty()).then(|| inner.to_string())
}
