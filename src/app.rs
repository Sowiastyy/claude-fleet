//! Application state and the actions the key handler can trigger.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant, SystemTime},
};

use anyhow::Result;

use crate::{
    config, git,
    gitview::GitView,
    history,
    registry::{self, RegistryEntry},
    session::{label_for, PtySession},
    supervise, update, usage,
};

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

/// How often GitHub is asked about a newer release. Every push to main is a
/// release, so they can come hours apart; an unauthenticated client gets sixty
/// API calls an hour, and this spends two.
const UPDATE_CHECK: Duration = Duration::from_secs(30 * 60);

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

pub struct NewSessionForm {
    pub input: String,
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
        (self.cursor > 0).then(|| self.subdirs.get(self.cursor - 1)).flatten()
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
    /// Whether the git panel is on screen. Toggled with `g`.
    pub show_git: bool,
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
            show_git: true,
            git: None,
            git_busy: Arc::new(AtomicBool::new(false)),
            git_tx,
            git_rx,
            last_git_read: None,
            git_view: GitView::new(),
            git_fits: true,
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

    /// Spawn with arguments for the child. Today that is `--resume <id>` and
    /// nothing else: a session picked up where it was left is an ordinary
    /// session in every other respect, down to the understand prompt.
    pub fn spawn_session_with(&mut self, cwd: PathBuf, args: &[String]) -> Result<()> {
        if !cwd.is_dir() {
            self.notify(format!("no such directory: {}", cwd.display()));
            return Ok(());
        }
        let taken: Vec<String> = self.sessions.iter().map(|s| s.label.clone()).collect();
        let label = label_for(&cwd, &taken);

        let session = PtySession::spawn(
            label.clone(),
            cwd,
            self.pane_rows.max(4),
            self.pane_cols.max(20),
            Arc::clone(&self.dirty),
            args,
        )?;

        self.sessions.push(session);
        self.selected = self.sessions.len() - 1;
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
            .filter(|(_, s)| s.is_alive())
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
            .map(|i| supervise::Restore::new(self.sessions[i].cwd.clone(), ids[i].clone()))
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
        let origin = supervise::origin().ok_or("updates need the supervisor — start fleet normally")?;
        if update::is_dev_build(&origin) {
            return Err("fleet runs out of a cargo build — git pull and build instead");
        }
        Ok(origin)
    }

    /// Ask GitHub about a newer release, on a thread of its own.
    fn check_for_update(&mut self, asked: bool) {
        self.last_update_check = Some(Instant::now());
        if self.update_target().is_err() || self.update_busy.swap(true, Ordering::Relaxed) {
            return;
        }
        let busy = Arc::clone(&self.update_busy);
        let tx = self.update_tx.clone();
        let dirty = Arc::clone(&self.dirty);
        std::thread::spawn(move || {
            // No network is an ordinary state for a laptop, and nothing worth
            // a status line unless someone pressed `i` and is waiting to hear:
            // the next periodic check is half an hour away regardless.
            match update::check() {
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
        if !self.show_git && self.mode == Mode::Git {
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
        self.mode = Mode::Git;
        self.dirty.store(true, Ordering::Relaxed);
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
            back: if self.mode == Mode::Git { Mode::Git } else { Mode::Nav },
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
        if !self.show_git {
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
                    // Said once per release, not on every six-hour check.
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
            match item.session {
                Some(id) => {
                    self.spawn_session_with(item.cwd, &["--resume".to_string(), id])?;
                    resumed += 1;
                }
                None => self.spawn_session(item.cwd)?,
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
                format!("{n} sessions came back after the restart, conversations resumed: {resumed}")
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
        (at.elapsed() <= config::understand_window() && idx < self.sessions.len())
            .then_some(idx)
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
        let label = s.label.clone();
        let prompt = config::understand_prompt();
        s.queue_prompt(&prompt);
        self.selected = idx;
        self.mode = Mode::Focus;
        self.notify(format!("{label}: \"{prompt}\" is in the prompt — enter sends it"));
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
            self.form = None;
            self.mode = Mode::Nav;
            return self.spawn_session(cwd);
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
        self.form = None;
        self.mode = Mode::Nav;
        // `spawn_session` reports the session it started; saying "utworzono"
        // here would only be overwritten by it a moment later.
        self.spawn_session(path)
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

    /// Per-frame bookkeeping: reap dead children, drop expired cards, refresh
    /// the registry, expire the status line.
    pub fn tick(&mut self) {
        // Reaps children and records exit codes; the return value is read via
        // `is_alive` during render.
        for s in &mut self.sessions {
            s.poll_alive();
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
        match config::reload_if_changed() {
            Some(config::Reload::Applied) => self.notify("config reloaded"),
            Some(config::Reload::Failed(e)) => self.notify(format!("config rejected: {e}")),
            None => {}
        }

        if !self.update_ready && self.last_exe_check.elapsed() >= EXE_CHECK {
            self.last_exe_check = Instant::now();
            let now = supervise::origin_stamp();
            if now.is_some() && now != self.origin_stamp {
                self.update_ready = true;
                self.notify("new build ready — r restarts the fleet");
            }
        }

        self.poll_update();
        self.poll_git();

        if self.last_registry_scan.elapsed() >= REGISTRY_REFRESH {
            self.registry = registry::read_all();
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
            if self.usage_refresh_due() {
                self.refresh_usage();
            }
        }

        if let Some((_, at)) = &self.status
            && at.elapsed() > Duration::from_secs(4) {
                self.status = None;
                self.dirty.store(true, Ordering::Relaxed);
            }
    }
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
