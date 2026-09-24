//! Application state and the actions the key handler can trigger.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime},
};

use anyhow::Result;

use crate::{
    bigbrother::{self, Level, Report, Scope},
    commitmsg, config, git,
    gitview::GitView,
    history,
    ide::Ide,
    registry::{self, RegistryEntry},
    repos,
    session::{PtySession, label_for},
    splash, supervise, update, usage,
};

/// The model a failed push is handed to.
const PUSH_FIX_MODEL: &str = "sonnet";

/// How many past conversations the resume list offers. Enough to hold a few
/// days of work; past that one knows the directory and opens it by name.
const RESUME_LIMIT: usize = 20;

const REGISTRY_REFRESH: Duration = Duration::from_millis(750);

/// How often the git panel reads the selected session's repository again.
/// Sessions commit and edit files on their own, and the panel should catch up
/// while one watches, but a `git status` is still a process.
const GIT_REFRESH: Duration = Duration::from_secs(2);

/// How often the binary this fleet was started from is checked for a rebuild.
/// It is one `stat` and nobody rebuilds twice a second.
const EXE_CHECK: Duration = Duration::from_millis(1000);
/// How often the config file's timestamp is looked at. Every frame meant
/// sixty filesystem calls a second for a file that changes a few times a day;
/// a quarter of a second still reads as instant after a save.
const CONFIG_CHECK: Duration = Duration::from_millis(250);

/// How often GitHub is asked about a newer release, so one shows up within
/// minutes of CI publishing it. An unauthenticated client gets sixty API calls
/// an hour — a `304` for an unchanged ETag counts too — and this spends twelve.
const UPDATE_CHECK: Duration = Duration::from_secs(5 * 60);

/// The shortest gap between two refreshes of the limit cache, used while our own
/// sessions are burning through it.
///
/// Six minutes, because Claude Code answers `/usage` from numbers it already
/// has for about five: asking inside that window spawns a child that reads the
/// dialog and changes nothing. A refresh costs no tokens, but it is still a
/// process.
const USAGE_REFRESH_MIN: Duration = Duration::from_secs(6 * 60);

/// The longest the numbers are left alone when nothing here has run since they
/// were fetched. Usage can still move elsewhere — another machine, claude.ai —
/// so the cache is not left to rot either.
const USAGE_REFRESH_MAX: Duration = Duration::from_secs(30 * 60);

/// How long a finished session's card stays on the list before the fleet drops
/// it by itself, and how long the `u` chord stays armed after landing in a
/// session. Both are config, read fresh so an edit takes effect at once:
/// `config::finished_ttl()` and `config::understand_window()`.
///
/// The chord reads both ways round — `u` first and then the target, or the
/// target first and `u` right after — because both are how one reaches for it.
/// The window is what makes the second order possible without stealing the
/// key from someone who simply started typing.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Keys drive the fleet: move between sessions, spawn, kill.
    Nav,
    /// Keys go to the focused session's PTY. Only the reserved function keys
    /// are intercepted, so nothing a chord would shadow reaches Claude wrong.
    Focus,
    /// The new-session dialog is open.
    NewSession,
    Help,
    /// Confirming a kill of the selected session.
    ConfirmKill,
    /// `u` is armed and waiting to be told which session to understand.
    Understand,
    /// Confirming a restart into a new build, which costs the running sessions.
    ConfirmRestart,
    /// The typed path has no directory behind it; asking whether to create it.
    ConfirmMkdir,
    /// Picking a past conversation to carry on.
    Resume,
    /// Keys move around the git panel; the pane shows what is under its cursor.
    Git,
    /// Picking a branch to switch the repository to.
    Branch,
    /// Typing the commit message at the foot of the git panel.
    Commit,
    /// `t` is armed: the next letter is the selected session's group.
    Tag,
    /// `B` is armed: the next key says what the new Big Brother watches.
    BigBrother,
    /// The reports the Big Brothers filed.
    Reports,
    /// A push failed; what git said, and whether Claude should sort it out.
    PushFailed,
    /// The editor has the pane: a file tree and the files open in it.
    Ide,
}

impl Mode {
    /// The git panel has the keyboard, browsing it or typing into it.
    pub fn on_git(self) -> bool {
        matches!(self, Mode::Git | Mode::Commit)
    }
}

/// Something the git panel runs in the background.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GitJob {
    Commit,
    /// A commit pressed with an empty box: a model writes the message and the
    /// commit goes ahead with it.
    GenerateCommit,
    Push,
    Fetch,
    Pull,
    Generate,
}

impl GitJob {
    pub fn doing(self) -> &'static str {
        match self {
            GitJob::Commit => "committing…",
            GitJob::GenerateCommit => "writing + committing…",
            GitJob::Push => "pushing…",
            GitJob::Fetch => "fetching…",
            GitJob::Pull => "pulling…",
            GitJob::Generate => "writing a message…",
        }
    }
}

/// A part of the git panel the mouse can press, as of the last draw.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GitHit {
    Message,
    Commit,
    Push,
    Pull,
    Generate,
    /// The name of the model Generate uses; a click moves to the next one.
    Model,
    /// The failed-push dialog's buttons.
    FixPush,
    RetryPush,
    CloseDialog,
}

/// What a background git job came back with.
enum JobDone {
    /// The new commit's hash, and the message a model wrote for it when the
    /// box was empty.
    Committed(String, Option<String>),
    Pushed,
    Fetched,
    Pulled,
    Generated(String),
    PushFailed(PathBuf, git::PushFailure),
    Failed(GitJob, String),
}

/// A push that failed, held open in a dialog until dealt with.
pub struct PushFailed {
    pub root: PathBuf,
    pub failure: git::PushFailure,
    /// How far the dialog's text is scrolled down.
    pub scroll: usize,
    /// Where the keyboard was when the dialog came up.
    back: Mode,
}

/// The list of past conversations, and where in it the cursor is.
pub struct ResumePicker {
    pub items: Vec<history::Conversation>,
    pub cursor: usize,
}

impl ResumePicker {
    fn new() -> Self {
        Self {
            items: history::recent(RESUME_LIMIT),
            cursor: 0,
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as isize;
        self.cursor = (self.cursor as isize + delta).rem_euclid(len) as usize;
    }

    pub fn selected(&self) -> Option<&history::Conversation> {
        self.items.get(self.cursor)
    }
}

/// The branches of one repository, narrowed down by what has been typed.
pub struct BranchPicker {
    pub root: PathBuf,
    pub items: Vec<git::Branch>,
    pub filter: String,
    /// Index into `rows()`.
    pub cursor: usize,
    /// Where the keyboard goes back to once the picker closes.
    back: Mode,
}

/// One line of the branch list.
pub enum BranchRow<'a> {
    Branch(&'a git::Branch),
    /// The typed name matches no branch exactly: offer to start it.
    Create(&'a str),
}

impl BranchPicker {
    pub fn rows(&self) -> Vec<BranchRow<'_>> {
        let needle = self.filter.trim().to_lowercase();
        let mut rows: Vec<BranchRow> = self
            .items
            .iter()
            .filter(|b| b.name.to_lowercase().contains(&needle))
            .map(BranchRow::Branch)
            .collect();
        let name = self.filter.trim();
        if !name.is_empty() && !self.items.iter().any(|b| b.local_name() == name) {
            rows.push(BranchRow::Create(name));
        }
        rows
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.rows().len() as isize;
        if len == 0 {
            return;
        }
        self.cursor = (self.cursor as isize + delta).rem_euclid(len) as usize;
    }

    pub fn push(&mut self, c: char) {
        self.filter.push(c);
        self.cursor = 0;
    }

    pub fn pop(&mut self) {
        self.filter.pop();
        self.cursor = 0;
    }
}

/// Where a session from the new-session form runs. The two remote kinds are
/// Claude Code's own: fleet only passes the flag and the child does the rest,
/// sign-in and the list of cloud sessions included.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpawnKind {
    /// An ordinary `claude` on this machine.
    Local,
    /// `claude --cloud`: a new session on Claude Code on the web, for the
    /// repository the directory belongs to.
    Remote,
    /// `claude --teleport`: Claude Code's picker over every remote session on
    /// the account; the one picked is pulled down into this directory.
    Teleport,
}

impl SpawnKind {
    pub const ALL: [SpawnKind; 3] = [SpawnKind::Local, SpawnKind::Remote, SpawnKind::Teleport];

    pub fn next(self) -> Self {
        match self {
            SpawnKind::Local => SpawnKind::Remote,
            SpawnKind::Remote => SpawnKind::Teleport,
            SpawnKind::Teleport => SpawnKind::Local,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            SpawnKind::Local => "local",
            SpawnKind::Remote => "remote",
            SpawnKind::Teleport => "teleport",
        }
    }

    /// The arguments `claude` gets for this kind.
    pub fn args(self) -> Vec<String> {
        match self {
            SpawnKind::Local => Vec::new(),
            SpawnKind::Remote => vec!["--cloud".to_string()],
            SpawnKind::Teleport => vec!["--teleport".to_string()],
        }
    }
}

pub struct NewSessionForm {
    pub input: String,
    /// Local, a new remote session, or a remote one teleported here.
    pub kind: SpawnKind,
    /// In remote mode typing filters the repositories rather than editing the
    /// path, and the arrows move over them.
    pub repo_filter: String,
    pub repo_cursor: usize,
    /// Subdirectories under the typed path, for browsing with the arrows.
    /// Refreshed on every edit: a path that ends at a directory lists its
    /// children, an unfinished last component filters its parent's.
    pub subdirs: Vec<PathBuf>,
    pub recent: Vec<PathBuf>,
    /// `0` is the free-text field; `1..=subdirs.len()` selects a subdirectory;
    /// past that selects `recent[n - subdirs.len() - 1]`.
    pub cursor: usize,
}

/// How many subdirectories the form lists before cutting off.
const SUBDIR_LIMIT: usize = 10;

impl NewSessionForm {
    fn new(default_cwd: &std::path::Path) -> Self {
        let mut form = Self {
            input: default_cwd.display().to_string(),
            kind: SpawnKind::Local,
            repo_filter: String::new(),
            repo_cursor: 0,
            subdirs: Vec::new(),
            recent: registry::recent_cwds(12),
            cursor: 0,
        };
        form.refresh_subdirs();
        form
    }

    pub fn selected_path(&self) -> PathBuf {
        if self.cursor == 0 {
            PathBuf::from(self.input.trim())
        } else if let Some(p) = self.subdirs.get(self.cursor - 1) {
            p.clone()
        } else {
            self.recent[self.cursor - 1 - self.subdirs.len()].clone()
        }
    }

    /// The subdirectory under the cursor, if the cursor is on one.
    pub fn selected_subdir(&self) -> Option<&PathBuf> {
        (self.cursor > 0)
            .then(|| self.subdirs.get(self.cursor - 1))
            .flatten()
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let max = (self.subdirs.len() + self.recent.len()) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
    }

    pub fn push(&mut self, c: char) {
        self.input.push(c);
        self.refresh_subdirs();
    }

    pub fn push_str(&mut self, s: &str) {
        self.input.push_str(s);
        self.refresh_subdirs();
    }

    pub fn pop(&mut self) {
        self.input.pop();
        self.refresh_subdirs();
    }

    /// Make the directory under the cursor (or the only match of what was
    /// typed) the input, ready to browse one level deeper.
    pub fn descend(&mut self) {
        let target = match self.selected_subdir() {
            Some(p) => p.clone(),
            None if self.cursor == 0 && self.subdirs.len() == 1 => self.subdirs[0].clone(),
            None => return,
        };
        self.input = format!("{}{}", target.display(), std::path::MAIN_SEPARATOR);
        self.cursor = 0;
        self.refresh_subdirs();
    }

    /// Step the input up to the parent of the directory it names.
    pub fn ascend(&mut self) {
        let (dir, _) = self.split_input();
        let Some(parent) = dir.parent() else { return };
        if parent.as_os_str().is_empty() {
            return;
        }
        self.input = format!("{}{}", parent.display(), std::path::MAIN_SEPARATOR);
        self.cursor = 0;
        self.refresh_subdirs();
    }

    /// The directory to list and the prefix its children must start with.
    fn split_input(&self) -> (PathBuf, String) {
        let raw = self.input.trim();
        let path = PathBuf::from(raw);
        let ends_in_sep = raw.ends_with(['/', '\\']);
        if ends_in_sep || path.is_dir() {
            return (path, String::new());
        }
        let prefix = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dir = path.parent().map(PathBuf::from).unwrap_or(path);
        (dir, prefix)
    }

    fn refresh_subdirs(&mut self) {
        let (dir, prefix) = self.split_input();
        let prefix = prefix.to_lowercase();
        let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                    .map(|e| e.path())
                    .filter(|p| {
                        let name = p
                            .file_name()
                            .map(|s| s.to_string_lossy().to_lowercase())
                            .unwrap_or_default();
                        !name.starts_with('.') && name.starts_with(&prefix)
                    })
                    .collect()
            })
            .unwrap_or_default();
        found.sort_by_key(|p| {
            p.file_name()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        });
        found.truncate(SUBDIR_LIMIT);
        self.subdirs = found;
        self.cursor = self.cursor.min(self.subdirs.len() + self.recent.len());
    }
}

pub struct App {
    pub sessions: Vec<PtySession>,
    pub selected: usize,
    pub mode: Mode,
    pub form: Option<NewSessionForm>,
    /// Path awaiting a yes/no on creating its directory.
    pub pending_mkdir: Option<PathBuf>,
    /// The resume list, while it is open.
    pub resume: Option<ResumePicker>,
    /// The branch list, while it is open.
    pub branches: Option<BranchPicker>,
    pub registry: Vec<RegistryEntry>,
    pub status: Option<(String, Instant)>,
    /// Shared with reader threads; set when any pane produced output.
    pub dirty: Arc<AtomicBool>,
    pub should_quit: bool,
    pub launch_cwd: PathBuf,
    last_registry_scan: Instant,
    last_config_check: Instant,
    /// The last frame of the turning name on an empty pane.
    last_splash: Instant,
    /// Scans made by the registry thread. Listing the pipe namespace and
    /// parsing every descriptor takes milliseconds, which the UI thread spent
    /// stalled on input every refresh while it did the scan itself.
    registry_rx: mpsc::Receiver<Vec<RegistryEntry>>,
    /// Pane geometry from the last render, used when spawning.
    pub pane_rows: u16,
    pub pane_cols: u16,
    /// Top-left of the pane's inner area on screen, for turning a mouse
    /// position into the pane-relative one a child expects.
    pub pane_x: u16,
    pub pane_y: u16,
    /// A paste handed over in pieces: its opening marker is out, its closing
    /// one is not. Until it is, every piece of input belongs to that paste.
    pub paste_open: bool,
    /// The session just landed in, and when. While this is fresh a lone `u`
    /// is the understand chord instead of a character for the child.
    understand_window: Option<(usize, Instant)>,
    /// The next session spawned carries the understand prompt. It has to be a
    /// flag rather than an argument because a spawn can cross the new-session
    /// form and the "create the directory?" question before it happens.
    spawn_understand: bool,
    /// Quitting to be started again, rather than quitting.
    pub restart_requested: bool,
    /// A build has replaced the binary this fleet was started from.
    pub update_ready: bool,
    /// A release on GitHub newer than this binary, once a check has found one.
    pub release: Option<update::Release>,
    /// Set while a check or a download is running, so only one runs at a time.
    update_busy: Arc<AtomicBool>,
    /// What the update thread has to say, read back in `tick`.
    update_tx: mpsc::Sender<UpdateEvent>,
    update_rx: mpsc::Receiver<UpdateEvent>,
    /// When GitHub was last asked; `None` makes the first tick ask.
    last_update_check: Option<Instant>,
    /// What that binary looked like at startup, to compare against.
    origin_stamp: Option<SystemTime>,
    last_exe_check: Instant,
    /// The account's rate-limit windows, as Claude Code last cached them.
    pub usage: usage::Watch,
    /// The pid of the hidden session refreshing those numbers, or zero when no
    /// refresh is in flight. It is what keeps that child off the session list.
    usage_refresh: Arc<AtomicU32>,
    /// Set while the refresh thread is alive, so only one runs at a time.
    usage_refreshing: Arc<AtomicBool>,
    /// When the last refresh was started, successful or not.
    last_usage_refresh: Option<Instant>,
    /// Whether one of our own sessions has been busy since the numbers were
    /// last fetched. Nothing of ours running means nothing of ours spending,
    /// which is the difference between asking every six minutes and every half
    /// hour.
    spent_since_refresh: bool,
    /// Whether the git panel is on screen. Toggled with `G` or Alt+Shift+G,
    /// and off at start: most of the time the pane wants the room.
    pub show_git: bool,
    /// The column border being dragged with the mouse, if any.
    pub drag: Option<Drag>,
    /// Text being marked in the pane with the mouse. Fleet captures the mouse,
    /// so the terminal around it can no longer select anything itself.
    pub selection: Option<Selection>,
    /// The terminal's size as of the last layout, which a drag is clamped to.
    pub term: ratatui::layout::Rect,
    /// The last git read, and the directory it was made in. The panel shows it
    /// only while that is still the selected session's directory.
    pub git: Option<(PathBuf, git::State)>,
    /// Set while a git read is running, so only one runs at a time.
    git_busy: Arc<AtomicBool>,
    git_tx: mpsc::Sender<(PathBuf, git::State)>,
    git_rx: mpsc::Receiver<(PathBuf, git::State)>,
    /// When the last read was started, and for which directory.
    last_git_read: Option<(PathBuf, Instant)>,
    /// The cursor, opened commits and loaded diffs of the git panel.
    pub git_view: GitView,
    /// Whether the terminal is wide enough for the panel, as of the last
    /// layout.
    pub git_fits: bool,
    /// The message in the box at the foot of the git panel.
    pub commit_msg: String,
    /// Where typing goes in `commit_msg`, as a byte offset.
    pub commit_cursor: usize,
    /// The model Generate asks, picked with `M` or the model button.
    pub commit_model: String,
    /// The commit, push or message being worked on right now. One at a time:
    /// a push racing the commit it is meant to carry would push without it.
    pub git_job: Option<GitJob>,
    job_tx: mpsc::Sender<JobDone>,
    job_rx: mpsc::Receiver<JobDone>,
    /// The repositories a remote session can start for, once listed. Kept for
    /// the whole run: the list costs a few API calls.
    pub repos: Option<repos::Listing>,
    repos_rx: Option<mpsc::Receiver<repos::Listing>>,
    /// A checkout being prepared for a remote session: the repository and
    /// where the clone or pull reports back.
    pub cloning: Option<String>,
    clone_rx: Option<mpsc::Receiver<Result<PathBuf, String>>>,
    /// Where the panel drew what a click can press, for the mouse handler.
    pub git_hits: Vec<(ratatui::layout::Rect, GitHit)>,
    /// The socket Big Brothers reach the fleet on, opened with the first one.
    bb_server: Option<bigbrother::Server>,
    /// Where the `fleet` command they run is written.
    bb_shim: Option<PathBuf>,
    /// Changes in the sessions, for `fleet wait`; numbered by `bb_seq`.
    bb_events: Vec<bigbrother::Event>,
    bb_seq: u64,
    /// The last event each Big Brother (by uid) has collected.
    bb_cursor: HashMap<u64, u64>,
    /// Each session's state as of the last look, by uid, to spot changes.
    last_state: HashMap<u64, String>,
    /// What the Big Brothers reported, oldest first.
    pub reports: Vec<Report>,
    /// Reports filed since the list was last opened.
    pub unread_reports: usize,
    /// The worst level among those.
    pub unread_level: Option<Level>,
    /// How far the report list is scrolled from its newest entry.
    pub reports_scroll: usize,
    /// The last push that failed, while its dialog is up.
    pub push_failed: Option<PushFailed>,
    /// The file tree and the files open in the editor. Kept while the pane
    /// shows a session, so nothing typed there is lost by looking away.
    pub ide: Ide,
    /// When quitting was refused over unsaved files; a second try soon
    /// after goes ahead.
    unsaved_warned: Option<Instant>,
}

/// How many session events the fleet keeps for Big Brothers to collect.
const EVENT_LOG: usize = 500;
/// How many reports the list keeps.
const REPORT_LOG: usize = 200;

/// A border between columns that the mouse can move.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Drag {
    /// Between the sidebar and the pane.
    Sidebar,
    /// Between the pane and the git panel.
    Git,
}

/// A run of pane cells marked with the mouse, pane-relative `(row, col)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    /// The session it was made on; another one's screen has other text.
    pub session: usize,
    /// Where the press was.
    pub anchor: (u16, u16),
    /// Where the pointer is now, or was when the button came up.
    pub head: (u16, u16),
    /// The button is still down.
    pub dragging: bool,
}

impl Selection {
    /// Both ends in reading order.
    pub fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Whether a cell falls inside, the way a terminal marks lines: the first
    /// row from the start column on, the last up to the end one, all between.
    pub fn contains(&self, row: u16, col: u16) -> bool {
        let (start, end) = self.ordered();
        (row, col) >= start && (row, col) <= end
    }
}

/// What the update thread reports back.
enum UpdateEvent {
    Found(update::Release),
    /// A check someone asked for found nothing newer. The periodic ones say
    /// nothing in that case.
    UpToDate,
    Installed(String),
    Failed(String),
}

impl App {
    pub fn new(launch_cwd: PathBuf) -> Self {
        if let Some(origin) = supervise::origin() {
            update::sweep(&origin);
        }
        let (update_tx, update_rx) = mpsc::channel();
        let (git_tx, git_rx) = mpsc::channel();
        let (job_tx, job_rx) = mpsc::channel();
        let ide = Ide::new(launch_cwd.clone());
        Self {
            sessions: Vec::new(),
            selected: 0,
            mode: Mode::Nav,
            form: None,
            pending_mkdir: None,
            resume: None,
            branches: None,
            registry: registry::read_all(),
            status: None,
            dirty: Arc::new(AtomicBool::new(true)),
            should_quit: false,
            launch_cwd,
            last_registry_scan: Instant::now(),
            last_config_check: Instant::now(),
            last_splash: Instant::now(),
            registry_rx: spawn_registry_scanner(),
            pane_rows: 24,
            pane_cols: 80,
            pane_x: 0,
            pane_y: 0,
            paste_open: false,
            understand_window: None,
            spawn_understand: false,
            restart_requested: false,
            update_ready: false,
            release: None,
            update_busy: Arc::new(AtomicBool::new(false)),
            update_tx,
            update_rx,
            last_update_check: None,
            usage: usage::Watch::new(),
            usage_refresh: Arc::new(AtomicU32::new(0)),
            usage_refreshing: Arc::new(AtomicBool::new(false)),
            last_usage_refresh: None,
            spent_since_refresh: false,
            show_git: false,
            drag: None,
            selection: None,
            term: ratatui::layout::Rect::default(),
            git: None,
            git_busy: Arc::new(AtomicBool::new(false)),
            git_tx,
            git_rx,
            last_git_read: None,
            git_view: GitView::new(),
            git_fits: true,
            commit_msg: String::new(),
            commit_cursor: 0,
            commit_model: commitmsg::saved_model().unwrap_or_else(config::commit_model),
            git_job: None,
            job_tx,
            job_rx,
            repos: None,
            repos_rx: None,
            cloning: None,
            clone_rx: None,
            git_hits: Vec::new(),
            bb_server: None,
            bb_shim: None,
            bb_events: Vec::new(),
            bb_seq: 0,
            bb_cursor: HashMap::new(),
            last_state: HashMap::new(),
            reports: Vec::new(),
            unread_reports: 0,
            unread_level: None,
            reports_scroll: 0,
            push_failed: None,
            ide,
            unsaved_warned: None,
            origin_stamp: supervise::origin_stamp(),
            last_exe_check: Instant::now(),
        }
    }

    pub fn selected_session(&self) -> Option<&PtySession> {
        self.sessions.get(self.selected)
    }

    pub fn selected_session_mut(&mut self) -> Option<&mut PtySession> {
        self.sessions.get_mut(self.selected)
    }

    pub fn notify(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Foreign sessions: everything in the registry we did not spawn.
    pub fn foreign(&self) -> Vec<&RegistryEntry> {
        let own: Vec<u32> = self.sessions.iter().filter_map(|s| s.child_pid).collect();
        // The session refreshing the limits registers itself like any other, and
        // lives for about two seconds. Showing it would be the panel reporting
        // its own bookkeeping as somebody's work.
        let hidden = self.usage_refresh.load(Ordering::Relaxed);
        self.registry
            .iter()
            .filter(|e| !own.contains(&e.pid) && e.pid != hidden)
            .collect()
    }

    /// Whether any session we own is reported as working right now.
    fn any_own_busy(&self) -> bool {
        self.sessions
            .iter()
            .filter_map(|s| s.child_pid)
            .filter_map(|pid| registry::find_by_pid(&self.registry, pid))
            .any(|e| e.status == "busy")
    }

    /// True while a hidden session is fetching new limit numbers.
    pub fn usage_refreshing(&self) -> bool {
        self.usage_refreshing.load(Ordering::Relaxed)
    }

    /// Start a refresh unless one is already running.
    ///
    /// It runs on a thread of its own: the child takes a couple of seconds to
    /// start, answer and die, and the panel has frames to draw in the meantime.
    pub fn refresh_usage(&mut self) {
        if self.usage_refreshing.swap(true, Ordering::Relaxed) {
            return;
        }
        self.last_usage_refresh = Some(Instant::now());
        let cwd = self
            .selected_session()
            .map(|s| s.cwd.clone())
            .unwrap_or_else(|| self.launch_cwd.clone());
        let pid = Arc::clone(&self.usage_refresh);
        let busy = Arc::clone(&self.usage_refreshing);
        let dirty = Arc::clone(&self.dirty);
        std::thread::spawn(move || {
            // A refresh that changes nothing is an ordinary outcome and says
            // nothing worth interrupting anyone for: the next attempt is five
            // minutes away regardless.
            let _ = usage::refresh_via_claude(&cwd, &pid);
            busy.store(false, Ordering::Relaxed);
            dirty.store(true, Ordering::Relaxed);
        });
    }

    /// Whether the numbers are old enough to be worth a child process.
    ///
    /// Two clocks: a short one while our own sessions are spending the limits,
    /// and a long one when nothing here has run since the last fetch.
    fn usage_refresh_due(&self) -> bool {
        if self.usage_refreshing() {
            return false;
        }
        if let Some(at) = self.last_usage_refresh
            && at.elapsed() < USAGE_REFRESH_MIN
        {
            return false;
        }
        let Some(u) = self.usage.current.as_ref() else {
            // No cache at all: nothing to go stale, and one fetch gives the
            // footer something to draw.
            return true;
        };
        let age = u.fetched_ago;
        if self.spent_since_refresh {
            age >= USAGE_REFRESH_MIN
        } else {
            age >= USAGE_REFRESH_MAX
        }
    }

    /// The registry entry describing one of our own sessions, if it has
    /// finished registering itself yet.
    pub fn entry_for(&self, idx: usize) -> Option<&RegistryEntry> {
        let pid = self.sessions.get(idx)?.child_pid?;
        registry::find_by_pid(&self.registry, pid)
    }

    pub fn spawn_session(&mut self, cwd: PathBuf) -> Result<()> {
        self.spawn_session_with(cwd, &[])
    }

    /// Spawn with arguments for the child: `--resume <id>`, or the remote
    /// flags of `SpawnKind`. A session picked up where it was left is an
    /// ordinary session in every other respect, down to the understand prompt.
    pub fn spawn_session_with(&mut self, cwd: PathBuf, args: &[String]) -> Result<()> {
        if !cwd.is_dir() {
            self.notify(format!("no such directory: {}", cwd.display()));
            return Ok(());
        }
        let idx = self.spawn_raw(cwd, args, &[], None)?;
        let label = self.sessions[idx].label.clone();
        self.selected = idx;
        self.mode = Mode::Focus;

        if std::mem::take(&mut self.spawn_understand) {
            // The child has no prompt box yet; the session holds the text and
            // types it in once it has one.
            let idx = self.selected;
            let prompt = config::understand_prompt();
            self.sessions[idx].queue_prompt(&prompt);
            self.understand_window = None;
            self.notify(format!("{label}: \"{prompt}\" will go into the prompt"));
        } else {
            self.open_understand_window(self.selected);
            self.notify(format!("started {label}"));
        }
        Ok(())
    }

    /// Start a session and put its card on the list, leaving the selection
    /// and the keyboard where they are. Returns its index.
    fn spawn_raw(
        &mut self,
        cwd: PathBuf,
        args: &[String],
        env: &[(String, String)],
        base: Option<String>,
    ) -> Result<usize> {
        let label = self.free_label(&cwd, base);
        let session = PtySession::spawn_with_env(
            label,
            cwd,
            self.pane_rows.max(4),
            self.pane_cols.max(20),
            Arc::new(AtomicBool::new(true)),
            args,
            env,
        )?;
        self.sessions.push(session);
        Ok(self.sessions.len() - 1)
    }

    /// `base`, or `base-2` and on when it is taken; with no base, a name
    /// made from the directory.
    fn free_label(&self, cwd: &std::path::Path, base: Option<String>) -> String {
        let taken: Vec<String> = self.sessions.iter().map(|s| s.label.clone()).collect();
        match base {
            Some(b) if !taken.contains(&b) => b,
            Some(b) => (2..)
                .map(|n| format!("{b}-{n}"))
                .find(|c| !taken.contains(c))
                .expect("an unbounded range always finds a free name"),
            None => label_for(cwd, &taken),
        }
    }

    /// Open a command shell where the selected session works, as one more
    /// card on the list, and put the keyboard in it.
    pub fn spawn_shell(&mut self) -> Result<()> {
        let cwd = self.default_cwd();
        if !cwd.is_dir() {
            self.notify(format!("no such directory: {}", cwd.display()));
            return Ok(());
        }
        let program = crate::session::shell_program();
        let name = std::path::Path::new(&program)
            .file_stem()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "shell".to_string());
        let label = self.free_label(&cwd, Some(name));
        let session = PtySession::spawn_shell(
            label.clone(),
            cwd.clone(),
            self.pane_rows.max(4),
            self.pane_cols.max(20),
            Arc::new(AtomicBool::new(true)),
        )?;
        self.sessions.push(session);
        self.selected = self.sessions.len() - 1;
        self.mode = Mode::Focus;
        self.notify(format!("{label}: {program} in {}", cwd.display()));
        Ok(())
    }

    /// Arm `u` and wait for the key that says which session it is for.
    pub fn arm_understand(&mut self) {
        self.mode = Mode::Understand;
        self.notify("understand project: F1-F9 session, u new, enter selected, esc cancels");
    }

    /// The next spawn, wherever it comes from, carries the prompt.
    pub fn arm_understand_spawn(&mut self) {
        self.spawn_understand = true;
    }

    /// Quit in the way that brings the fleet back, running the new build.
    ///
    /// The directories in use are written out first: the sessions themselves
    /// cannot survive — a child dies with the pseudoconsole its parent owns —
    /// but where they were working is worth carrying over.
    pub fn restart(&mut self) {
        let list = self.restore_list();
        supervise::write_restore(&list);
        self.restart_requested = true;
        self.should_quit = true;
    }

    /// The live sessions, each with the conversation it was holding.
    ///
    /// The registry says it exactly: every process writes the `sessionId` it
    /// is holding next to its pid, and the pid is ours. That id is taken
    /// whenever a transcript with it exists — a session that never got a
    /// message has an id but nothing to resume.
    ///
    /// Only a session that has not registered yet falls back to a guess by
    /// order: within one directory the transcript written to last belongs to
    /// the session started last. Ids already claimed by the registry are left
    /// out of that guess, so it never hands one conversation to two sessions.
    fn restore_list(&self) -> Vec<supervise::Restore> {
        let registry = registry::read_all();
        let alive: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            // A shell has no conversation, and restoring it as `claude`
            // would start a session nobody asked for.
            .filter(|(_, s)| s.is_alive() && !s.shell)
            .map(|(i, _)| i)
            .collect();

        let mut ids: Vec<Option<String>> = vec![None; self.sessions.len()];
        let mut unregistered: Vec<usize> = Vec::new();
        for &i in &alive {
            let known = self.sessions[i]
                .child_pid
                .and_then(|pid| registry::find_by_pid(&registry, pid))
                .map(|e| e.session_id.clone())
                .filter(|id| !id.is_empty());
            match known {
                Some(id) if history::exists(&id) => ids[i] = Some(id),
                // Registered, but nothing said yet: a fresh start is right.
                Some(_) => {}
                None => unregistered.push(i),
            }
        }

        let claimed: Vec<String> = ids.iter().flatten().cloned().collect();
        let mut done: Vec<&std::path::Path> = Vec::new();
        for &i in &unregistered {
            let cwd = self.sessions[i].cwd.as_path();
            if done.contains(&cwd) {
                continue;
            }
            done.push(cwd);

            let mut here: Vec<usize> = unregistered
                .iter()
                .copied()
                .filter(|&j| self.sessions[j].cwd == cwd)
                .collect();
            here.sort_by_key(|&j| std::cmp::Reverse(self.sessions[j].started));
            let guesses = history::latest_in(cwd, here.len() + claimed.len())
                .into_iter()
                .filter(|c| !claimed.contains(&c.id));
            for (j, c) in here.iter().zip(guesses) {
                ids[*j] = Some(c.id);
            }
        }

        alive
            .into_iter()
            .map(|i| {
                let s = &self.sessions[i];
                let mut r = supervise::Restore::new(s.cwd.clone(), ids[i].clone());
                r.group = s.group;
                r.watch = s.watch.as_ref().map(|w| w.scope.name());
                r
            })
            .collect()
    }

    /// Open the list of past conversations.
    pub fn open_resume_picker(&mut self) {
        let picker = ResumePicker::new();
        if picker.items.is_empty() {
            self.notify("no saved conversations in ~/.claude/projects");
            return;
        }
        self.form = None;
        self.pending_mkdir = None;
        self.resume = Some(picker);
        self.mode = Mode::Resume;
    }

    pub fn close_resume_picker(&mut self) {
        self.resume = None;
        self.mode = Mode::Nav;
    }

    /// Carry on the conversation under the cursor, in the directory it was
    /// held in.
    pub fn resume_selected(&mut self) -> Result<()> {
        let Some(c) = self.resume.as_ref().and_then(|p| p.selected()).cloned() else {
            self.close_resume_picker();
            return Ok(());
        };
        self.close_resume_picker();

        if self.sessions.len() >= 9 {
            self.notify("every F1-F9 slot is taken");
            return Ok(());
        }
        let before = self.sessions.len();
        self.spawn_session_with(c.cwd.clone(), &["--resume".to_string(), c.id.clone()])?;
        if self.sessions.len() > before {
            self.notify(format!("resumed: {}", crate::ui::truncate(&c.summary, 48)));
        }
        Ok(())
    }

    /// Restart, asking first when there is something to lose.
    pub fn request_restart(&mut self) {
        if !self.unsaved_guard("r") {
            return;
        }
        if supervise::origin().is_none() {
            // Started outside the supervisor, so there is nothing to come back
            // as. Saying so beats a key that looks broken.
            self.notify("restart unavailable — fleet was started without a supervisor");
            return;
        }
        if self.sessions.iter().any(|s| s.is_alive()) {
            self.mode = Mode::ConfirmRestart;
            return;
        }
        self.restart();
    }

    /// The file an update would replace, or why there is none.
    fn update_target(&self) -> Result<PathBuf, &'static str> {
        supervise::origin().ok_or("updates need the supervisor — start fleet normally")
    }

    /// Ask GitHub about a newer release, on a thread of its own.
    fn check_for_update(&mut self, asked: bool) {
        self.last_update_check = Some(Instant::now());
        let Ok(origin) = self.update_target() else {
            return;
        };
        if self.update_busy.swap(true, Ordering::Relaxed) {
            return;
        }
        // A cargo build is judged by its checkout, not by its version number.
        let repo = update::dev_repo(&origin);
        let busy = Arc::clone(&self.update_busy);
        let tx = self.update_tx.clone();
        let dirty = Arc::clone(&self.dirty);
        std::thread::spawn(move || {
            // No network is an ordinary state for a laptop, and nothing worth
            // a status line unless someone pressed `i` and is waiting to hear:
            // the next periodic check is a few minutes away regardless.
            match update::check(repo.as_deref()) {
                Ok(Some(rel)) => {
                    let _ = tx.send(UpdateEvent::Found(rel));
                }
                Ok(None) if asked => {
                    let _ = tx.send(UpdateEvent::UpToDate);
                }
                Err(e) if asked => {
                    let _ = tx.send(UpdateEvent::Failed(format!("update check failed: {e:#}")));
                }
                _ => {}
            }
            busy.store(false, Ordering::Relaxed);
            dirty.store(true, Ordering::Relaxed);
        });
    }

    /// The directory the git panel is about: the selected session's, or the
    /// one the fleet was started in when there are no sessions.
    pub fn git_target(&self) -> PathBuf {
        self.default_cwd()
    }

    pub fn toggle_git(&mut self) {
        self.show_git = !self.show_git;
        if !self.show_git && self.mode.on_git() {
            self.mode = Mode::Nav;
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Put the keyboard on the git panel, showing it first if it was hidden.
    pub fn focus_git(&mut self) {
        if !self.git_fits {
            self.notify("the terminal is too narrow for the git panel");
            return;
        }
        self.show_git = true;
        if !self.mode.on_git() {
            // Coming onto the panel leaves the session on the pane until a
            // change or a commit is entered.
            self.git_view.preview_open = false;
        }
        self.mode = Mode::Git;
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Move Generate on to the next model, and remember it for next time.
    pub fn cycle_commit_model(&mut self) {
        self.commit_model = commitmsg::next_model(&self.commit_model, &config::commit_model());
        commitmsg::save_model(&self.commit_model);
        self.notify(format!(
            "commit messages now come from {}",
            self.commit_model
        ));
    }

    /// Put the keyboard in the commit message box.
    pub fn focus_commit(&mut self) {
        self.focus_git();
        if self.mode == Mode::Git {
            self.mode = Mode::Commit;
        }
    }

    /// Put the editor on the pane, its tree rooted where the selected
    /// session works.
    pub fn open_ide(&mut self) {
        let root = self.default_cwd();
        self.ide.tree.set_root(root);
        self.mode = Mode::Ide;
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Whether quitting or restarting may go ahead. Unsaved files in the
    /// editor stop the first try with a word about them; the same key again
    /// within a few seconds goes ahead without them.
    pub fn unsaved_guard(&mut self, key: &str) -> bool {
        let unsaved = self.ide.unsaved();
        if unsaved.is_empty() {
            return true;
        }
        if self
            .unsaved_warned
            .take()
            .is_some_and(|at| at.elapsed() < Duration::from_secs(5))
        {
            return true;
        }
        self.unsaved_warned = Some(Instant::now());
        self.notify(format!(
            "unsaved in the editor: {} — e shows them, {key} again goes ahead without them",
            unsaved.join(", ")
        ));
        false
    }

    /// The snapshot the panel is showing, when it is of a repository.
    fn git_snapshot(&self) -> Option<&git::Snapshot> {
        let target = self.git_target();
        match &self.git {
            Some((cwd, git::State::Repo(snap))) if *cwd == target => Some(snap),
            _ => None,
        }
    }

    /// Start `job` on a thread, unless another one is still running.
    fn start_job(&mut self, job: GitJob, work: impl FnOnce() -> JobDone + Send + 'static) {
        if let Some(running) = self.git_job {
            self.notify(format!("still {}", running.doing()));
            return;
        }
        self.git_job = Some(job);
        let tx = self.job_tx.clone();
        let dirty = Arc::clone(&self.dirty);
        std::thread::spawn(move || {
            let _ = tx.send(work());
            dirty.store(true, Ordering::Relaxed);
        });
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Commit with the message in the box: what is staged, or everything
    /// when nothing is. An empty box has a model write the message first.
    pub fn commit(&mut self) {
        let Some(snap) = self.git_snapshot() else {
            self.notify("not in a git repository");
            return;
        };
        if snap.changes_total == 0 {
            self.notify("nothing to commit");
            return;
        }
        let message = self.commit_msg.trim().to_string();
        let root = snap.root.clone();
        if message.is_empty() {
            // Nothing typed is a request to have it written and committed in
            // one go, not to be sent off to write it.
            let model = self.commit_model.clone();
            self.start_job(GitJob::GenerateCommit, move || {
                let message = match commitmsg::generate(&root, &model) {
                    Ok(m) => m,
                    Err(e) => return JobDone::Failed(GitJob::GenerateCommit, e),
                };
                match git::commit(&root, &message) {
                    Ok(hash) => JobDone::Committed(hash, Some(message)),
                    Err(e) => JobDone::Failed(GitJob::Commit, e),
                }
            });
            return;
        }
        self.start_job(GitJob::Commit, move || match git::commit(&root, &message) {
            Ok(hash) => JobDone::Committed(hash, None),
            Err(e) => JobDone::Failed(GitJob::Commit, e),
        });
    }

    /// Push the branch checked out.
    pub fn push(&mut self) {
        let Some(snap) = self.git_snapshot() else {
            self.notify("not in a git repository");
            return;
        };
        let Some(branch) = snap.branch.clone() else {
            self.notify("HEAD is detached — no branch to push");
            return;
        };
        let root = snap.root.clone();
        let upstream = snap.upstream.is_some();
        self.start_job(GitJob::Push, move || {
            match git::push(&root, &branch, upstream) {
                Ok(()) => JobDone::Pushed,
                Err(e) => JobDone::PushFailed(root, e),
            }
        });
    }

    /// Fetch the remote, so ahead and behind count against what it has now.
    pub fn fetch(&mut self) {
        let Some(snap) = self.git_snapshot() else {
            self.notify("not in a git repository");
            return;
        };
        let root = snap.root.clone();
        self.start_job(GitJob::Fetch, move || match git::fetch(&root) {
            Ok(()) => JobDone::Fetched,
            Err(e) => JobDone::Failed(GitJob::Fetch, e),
        });
    }

    /// Pull the upstream's new commits in under the local ones.
    pub fn pull(&mut self) {
        let Some(snap) = self.git_snapshot() else {
            self.notify("not in a git repository");
            return;
        };
        if snap.branch.is_none() {
            self.notify("HEAD is detached — no branch to pull into");
            return;
        }
        if snap.upstream.is_none() {
            self.notify("this branch has no upstream to pull from — p pushes it first");
            return;
        }
        let root = snap.root.clone();
        self.start_job(GitJob::Pull, move || match git::pull(&root) {
            Ok(()) => JobDone::Pulled,
            Err(e) => JobDone::Failed(GitJob::Pull, git::reason(&e)),
        });
    }

    /// Close the failed-push dialog, back to wherever the keyboard was.
    pub fn close_push_failed(&mut self) {
        let back = self.push_failed.take().map_or(Mode::Nav, |p| p.back);
        self.mode = if back.on_git() && !(self.show_git && self.git_fits) {
            Mode::Nav
        } else {
            back
        };
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Close the failed-push dialog and push again.
    pub fn retry_push(&mut self) {
        self.close_push_failed();
        self.push();
    }

    /// Hand the failed push to a new Sonnet session started in the
    /// repository, with the prompt sent at once — pressing the button was the
    /// ask, and a fresh session carries nothing else it could mix it up with.
    pub fn fix_push(&mut self) -> Result<()> {
        let Some(failed) = self.push_failed.take() else {
            return Ok(());
        };
        self.mode = Mode::Nav;
        let prompt = push_fix_prompt(&failed.failure);
        let args = ["--model".to_string(), PUSH_FIX_MODEL.to_string()];
        let idx = self.spawn_raw(failed.root.clone(), &args, &[], None)?;
        self.selected = idx;
        let s = &mut self.sessions[idx];
        s.queue_submit(&prompt);
        let label = s.label.clone();
        self.mode = Mode::Focus;
        self.notify(format!(
            "{label}: {PUSH_FIX_MODEL} is fixing the push ({})",
            failed.failure.kind.name()
        ));
        Ok(())
    }

    /// Have a model write the message for what a commit would hold now.
    pub fn generate_message(&mut self) {
        let Some(snap) = self.git_snapshot() else {
            self.notify("not in a git repository");
            return;
        };
        if snap.changes_total == 0 {
            self.notify("nothing to commit, so nothing to describe");
            return;
        }
        let root = snap.root.clone();
        let model = self.commit_model.clone();
        self.start_job(GitJob::Generate, move || {
            match commitmsg::generate(&root, &model) {
                Ok(m) => JobDone::Generated(m),
                Err(e) => JobDone::Failed(GitJob::Generate, e),
            }
        });
    }

    fn poll_git_jobs(&mut self) {
        while let Ok(done) = self.job_rx.try_recv() {
            self.git_job = None;
            match done {
                JobDone::Committed(hash, None) => {
                    self.commit_msg.clear();
                    self.commit_cursor = 0;
                    if self.mode == Mode::Commit {
                        self.mode = Mode::Git;
                    }
                    self.notify(format!("committed {hash} — p pushes it"));
                }
                // The box was empty when it started; whatever is in it now was
                // typed meanwhile and is left alone.
                JobDone::Committed(hash, Some(message)) => {
                    if self.mode == Mode::Commit && self.commit_msg.is_empty() {
                        self.mode = Mode::Git;
                    }
                    let subject = message.lines().next().unwrap_or_default();
                    self.notify(format!("committed {hash} {subject} — p pushes it"));
                }
                JobDone::Pushed => self.notify("pushed"),
                JobDone::Fetched => self.notify("fetched"),
                JobDone::Pulled => self.notify("pulled — up to date with the remote"),
                JobDone::PushFailed(root, failure) => {
                    self.notify(format!(
                        "push failed ({}): {}",
                        failure.kind.name(),
                        failure.reason
                    ));
                    let back = match self.mode {
                        Mode::Git | Mode::Commit | Mode::Focus => self.mode,
                        _ => Mode::Nav,
                    };
                    self.push_failed = Some(PushFailed {
                        root,
                        failure,
                        scroll: 0,
                        back,
                    });
                    self.mode = Mode::PushFailed;
                }
                JobDone::Generated(m) => {
                    self.commit_cursor = m.len();
                    self.commit_msg = m;
                    self.notify("message written — enter commits, or edit it first");
                }
                JobDone::Failed(job, why) => {
                    let what = match job {
                        GitJob::Commit => "commit failed",
                        GitJob::GenerateCommit => "no message, nothing committed",
                        GitJob::Push => "push failed",
                        GitJob::Fetch => "fetch failed",
                        GitJob::Pull => "pull failed",
                        GitJob::Generate => "no message",
                    };
                    self.notify(format!("{what}: {why}"));
                }
            }
            // The panel still shows the repository as it was before.
            self.last_git_read = None;
        }
    }

    /// Open the list of branches of the repository the panel is about.
    pub fn open_branch_picker(&mut self) {
        let target = self.git_target();
        let root = match &self.git {
            Some((cwd, git::State::Repo(snap))) if *cwd == target => snap.root.clone(),
            _ => {
                self.notify("not in a git repository");
                return;
            }
        };
        let items = git::branches(&root);
        // Start on the first branch that is not the one already checked out:
        // that one is where nobody needs to go.
        let cursor = items.iter().position(|b| !b.current).unwrap_or(0);
        self.branches = Some(BranchPicker {
            root,
            items,
            filter: String::new(),
            cursor,
            back: if self.mode.on_git() {
                Mode::Git
            } else {
                Mode::Nav
            },
        });
        self.mode = Mode::Branch;
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn close_branch_picker(&mut self) {
        let back = self.branches.take().map_or(Mode::Nav, |p| p.back);
        // The panel may have been hidden or squeezed out while the list was up.
        self.mode = if back == Mode::Git && !(self.show_git && self.git_fits) {
            Mode::Nav
        } else {
            back
        };
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Switch to the branch under the cursor, or start the one typed.
    pub fn switch_selected_branch(&mut self) {
        let Some(picker) = self.branches.as_ref() else {
            return;
        };
        let rows = picker.rows();
        let Some(row) = rows.get(picker.cursor) else {
            return;
        };
        let root = picker.root.clone();
        let (result, name) = match row {
            BranchRow::Branch(b) if b.current => {
                let name = b.name.clone();
                self.close_branch_picker();
                self.notify(format!("already on {name}"));
                return;
            }
            BranchRow::Branch(b) => (git::switch(&root, b), b.local_name().to_string()),
            BranchRow::Create(n) => (git::create_branch(&root, n), n.to_string()),
        };
        match result {
            Ok(()) => {
                self.close_branch_picker();
                self.notify(format!("switched to {name}"));
                // Read the repository again now rather than in two seconds:
                // the panel still shows the branch that was left.
                self.last_git_read = None;
            }
            // The list stays up, so another branch is one keypress away.
            Err(why) => self.notify(why),
        }
    }

    /// Run `f` on the panel's state and the snapshot it is about, when there
    /// is a repository to be about.
    pub fn with_git(&mut self, f: impl FnOnce(&mut GitView, &git::Snapshot)) {
        let target = self.git_target();
        if let Some((cwd, git::State::Repo(snap))) = &self.git
            && *cwd == target
        {
            f(&mut self.git_view, snap);
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Take in finished reads, and start the next one when the selection has
    /// moved to another directory or the last read has aged.
    fn poll_git(&mut self) {
        while let Ok((cwd, state)) = self.git_rx.try_recv() {
            if self.git.as_ref() != Some(&(cwd.clone(), state.clone())) {
                self.git = Some((cwd, state));
                self.git_view.worktree_changed();
                self.dirty.store(true, Ordering::Relaxed);
            }
        }
        // The editor's tree marks changed files, so it wants the reads too.
        if !self.show_git && self.mode != Mode::Ide {
            return;
        }
        let target = self.git_target();
        if let Some((cwd, git::State::Repo(snap))) = &self.git
            && *cwd == target
            && self.git_view.poll(snap)
        {
            self.dirty.store(true, Ordering::Relaxed);
        }
        let due = match &self.last_git_read {
            Some((cwd, at)) => *cwd != target || at.elapsed() >= GIT_REFRESH,
            None => true,
        };
        if !due || self.git_busy.swap(true, Ordering::Relaxed) {
            return;
        }
        self.last_git_read = Some((target.clone(), Instant::now()));
        let busy = Arc::clone(&self.git_busy);
        let tx = self.git_tx.clone();
        // No redraw from here: `tick` runs every frame and redraws only when
        // what came back differs from what is on screen.
        std::thread::spawn(move || {
            let state = git::read(&target);
            let _ = tx.send((target, state));
            busy.store(false, Ordering::Relaxed);
        });
    }

    /// Download the release found earlier and put it in place of the binary
    /// the supervisor starts. The restart that picks it up is asked for once
    /// the file is there.
    pub fn install_update(&mut self) {
        let origin = match self.update_target() {
            Ok(o) => o,
            Err(why) => {
                self.notify(why);
                return;
            }
        };
        let Some(rel) = self.release.clone() else {
            // Nothing found yet may only mean nothing asked since it came out,
            // so the key asks now instead of saying no.
            if self.update_busy() {
                self.notify("already talking to GitHub — a moment");
            } else {
                self.check_for_update(true);
                self.notify("asking GitHub for a newer release…");
            }
            return;
        };
        if self.update_busy.swap(true, Ordering::Relaxed) {
            self.notify("already talking to GitHub — a moment");
            return;
        }
        self.notify(format!("downloading {}…", rel.tag));
        let busy = Arc::clone(&self.update_busy);
        let tx = self.update_tx.clone();
        let dirty = Arc::clone(&self.dirty);
        std::thread::spawn(move || {
            let ev = match update::install(&rel, &origin) {
                Ok(()) => UpdateEvent::Installed(rel.tag),
                Err(e) => UpdateEvent::Failed(format!("update failed: {e:#}")),
            };
            let _ = tx.send(ev);
            busy.store(false, Ordering::Relaxed);
            dirty.store(true, Ordering::Relaxed);
        });
    }

    /// Whether a download is running right now.
    pub fn update_busy(&self) -> bool {
        self.update_busy.load(Ordering::Relaxed)
    }

    fn poll_update(&mut self) {
        while let Ok(ev) = self.update_rx.try_recv() {
            match ev {
                UpdateEvent::Found(rel) => {
                    // Said once per release, not on every periodic check.
                    if self.release.as_ref().map(|r| &r.tag) != Some(&rel.tag) {
                        self.notify(format!("{} is out — i installs it", rel.tag));
                    }
                    self.release = Some(rel);
                }
                UpdateEvent::Installed(tag) => {
                    self.release = None;
                    self.update_ready = true;
                    self.origin_stamp = supervise::origin_stamp();
                    self.notify(format!("{tag} installed — r restarts into it"));
                    // Straight into the restart when the list is what is on
                    // screen; from inside a pane the question would land on
                    // someone typing.
                    if self.mode == Mode::Nav {
                        self.request_restart();
                    }
                }
                UpdateEvent::UpToDate => {
                    self.notify(format!("no newer release — this is v{}", update::CURRENT));
                }
                UpdateEvent::Failed(e) => self.notify(e),
            }
        }
        let due = self
            .last_update_check
            .is_none_or(|at| at.elapsed() >= UPDATE_CHECK);
        if due && config::check_updates() {
            self.check_for_update(false);
        }
    }

    /// Reopen what a previous run left behind, carrying on the conversations
    /// it was holding wherever their transcripts could be found.
    pub fn restore_sessions(&mut self, items: Vec<supervise::Restore>) -> Result<()> {
        let mut resumed = 0;
        for item in items {
            if let Some(scope) = item.watch.as_deref().and_then(Scope::parse) {
                resumed += usize::from(item.session.is_some());
                self.start_big_brother(scope, item.cwd, item.session)?;
                continue;
            }
            let before = self.sessions.len();
            match item.session {
                Some(id) => {
                    self.spawn_session_with(item.cwd, &["--resume".to_string(), id])?;
                    resumed += 1;
                }
                None => self.spawn_session(item.cwd)?,
            }
            if self.sessions.len() > before
                && let Some(s) = self.sessions.last_mut()
            {
                s.group = item.group;
            }
        }
        if !self.sessions.is_empty() {
            // Coming back into a focused pane of a session that is still
            // painting reads as a freeze; the list shows what came back.
            self.mode = Mode::Nav;
            self.selected = 0;
            let n = self.sessions.len();
            self.notify(if resumed == n {
                format!("{n} sessions came back after the restart, with their conversations")
            } else {
                format!(
                    "{n} sessions came back after the restart, conversations resumed: {resumed}"
                )
            });
        }
        Ok(())
    }

    /// Drop an armed prompt that never got a session, so it cannot ride along
    /// with an unrelated spawn later.
    pub fn disarm_understand_spawn(&mut self) {
        self.spawn_understand = false;
    }

    /// Start the window in which a lone `u` is still the chord.
    pub fn open_understand_window(&mut self, idx: usize) {
        self.understand_window = Some((idx, Instant::now()));
    }

    /// Read the window back, if it is still open and still points somewhere.
    /// Consuming it either way keeps a stale window from firing much later.
    pub fn take_understand_window(&mut self) -> Option<usize> {
        let (idx, at) = self.understand_window.take()?;
        (at.elapsed() <= config::understand_window() && idx < self.sessions.len()).then_some(idx)
    }

    /// Put the prompt into a session that already exists.
    pub fn understand(&mut self, idx: usize) {
        self.understand_window = None;
        let Some(s) = self.sessions.get_mut(idx) else {
            self.notify("no such session");
            return;
        };
        if !s.is_alive() {
            let label = s.label.clone();
            self.notify(format!("{label} has finished — nowhere to type"));
            return;
        }
        if s.shell {
            let label = s.label.clone();
            self.notify(format!("{label} is a shell, not a Claude session"));
            return;
        }
        let label = s.label.clone();
        let prompt = config::understand_prompt();
        s.queue_prompt(&prompt);
        self.selected = idx;
        self.mode = Mode::Focus;
        self.notify(format!(
            "{label}: \"{prompt}\" is in the prompt — enter sends it"
        ));
    }

    /// Where a new session lands when nothing else says otherwise: next to the
    /// session you are looking at, or the directory the fleet was started in.
    pub fn default_cwd(&self) -> PathBuf {
        self.selected_session()
            .map(|s| s.cwd.clone())
            .unwrap_or_else(|| self.launch_cwd.clone())
    }

    /// Spawn into `cwd`, but stop and ask first when the directory is missing.
    /// Typing a path that does not exist yet is a normal thing to do; refusing
    /// it outright means retyping it somewhere else.
    pub fn request_spawn(&mut self, cwd: PathBuf) -> Result<()> {
        if cwd.is_dir() {
            let kind = self.form_kind();
            self.form = None;
            self.mode = Mode::Nav;
            return self.spawn_session_kind(cwd, kind);
        }
        if cwd.exists() {
            self.notify(format!("not a directory: {}", cwd.display()));
            return Ok(());
        }
        if cwd.as_os_str().is_empty() {
            self.notify("empty path");
            return Ok(());
        }
        self.pending_mkdir = Some(cwd);
        self.mode = Mode::ConfirmMkdir;
        Ok(())
    }

    /// Answer to the "create it?" dialog. `yes` creates the directory and
    /// spawns; anything else drops back into the form with the path intact.
    pub fn resolve_mkdir(&mut self, yes: bool) -> Result<()> {
        let Some(path) = self.pending_mkdir.take() else {
            self.mode = Mode::Nav;
            return Ok(());
        };
        if !yes {
            // The form is still behind the dialog, so editing continues where
            // it left off.
            self.mode = if self.form.is_some() {
                Mode::NewSession
            } else {
                Mode::Nav
            };
            return Ok(());
        }
        if let Err(e) = std::fs::create_dir_all(&path) {
            self.mode = if self.form.is_some() {
                Mode::NewSession
            } else {
                Mode::Nav
            };
            self.notify(format!("could not create the directory: {e}"));
            return Ok(());
        }
        let kind = self.form_kind();
        self.form = None;
        self.mode = Mode::Nav;
        // `spawn_session` reports the session it started; saying "utworzono"
        // here would only be overwritten by it a moment later.
        self.spawn_session_kind(path, kind)
    }

    /// The kind picked in the open form; local when there is none.
    fn form_kind(&self) -> SpawnKind {
        self.form.as_ref().map_or(SpawnKind::Local, |f| f.kind)
    }

    /// List the repositories on a thread, unless a list is there or coming.
    /// A list that came back short (no token, API down) is asked for again.
    pub fn load_repos(&mut self) {
        let short = self.repos.as_ref().is_some_and(|l| l.note.is_some());
        if self.repos_rx.is_some() || (self.repos.is_some() && !short) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let dirty = Arc::clone(&self.dirty);
        std::thread::spawn(move || {
            let _ = tx.send(repos::list());
            dirty.store(true, Ordering::Relaxed);
        });
        self.repos_rx = Some(rx);
    }

    pub fn repos_loading(&self) -> bool {
        self.repos_rx.is_some()
    }

    /// The repositories matching what was typed into the form.
    pub fn filtered_repos(&self) -> Vec<&repos::Repo> {
        let (Some(list), Some(form)) = (&self.repos, &self.form) else {
            return Vec::new();
        };
        let filter = form.repo_filter.to_lowercase();
        list.repos
            .iter()
            .filter(|r| r.full_name.to_lowercase().contains(&filter))
            .collect()
    }

    /// Start a remote session for the repository under the form's cursor:
    /// get it a checkout on a thread, then `claude --cloud` in it. With no
    /// repository to pick, the typed directory is used as it stands.
    pub fn start_remote(&mut self) -> Result<()> {
        if self.cloning.is_some() {
            self.notify("still preparing the previous repository");
            return Ok(());
        }
        let cursor = self.form.as_ref().map_or(0, |f| f.repo_cursor);
        let Some(repo) = self.filtered_repos().get(cursor).map(|r| (*r).clone()) else {
            let path = self
                .form
                .as_ref()
                .map(|f| f.selected_path())
                .unwrap_or_default();
            return self.request_spawn(path);
        };
        self.form = None;
        self.mode = Mode::Nav;
        self.spawn_understand = false;
        let (tx, rx) = mpsc::channel();
        let dirty = Arc::clone(&self.dirty);
        let name = repo.full_name.clone();
        let fresh = repo.local.is_none();
        std::thread::spawn(move || {
            let _ = tx.send(repos::checkout(&repo));
            dirty.store(true, Ordering::Relaxed);
        });
        self.clone_rx = Some(rx);
        self.notify(if fresh {
            format!("cloning {name} for the remote session…")
        } else {
            format!("starting a remote session for {name}…")
        });
        self.cloning = Some(name);
        Ok(())
    }

    fn poll_repos(&mut self) {
        if let Some(rx) = &self.repos_rx
            && let Ok(listing) = rx.try_recv()
        {
            self.repos_rx = None;
            self.repos = Some(listing);
            if let Some(form) = self.form.as_mut() {
                form.repo_cursor = 0;
            }
        }
        if let Some(rx) = &self.clone_rx
            && let Ok(done) = rx.try_recv()
        {
            self.clone_rx = None;
            let name = self.cloning.take().unwrap_or_default();
            match done {
                Ok(dir) => {
                    // A clone fleet just made is a local clone from now on.
                    if let Some(r) = self
                        .repos
                        .as_mut()
                        .and_then(|l| l.repos.iter_mut().find(|r| r.full_name == name))
                    {
                        r.local.get_or_insert(dir.clone());
                    }
                    if let Err(e) = self.spawn_session_kind(dir, SpawnKind::Remote) {
                        self.notify(format!("{name}: {e}"));
                    }
                }
                Err(e) => self.notify(format!("{name}: {e}")),
            }
        }
    }

    /// Spawn a session of the given kind in `cwd`.
    pub fn spawn_session_kind(&mut self, cwd: PathBuf, kind: SpawnKind) -> Result<()> {
        if kind == SpawnKind::Local {
            return self.spawn_session(cwd);
        }
        // The understand prompt is for a fresh local box; a cloud session or
        // the teleport picker has no place for it.
        self.spawn_understand = false;
        self.spawn_session_with(cwd, &kind.args())?;
        if let Some(s) = self.sessions.last() {
            let label = s.label.clone();
            self.notify(match kind {
                SpawnKind::Remote => format!("{label}: new remote session (claude --cloud)"),
                _ => format!("{label}: pick a remote session to teleport (claude --teleport)"),
            });
        }
        Ok(())
    }

    pub fn open_new_session_form(&mut self) {
        self.open_new_session_form_with(false);
    }

    /// The form, with the understand prompt armed or explicitly disarmed.
    /// Opening it plainly has to clear the flag: an abandoned `u` from earlier
    /// must not ride along with the next ordinary new session.
    pub fn open_new_session_form_with(&mut self, understand: bool) {
        self.spawn_understand = understand;
        // A fresh form means the earlier "create it?" question is void.
        self.pending_mkdir = None;
        let default = self.default_cwd();
        self.form = Some(NewSessionForm::new(&default));
        self.mode = Mode::NewSession;
    }

    pub fn kill_selected(&mut self) {
        if let Some(s) = self.sessions.get_mut(self.selected) {
            let label = s.label.clone();
            s.kill();
            self.notify(format!("killed {label}"));
        }
    }

    pub fn close_selected(&mut self) {
        if self.selected < self.sessions.len() {
            self.sessions.remove(self.selected);
            self.selected = self.selected.saturating_sub(1);
            if self.sessions.is_empty() {
                self.mode = Mode::Nav;
            }
        }
    }

    /// Drop the cards of sessions that have been dead for longer than
    /// the configured TTL, keeping the selection on the same session where it
    /// can.
    fn expire_finished(&mut self) {
        let expired: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.finished_for()
                    .is_some_and(|d| d >= config::finished_ttl())
            })
            .map(|(i, _)| i)
            .collect();
        if expired.is_empty() {
            return;
        }

        // Removing back to front keeps the remaining indices valid.
        for &i in expired.iter().rev() {
            self.sessions.remove(i);
            if i < self.selected {
                self.selected -= 1;
            }
        }
        self.selected = self.selected.min(self.sessions.len().saturating_sub(1));
        if self.sessions.is_empty() || !self.sessions[self.selected].is_alive() {
            // Nothing left to type into, so never leave the user in a focused
            // pane that no longer exists.
            if self.mode == Mode::Focus {
                self.mode = Mode::Nav;
            }
        }
        let n = expired.len();
        self.notify(if n == 1 {
            "closed the finished session".to_string()
        } else {
            format!("closed finished sessions ({n})")
        });
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn select(&mut self, delta: isize) {
        if self.sessions.is_empty() {
            return;
        }
        let len = self.sessions.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.selected = next as usize;
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn select_index(&mut self, idx: usize) {
        if idx < self.sessions.len() {
            self.selected = idx;
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Put the selected session in a group, or take it out of one.
    pub fn set_group(&mut self, group: Option<char>) {
        self.mode = Mode::Nav;
        let Some(s) = self.sessions.get_mut(self.selected) else {
            return;
        };
        if s.watch.is_some() {
            self.notify("a Big Brother's scope is fixed when it starts — B starts another");
            return;
        }
        s.group = group;
        let label = s.label.clone();
        self.notify(match group {
            Some(g) => format!("{label} is in group {g} now"),
            None => format!("{label} is in no group now"),
        });
    }

    /// The groups in use, with how many sessions each holds.
    pub fn groups(&self) -> Vec<(char, usize)> {
        let mut out: Vec<(char, usize)> = Vec::new();
        for g in self
            .sessions
            .iter()
            .filter(|s| s.watch.is_none())
            .filter_map(|s| s.group)
        {
            match out.iter_mut().find(|(c, _)| *c == g) {
                Some((_, n)) => *n += 1,
                None => out.push((g, 1)),
            }
        }
        out.sort();
        out
    }

    /// What `B` then `enter` watches: the selected session's group, or all.
    pub fn default_scope(&self) -> Scope {
        self.selected_session()
            .filter(|s| s.watch.is_none())
            .and_then(|s| s.group)
            .map_or(Scope::All, Scope::Group)
    }

    /// Start a Big Brother over `scope` and put the pane on it — or on the
    /// one already watching that scope.
    pub fn spawn_big_brother(&mut self, scope: Scope) -> Result<()> {
        self.mode = Mode::Nav;
        if let Some(i) = self
            .sessions
            .iter()
            .position(|s| s.is_alive() && s.watch.as_ref().is_some_and(|w| w.scope == scope))
        {
            self.selected = i;
            self.mode = Mode::Focus;
            self.notify(format!("{} is already watching", self.sessions[i].label));
            return Ok(());
        }
        let cwd = self.launch_cwd.clone();
        if let Some(i) = self.start_big_brother(scope, cwd, None)? {
            self.selected = i;
            self.mode = Mode::Focus;
            self.notify(format!(
                "{} started — it watches {}",
                self.sessions[i].label,
                scope.describe()
            ));
        }
        Ok(())
    }

    /// Start a Big Brother, carrying on `resume` when given. Returns its index.
    fn start_big_brother(
        &mut self,
        scope: Scope,
        cwd: PathBuf,
        resume: Option<String>,
    ) -> Result<Option<usize>> {
        if self.bb_server.is_none() {
            self.bb_server = Some(bigbrother::Server::start(Arc::clone(&self.dirty))?);
        }
        let exe = std::env::current_exe()?;
        if self.bb_shim.is_none() {
            self.bb_shim = Some(bigbrother::write_shim(&exe)?);
        }
        let (Some(server), Some(shim)) = (&self.bb_server, &self.bb_shim) else {
            return Ok(None);
        };
        let cwd = if cwd.is_dir() {
            cwd
        } else {
            self.launch_cwd.clone()
        };
        let token = bigbrother::new_token();
        let env = bigbrother::child_env(&server.addr, &token, shim, &exe);
        let mut args = Vec::new();
        if let Some(id) = &resume {
            args.push("--resume".to_string());
            args.push(id.clone());
        }
        args.extend(bigbrother::claude_args(scope));
        let idx = self.spawn_raw(cwd, &args, &env, Some(scope.label()))?;
        let s = &mut self.sessions[idx];
        s.watch = Some(bigbrother::Watch { scope, token });
        s.queue_submit(if resume.is_some() {
            "The fleet restarted and you are back. Carry on watching: `fleet list`, then `fleet wait`."
        } else {
            bigbrother::KICKOFF
        });
        // It hears about what happens from now on; `fleet list` tells it the rest.
        let uid = s.uid;
        self.bb_cursor.insert(uid, self.bb_seq);
        Ok(Some(idx))
    }

    /// How a session stands, in words a Big Brother reads.
    pub fn state_of(&self, idx: usize) -> String {
        let Some(s) = self.sessions.get(idx) else {
            return "gone".to_string();
        };
        if !s.is_alive() {
            return "finished".to_string();
        }
        if s.shell {
            return "a command shell".to_string();
        }
        match self.entry_for(idx) {
            Some(e) if e.status == "busy" => "working".to_string(),
            Some(e) if e.status == "idle" => "idle".to_string(),
            Some(e) if e.is_waiting() && !e.waiting_for.is_empty() => {
                format!("waiting for the user ({})", e.waiting_for)
            }
            Some(e) if e.is_waiting() => "waiting for the user".to_string(),
            Some(e) => e.status.clone(),
            None => "starting".to_string(),
        }
    }

    /// Note every watched session whose state moved since the last look.
    fn record_states(&mut self) {
        let mut seen = Vec::new();
        for i in 0..self.sessions.len() {
            let s = &self.sessions[i];
            if s.watch.is_some() {
                continue;
            }
            let (uid, label, group) = (s.uid, s.label.clone(), s.group);
            let cwd = s.cwd.display().to_string();
            seen.push(uid);
            let now = self.state_of(i);
            let text = match self.last_state.get(&uid) {
                Some(before) if *before == now => continue,
                Some(before) => format!("{label}: {before} -> {now}"),
                None => format!("{label}: started in {cwd} ({now})"),
            };
            self.last_state.insert(uid, now);
            self.bb_seq += 1;
            self.bb_events.push(bigbrother::Event {
                seq: self.bb_seq,
                group,
                text,
                at: Instant::now(),
            });
        }
        self.last_state.retain(|uid, _| seen.contains(uid));
        if self.bb_events.len() > EVENT_LOG {
            let excess = self.bb_events.len() - EVENT_LOG;
            self.bb_events.drain(..excess);
        }
    }

    /// Answer whatever the Big Brothers asked since the last frame.
    fn poll_big_brother(&mut self) {
        let mut requests = Vec::new();
        if let Some(server) = &self.bb_server {
            while let Some(req) = server.try_recv() {
                requests.push(req);
            }
        }
        for req in requests {
            let answer = self.answer_big_brother(&req.token, &req.cmd, &req.args);
            req.answer(match answer {
                Ok(v) => v,
                Err(e) => bigbrother::err(e),
            });
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// The sessions a Big Brother over `scope` may see: not itself, not any
    /// other Big Brother, and only its own group unless it watches them all.
    fn watched(&self, scope: Scope) -> Vec<usize> {
        (0..self.sessions.len())
            .filter(|&i| {
                let s = &self.sessions[i];
                s.watch.is_none() && scope.covers(s.group)
            })
            .collect()
    }

    /// A session named in a request, within the asker's scope.
    fn target(&self, scope: Scope, name: Option<&String>) -> Result<usize, String> {
        let name = name.ok_or("which session? give its name (fleet list shows them)")?;
        let watched = self.watched(scope);
        let by_key = name
            .strip_prefix(['F', 'f'])
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|n| n.checked_sub(1));
        watched
            .iter()
            .copied()
            .find(|&i| self.sessions[i].label.eq_ignore_ascii_case(name))
            .or_else(|| by_key.filter(|k| watched.contains(k)))
            .ok_or_else(|| {
                let names: Vec<&str> = watched
                    .iter()
                    .map(|&i| self.sessions[i].label.as_str())
                    .collect();
                if names.is_empty() {
                    format!("no session called {name} — you watch none right now")
                } else {
                    format!(
                        "no session called {name} in your scope; there is: {}",
                        names.join(", ")
                    )
                }
            })
    }

    fn answer_big_brother(
        &mut self,
        token: &str,
        cmd: &str,
        args: &[String],
    ) -> Result<serde_json::Value, String> {
        let me = self
            .sessions
            .iter()
            .position(|s| s.watch.as_ref().is_some_and(|w| w.token == token))
            .ok_or("this Big Brother is not known to the fleet (was it restarted?)")?;
        let (my_uid, my_label, my_cwd) = {
            let s = &self.sessions[me];
            (s.uid, s.label.clone(), s.cwd.clone())
        };
        let scope = self.sessions[me]
            .watch
            .as_ref()
            .map_or(Scope::All, |w| w.scope);
        let ok = |t: String| Ok(bigbrother::ok(t));

        match cmd {
            "whoami" => ok(format!("you are {my_label}, watching {}", scope.describe())),
            "list" => {
                let rows: Vec<String> = self
                    .watched(scope)
                    .into_iter()
                    .map(|i| {
                        let s = &self.sessions[i];
                        format!(
                            "{:<18} group {:<2} F{:<2} {:<28} up {:<6} {}",
                            s.label,
                            s.group.map(String::from).unwrap_or_else(|| "-".into()),
                            i + 1,
                            self.state_of(i),
                            crate::ui::fmt_uptime(s.started.elapsed()),
                            s.cwd.display()
                        )
                    })
                    .collect();
                ok(if rows.is_empty() {
                    format!("no sessions in your scope ({})", scope.describe())
                } else {
                    rows.join("\n")
                })
            }
            "events" => {
                let from = self.bb_cursor.get(&my_uid).copied().unwrap_or(0);
                let lines: Vec<String> = self
                    .bb_events
                    .iter()
                    .filter(|e| e.seq > from && scope.covers(e.group))
                    .map(|e| format!("[{}s ago] {}", e.at.elapsed().as_secs(), e.text))
                    .collect();
                self.bb_cursor.insert(my_uid, self.bb_seq);
                ok(lines.join("\n"))
            }
            "peek" => {
                let i = self.target(scope, args.first())?;
                let screen = self.sessions[i].screen_text();
                let mut lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
                while lines.last().is_some_and(|l| l.is_empty()) {
                    lines.pop();
                }
                if let Some(n) = args.get(1).and_then(|n| n.parse::<usize>().ok()) {
                    let start = lines.len().saturating_sub(n);
                    lines.drain(..start);
                }
                ok(format!(
                    "--- {} ({}) ---\n{}",
                    self.sessions[i].label,
                    self.state_of(i),
                    lines.join("\n")
                ))
            }
            "resolve" => {
                let i = self.target(scope, args.first())?;
                let id = self
                    .entry_for(i)
                    .map(|e| e.session_id.clone())
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| format!("{} has no conversation yet", self.sessions[i].label))?;
                Ok(serde_json::json!({
                    "ok": true,
                    "text": "",
                    "session_id": id,
                    "cwd": self.sessions[i].cwd.display().to_string(),
                }))
            }
            "send" | "clear" | "key" | "kill" => {
                let i = self.target(scope, args.first())?;
                let s = &mut self.sessions[i];
                if !s.is_alive() {
                    return Err(format!("{} has finished", s.label));
                }
                let label = s.label.clone();
                let done = match cmd {
                    "send" => {
                        let text = args[1..].join(" ");
                        if text.trim().is_empty() {
                            return Err("usage: fleet send <name> <text>".into());
                        }
                        s.queue_submit(&text);
                        format!("sent to {label}")
                    }
                    "clear" => {
                        s.queue_submit("/clear");
                        format!("{label}: /clear sent")
                    }
                    "key" => {
                        if args.len() < 2 {
                            return Err("usage: fleet key <name> <key>...".into());
                        }
                        let mut bytes = Vec::new();
                        for k in &args[1..] {
                            bytes.extend(
                                bigbrother::key_bytes(k).ok_or(format!("unknown key: {k}"))?,
                            );
                        }
                        s.write_passthrough(&bytes).map_err(|e| e.to_string())?;
                        format!("{label}: pressed {}", args[1..].join(" "))
                    }
                    _ => {
                        s.kill();
                        format!("killed {label}")
                    }
                };
                self.notify(format!("{my_label}: {done}"));
                ok(done)
            }
            "spawn" => {
                let dir = args.first().ok_or("usage: fleet spawn <dir> [prompt]")?;
                let path = PathBuf::from(dir);
                let path = if path.is_absolute() {
                    path
                } else {
                    my_cwd.join(path)
                };
                if !path.is_dir() {
                    return Err(format!("no such directory: {}", path.display()));
                }
                let idx = self
                    .spawn_raw(path, &[], &[], None)
                    .map_err(|e| e.to_string())?;
                let s = &mut self.sessions[idx];
                if let Scope::Group(g) = scope {
                    s.group = Some(g);
                }
                let prompt = args[1..].join(" ");
                if !prompt.trim().is_empty() {
                    s.queue_submit(&prompt);
                }
                let label = s.label.clone();
                self.notify(format!("{my_label} started {label}"));
                ok(format!("started {label} (F{})", idx + 1))
            }
            "alert" => {
                let (level, text) = match args.first().and_then(|a| Level::parse(a)) {
                    Some(l) => (l, args[1..].join(" ")),
                    None => (Level::Warn, args.join(" ")),
                };
                if text.trim().is_empty() {
                    return Err("usage: fleet alert <info|warn|alarm> <text>".into());
                }
                self.file_report(my_label, level, text);
                ok("reported".to_string())
            }
            other => Err(format!("unknown command: {other} (fleet help lists them)")),
        }
    }

    /// Keep a report, say it on the status line, and ring for the serious ones.
    fn file_report(&mut self, from: String, level: Level, text: String) {
        self.notify(format!("{from} [{}]: {text}", level.name()));
        if level >= Level::Warn {
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x07");
            let _ = out.flush();
        }
        self.reports.push(Report {
            from,
            level,
            text,
            at: Instant::now(),
        });
        if self.reports.len() > REPORT_LOG {
            self.reports.remove(0);
        }
        self.unread_reports += 1;
        self.unread_level = self.unread_level.max(Some(level));
    }

    pub fn open_reports(&mut self) {
        if self.reports.is_empty() {
            self.notify("no reports yet — B starts a Big Brother");
            return;
        }
        self.unread_reports = 0;
        self.unread_level = None;
        self.reports_scroll = 0;
        self.mode = Mode::Reports;
    }

    /// Per-frame bookkeeping: reap dead children, drop expired cards, refresh
    /// the registry, expire the status line.
    pub fn tick(&mut self) {
        // Reaps children and records exit codes; the return value is read via
        // `is_alive` during render.
        for (i, s) in self.sessions.iter_mut().enumerate() {
            let died = s.is_alive() && !s.poll_alive();
            // Output of a pane nobody is looking at changes nothing on screen,
            // so a busy session in the background no longer forces a redraw
            // every frame. Its card only changes when it dies.
            let settled = s.settle_resize();
            if (s.take_output() || settled) && i == self.selected || died {
                self.dirty.store(true, Ordering::Relaxed);
            }
            // Text queued before the child painted its input box goes in as
            // soon as it has one.
            s.flush_prompt();
        }
        // A draining paste changes the pane's badge every frame, and the child's
        // own output may be quiet meanwhile, so ask for the redraw here.
        if self
            .sessions
            .iter()
            .any(|s| s.queued_input() > 0 || s.prompt_pending())
        {
            self.dirty.store(true, Ordering::Relaxed);
        }
        self.expire_finished();

        // The config is read back whenever the file moves, so an edit shows up
        // in the next frame without anything being restarted.
        if self.last_config_check.elapsed() >= CONFIG_CHECK {
            self.last_config_check = Instant::now();
            match config::reload_if_changed() {
                Some(config::Reload::Applied) => self.notify("config reloaded"),
                Some(config::Reload::Failed(e)) => self.notify(format!("config rejected: {e}")),
                None => {}
            }
        }

        if !self.update_ready && self.last_exe_check.elapsed() >= EXE_CHECK {
            self.last_exe_check = Instant::now();
            let now = supervise::origin_stamp();
            if now.is_some() && now != self.origin_stamp {
                self.update_ready = true;
                self.notify("new build ready — r restarts the fleet");
            }
        }

        self.poll_big_brother();
        self.poll_update();
        self.poll_git();
        self.poll_git_jobs();
        self.poll_repos();

        let on_ide = self.mode == Mode::Ide;
        if on_ide {
            // The wheel over the list can move the selection meanwhile.
            let root = self.default_cwd();
            self.ide.tree.set_root(root);
        }
        if self.ide.tick(on_ide) {
            self.dirty.store(true, Ordering::Relaxed);
        }
        if let Some(msg) = self.ide.message.take() {
            self.notify(msg);
        }

        // Nothing else moves on an empty pane, so the turning name and the
        // dancing Clawd ask for their own frames.
        let empty_pane = self.sessions.is_empty()
            && !on_ide
            && !(self.mode.on_git() && self.git_view.preview_open);
        if empty_pane && self.last_splash.elapsed() >= splash::FRAME {
            self.last_splash = Instant::now();
            self.dirty.store(true, Ordering::Relaxed);
        }

        if self.last_registry_scan.elapsed() >= REGISTRY_REFRESH {
            if let Some(latest) = self.registry_rx.try_iter().last() {
                self.registry = latest;
            }
            self.last_registry_scan = Instant::now();
            self.dirty.store(true, Ordering::Relaxed);
            // Same cadence as the registry: the limits move slowly, and their
            // countdowns only need to be right to the minute on screen.
            if self.usage.refresh() {
                self.spent_since_refresh = false;
                self.dirty.store(true, Ordering::Relaxed);
            }
            // A session of ours that is working is spending the limits the
            // footer is showing, so the numbers on screen are already wrong.
            if self.any_own_busy() {
                self.spent_since_refresh = true;
            }
            self.record_states();
            if self.usage_refresh_due() {
                self.refresh_usage();
            }
        }

        if let Some((_, at)) = &self.status
            && at.elapsed() > Duration::from_secs(4)
        {
            self.status = None;
            self.dirty.store(true, Ordering::Relaxed);
        }
    }
}

/// Re-read the registry on a thread of its own for as long as the app is
/// there to take the results; the thread ends with the receiver.
fn spawn_registry_scanner() -> mpsc::Receiver<Vec<RegistryEntry>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        while tx.send(registry::read_all()).is_ok() {
            std::thread::sleep(REGISTRY_REFRESH);
        }
    });
    rx
}

/// What a session is told when asked to sort out a failed push.
fn push_fix_prompt(failure: &git::PushFailure) -> String {
    let what = match failure.kind {
        git::PushError::Conflict => {
            "The repository is stopped on a merge conflict. Resolve the conflicts \
             keeping the intent of both sides, finish the merge or rebase, then push."
        }
        git::PushError::Rejected => {
            "The remote has commits this branch does not. Pull them in (rebase unless \
             the history says merges are the norm here), resolve any conflicts keeping \
             the intent of both sides, then push. Do not force-push."
        }
        git::PushError::Auth => {
            "The remote refused the credentials. Find out why (credential helper, \
             token, SSH key, remote URL) and fix what can be fixed from here; tell me \
             exactly what I have to do myself for the rest."
        }
        git::PushError::Network => {
            "The remote could not be reached. Check whether it is the network, a \
             proxy or a wrong remote URL, and push again once it is sorted out."
        }
        git::PushError::Declined => {
            "The remote declined the push (a hook or branch protection). Find out \
             what rule it hit and what the way through is; do not force-push or \
             bypass the protection."
        }
        git::PushError::TooLarge => {
            "A file in the commits is over the remote's size limit. Take it out of \
             the unpushed commits (or move it to Git LFS if that is set up here), \
             then push."
        }
        git::PushError::NoRemote => {
            "There is no usable remote to push to. Find out what is missing and set \
             it up if the right remote is clear; ask me otherwise."
        }
        git::PushError::Other => "Find out what went wrong and fix it, then push.",
    };
    format!(
        "git push failed ({kind}). {what}\n\nWhat git said:\n{output}\n\nThe branch at the time:\n{log}",
        kind = failure.kind.name(),
        output = failure.output,
        log = failure.log,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::new(PathBuf::from("."))
    }

    #[test]
    fn the_understand_window_needs_a_session_behind_it() {
        let mut app = app();
        app.open_understand_window(0);
        // The window points at a session that is not there, so a `u` arriving
        // now is an ordinary letter.
        assert!(app.take_understand_window().is_none());
    }

    #[test]
    fn a_stale_window_no_longer_answers() {
        let mut app = app();
        app.understand_window = Some((0, Instant::now() - config::understand_window() * 2));
        assert!(app.take_understand_window().is_none());
    }

    #[test]
    fn the_window_answers_once() {
        let mut app = app();
        app.open_understand_window(0);
        // Consumed either way: a window left open would fire much later, on a
        // `u` that meant nothing of the sort.
        let _ = app.take_understand_window();
        assert!(app.understand_window.is_none());
    }

    fn aged(secs: u64) -> usage::Usage {
        usage::Usage {
            session: None,
            weekly: None,
            fetched_ago: Duration::from_secs(secs),
        }
    }

    #[test]
    fn limits_are_refreshed_sooner_when_our_own_sessions_are_spending_them() {
        let mut app = app();

        // Nothing of ours has run, so numbers this age are probably still right.
        app.usage.current = Some(aged(10 * 60));
        app.spent_since_refresh = false;
        assert!(!app.usage_refresh_due());

        // The same age, with a session of ours working since they were fetched:
        // whatever is on screen is already out of date.
        app.spent_since_refresh = true;
        assert!(app.usage_refresh_due());
    }

    #[test]
    fn an_idle_fleet_still_asks_eventually() {
        let mut app = app();
        app.spent_since_refresh = false;
        app.usage.current = Some(aged(31 * 60));
        // Usage moves on other machines too, so the long clock exists.
        assert!(app.usage_refresh_due());
    }

    #[test]
    fn numbers_fresher_than_claude_codes_own_cache_are_left_alone() {
        let mut app = app();
        app.spent_since_refresh = true;
        // Asking again this soon spawns a child that changes nothing: Claude
        // Code would answer `/usage` from what it already has.
        app.usage.current = Some(aged(60));
        assert!(!app.usage_refresh_due());
    }

    #[test]
    fn one_refresh_at_a_time_and_not_twice_in_a_row() {
        let mut app = app();
        app.spent_since_refresh = true;
        app.usage.current = Some(aged(60 * 60));
        assert!(app.usage_refresh_due());

        // An attempt just made rules out the next one, whatever the age says:
        // the stamp only moves when Claude Code decides to fetch.
        app.last_usage_refresh = Some(Instant::now());
        assert!(!app.usage_refresh_due());

        app.last_usage_refresh = None;
        app.usage_refreshing.store(true, Ordering::Relaxed);
        assert!(!app.usage_refresh_due());
    }

    #[test]
    fn the_session_doing_the_refreshing_is_not_shown_as_somebody_elses() {
        let mut app = app();
        app.registry = vec![RegistryEntry {
            pid: 4242,
            cwd: ".".into(),
            name: "usage".into(),
            status: "idle".into(),
            waiting_for: String::new(),
            started_at: 0,
            session_id: String::new(),
        }];
        assert_eq!(app.foreign().len(), 1);

        app.usage_refresh.store(4242, Ordering::Relaxed);
        assert!(app.foreign().is_empty());
    }

    #[test]
    fn an_abandoned_form_drops_its_armed_prompt() {
        let mut app = app();
        app.arm_understand_spawn();
        app.disarm_understand_spawn();
        assert!(!app.spawn_understand);
    }

    #[test]
    fn an_ordinary_new_session_form_clears_an_older_arming() {
        let mut app = app();
        app.arm_understand_spawn();
        app.open_new_session_form();
        assert!(!app.spawn_understand);
    }
}
