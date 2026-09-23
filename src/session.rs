//! A `claude` process running under a pseudo-terminal we own.
//!
//! Owning the PTY is what makes the pane fully interactive: keystrokes are
//! written straight to the child's stdin, and its output is fed through a
//! `vt100` parser whose screen the UI renders.

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::{bigbrother, dsr, keys};

const SCROLLBACK: usize = 5_000;

/// How much of a queued paste goes into one `write_all`. The child drains its
/// pipe at its own pace, so a megabyte in one call blocks until it catches up —
/// on the writer thread that is fine, on the UI thread it froze the whole fleet.
const WRITE_CHUNK: usize = 8 * 1024;

/// How long queued prompt text waits for the child's input box to appear
/// before it is handed over regardless.
///
/// The screen check that releases it is a heuristic, and a heuristic that
/// never matches would swallow the text for good; this deadline is what makes
/// the queue always drain.
const PROMPT_DEADLINE: Duration = Duration::from_secs(8);

/// How long after typed text the Enter that submits it follows.
///
/// Claude Code reads a burst of input as a paste, and an Enter arriving inside
/// that burst is taken as a newline in the text rather than the submit.
const SUBMIT_DELAY: Duration = Duration::from_millis(300);

/// How long after a resize what scrolls up is still the child reprinting
/// itself for the new size. It arrives within a few hundred milliseconds.
const RESIZE_SETTLE: Duration = Duration::from_secs(1);

/// The screen as it was before a resize, history and all, and when the size
/// last changed.
struct Resized {
    before: vt100::Screen,
    at: Instant,
}

/// Hands out `PtySession::uid`, which stays put while indices shift.
static NEXT_UID: AtomicU64 = AtomicU64::new(1);

/// Marks of Claude Code's input box, used to tell a painted prompt from a
/// child that has not drawn one yet.
///
/// The caret is the one that carries: current versions rule the input off with
/// plain horizontal lines and put `❯` in front of it, while older ones drew a
/// rounded box instead. Both are listed, so neither layout waits out the
/// deadline for nothing.
const PROMPT_CARET: char = '❯';
const BOX_BOTTOM_LEFT: char = '╰';
const BOX_BOTTOM_RIGHT: char = '╯';

/// Shared so the reader thread can answer terminal queries while the writer
/// thread pushes keystrokes and pastes.
type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

pub struct PtySession {
    /// Stable for the session's life. Indices move whenever a card closes;
    /// a Big Brother's event log and cursor need something that does not.
    pub uid: u64,
    pub label: String,
    /// The group a session belongs to, `a`-`z`. A Big Brother watches one
    /// group, or all of them.
    pub group: Option<char>,
    /// Set on a Big Brother: what it watches and the token it talks with.
    pub watch: Option<bigbrother::Watch>,
    pub cwd: PathBuf,
    /// A plain command shell rather than `claude`: it never registers, has
    /// no conversation to resume and no prompt box to type a prompt into.
    pub shell: bool,
    pub parser: Arc<RwLock<vt100::Parser>>,
    pub child_pid: Option<u32>,
    pub started: Instant,
    pub rows: u16,
    pub cols: u16,
    pub scrollback: usize,
    pub exited: Option<u32>,
    /// When the child was reaped, so finished panes can expire on their own.
    pub exited_at: Option<Instant>,
    /// Keystrokes and pastes leave the UI thread through here.
    input_tx: Sender<Vec<u8>>,
    /// Bytes handed to the writer thread but not yet pushed into the PTY.
    queued: Arc<AtomicUsize>,
    /// Set by the reader thread whenever the child writes something. The UI
    /// only redraws for it while this pane is the one on screen.
    output: Arc<AtomicBool>,
    /// Text waiting for the child to grow a prompt box to put it in.
    prompt_queue: Option<String>,
    prompt_since: Option<Instant>,
    /// Whether the queued text is submitted once typed, rather than left in
    /// the box for a human.
    prompt_submit: bool,
    /// When the Enter submitting typed text is due.
    enter_at: Option<Instant>,
    /// Set while a resize settles; see `settle_resize`.
    resized: Option<Resized>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

/// Resolve the `claude` executable, falling back to the standard install path
/// when it is not on PATH (common when launched from a GUI shell).
pub fn claude_binary() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        let local = home.join(".local").join("bin").join(if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        });
        if local.exists() {
            return local;
        }
    }
    PathBuf::from("claude")
}

/// The shell `s` opens: `%COMSPEC%` (cmd.exe) on Windows, `$SHELL` elsewhere.
pub fn shell_program() -> String {
    let (var, fallback) = if cfg!(windows) {
        ("COMSPEC", "cmd.exe")
    } else {
        ("SHELL", "/bin/sh")
    };
    std::env::var(var)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

impl PtySession {
    pub fn spawn(
        label: String,
        cwd: PathBuf,
        rows: u16,
        cols: u16,
        output: Arc<AtomicBool>,
        extra_args: &[String],
    ) -> Result<Self> {
        Self::spawn_with_env(label, cwd, rows, cols, output, extra_args, &[])
    }

    /// `spawn`, with variables set on the child on top of what it inherits.
    pub fn spawn_with_env(
        label: String,
        cwd: PathBuf,
        rows: u16,
        cols: u16,
        output: Arc<AtomicBool>,
        extra_args: &[String],
        env: &[(String, String)],
    ) -> Result<Self> {
        let mut cmd = CommandBuilder::new(claude_binary());
        for arg in extra_args {
            cmd.arg(arg);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        Self::spawn_command(label, cwd, rows, cols, output, cmd, false)
    }

    /// A command shell in `cwd`, under the same kind of PTY a session gets.
    pub fn spawn_shell(
        label: String,
        cwd: PathBuf,
        rows: u16,
        cols: u16,
        output: Arc<AtomicBool>,
    ) -> Result<Self> {
        let cmd = CommandBuilder::new(shell_program());
        Self::spawn_command(label, cwd, rows, cols, output, cmd, true)
    }

    fn spawn_command(
        label: String,
        cwd: PathBuf,
        rows: u16,
        cols: u16,
        output: Arc<AtomicBool>,
        mut cmd: CommandBuilder,
        shell: bool,
    ) -> Result<Self> {
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system()
            .openpty(size)
            .context("could not open a PTY")?;

        cmd.cwd(&cwd);
        // Claude Code renders 24-bit colour; announce a terminal that supports it.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let what = if shell { "the shell" } else { "`claude`" };
        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("could not start {what}"))?;
        // The slave handle must be dropped or the reader never sees EOF.
        drop(pair.slave);

        let child_pid = child.process_id();
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let writer: SharedWriter = Arc::new(Mutex::new(writer));
        let parser = Arc::new(RwLock::new(vt100::Parser::new(rows, cols, SCROLLBACK)));
        spawn_reader(
            reader,
            Arc::clone(&parser),
            Arc::clone(&writer),
            Arc::clone(&output),
        );

        let queued = Arc::new(AtomicUsize::new(0));
        let input_tx = spawn_writer(Arc::clone(&writer), Arc::clone(&queued));

        Ok(Self {
            uid: NEXT_UID.fetch_add(1, Ordering::Relaxed),
            label,
            group: None,
            watch: None,
            cwd,
            shell,
            parser,
            child_pid,
            started: Instant::now(),
            rows,
            cols,
            scrollback: 0,
            exited: None,
            exited_at: None,
            input_tx,
            queued,
            output,
            prompt_queue: None,
            prompt_since: None,
            prompt_submit: false,
            enter_at: None,
            resized: None,
            master: pair.master,
            child,
        })
    }

    /// Hand input to the writer thread. Never blocks: a paste of any size is
    /// queued, so the UI keeps redrawing and `F10` keeps working while the PTY
    /// swallows it.
    pub fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        // Any keystroke means the user wants to see the live tail, not history.
        if self.scrollback != 0 {
            self.set_scrollback(0);
        }
        self.queued.fetch_add(bytes.len(), Ordering::Relaxed);
        self.input_tx
            .send(bytes.to_vec())
            .map_err(|_| anyhow::anyhow!("the PTY writer is gone"))?;
        Ok(())
    }

    /// Type text into the child's prompt box as soon as it has one.
    ///
    /// A session spawned a moment ago is still painting its first frame and
    /// drops anything written before its input box exists, so the text waits
    /// here and `flush_prompt` hands it over once the screen says the box is
    /// up. Nothing submits it: the text lands in the box and stays there.
    pub fn queue_prompt(&mut self, text: &str) {
        self.prompt_queue = Some(text.to_string());
        self.prompt_since = Some(Instant::now());
        self.prompt_submit = false;
    }

    /// Type text into the prompt box and press Enter after it: a message
    /// sent, not a suggestion left for a human to send.
    pub fn queue_submit(&mut self, text: &str) {
        self.queue_prompt(text);
        self.prompt_submit = true;
    }

    /// True while queued text, or the Enter after it, has not reached the
    /// child yet.
    pub fn prompt_pending(&self) -> bool {
        self.prompt_queue.is_some() || self.enter_at.is_some()
    }

    /// Whether the child looks ready to take typed text. Claude Code draws a
    /// rounded box around its input, so its bottom border showing up is the
    /// cheapest signal that there is a prompt to type into.
    fn prompt_box_up(&self) -> bool {
        self.parser
            .read()
            .map(|p| {
                let screen = p.screen().contents();
                screen.contains(PROMPT_CARET)
                    || screen.contains(BOX_BOTTOM_RIGHT)
                    || screen.contains(BOX_BOTTOM_LEFT)
                    || screen.contains("for shortcuts")
            })
            .unwrap_or(false)
    }

    /// Hand queued prompt text to the child once it can take it.
    pub fn flush_prompt(&mut self) {
        if let Some(at) = self.enter_at
            && Instant::now() >= at
        {
            self.enter_at = None;
            if self.is_alive() {
                let _ = self.write_passthrough(b"\r");
            }
        }
        if self.prompt_queue.is_none() {
            return;
        }
        if !self.is_alive() {
            // Nowhere to type it. Keeping it queued would only leave the card
            // promising a prompt that is never coming.
            self.prompt_queue = None;
            self.prompt_since = None;
            return;
        }
        let waited = self.prompt_since.map(|t| t.elapsed()).unwrap_or_default();
        if !self.prompt_box_up() && waited < PROMPT_DEADLINE {
            return;
        }
        let Some(text) = self.prompt_queue.take() else {
            return;
        };
        self.prompt_since = None;
        // Several lines typed plainly would submit at the first newline, so
        // they go in as one paste when the child understands those.
        let bytes = if text.contains('\n') && self.bracketed_paste() {
            keys::encode_paste_chunk(&text, true, true, true)
        } else {
            text.into_bytes()
        };
        let _ = self.write_input(&bytes);
        if std::mem::take(&mut self.prompt_submit) {
            self.enter_at = Some(Instant::now() + SUBMIT_DELAY);
        }
    }

    /// The screen as text, the way someone looking at the pane would read it.
    pub fn screen_text(&self) -> String {
        self.parser
            .read()
            .map(|p| p.screen().contents())
            .unwrap_or_default()
    }

    /// Bytes still waiting to reach the child, so a long paste can say so.
    pub fn queued_input(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 || (rows == self.rows && cols == self.cols) {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut p) = self.parser.write() {
            if !self.shell {
                // The first change of a drag keeps the history as it stood;
                // the ones after it only push the deadline out.
                let before = match self.resized.take() {
                    Some(r) => r.before,
                    None => p.screen().clone(),
                };
                self.resized = Some(Resized {
                    before,
                    at: Instant::now(),
                });
            }
            p.screen_mut().set_size(rows, cols);
        }
    }

    /// Once a resize has settled, puts the history back as it was before it,
    /// under what the pane shows now. Returns whether the pane changed.
    ///
    /// Claude Code, through ConPTY, answers a new size by printing the tail
    /// of its transcript again, laid out for the new width, and that reprint
    /// scrolls up into the history. It is a copy of rows already there, and
    /// after a narrow-then-wide round trip it left the history wrapped at the
    /// narrow width. What scrolls up in the moments after a resize is that
    /// copy, so it is dropped; the history keeps the layout it was printed in.
    /// A shell streams output of its own, which must not be lost this way.
    pub fn settle_resize(&mut self) -> bool {
        match &self.resized {
            Some(r) if r.at.elapsed() >= RESIZE_SETTLE => {}
            _ => return false,
        }
        let Some(r) = self.resized.take() else {
            return false;
        };
        let Ok(mut p) = self.parser.write() else {
            return false;
        };
        drop_reprint(&mut p, r.before);
        p.screen_mut().set_scrollback(self.scrollback);
        self.scrollback = p.screen().scrollback();
        true
    }

    pub fn set_scrollback(&mut self, lines: usize) {
        let lines = lines.min(SCROLLBACK);
        self.scrollback = lines;
        if let Ok(mut p) = self.parser.write() {
            p.screen_mut().set_scrollback(lines);
        }
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let next = (self.scrollback as isize + delta).max(0) as usize;
        self.set_scrollback(next);
    }

    /// True while the child is still running. Also reaps it, so the exit code
    /// is available afterwards.
    pub fn poll_alive(&mut self) -> bool {
        if self.exited.is_some() {
            return false;
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.exited = Some(status.exit_code());
                self.exited_at = Some(Instant::now());
                false
            }
            Ok(None) => true,
            // Treat an un-waitable child as gone rather than looping on it.
            Err(_) => {
                self.exited = Some(u32::MAX);
                self.exited_at = Some(Instant::now());
                false
            }
        }
    }

    /// Whether the child has written anything since the last call.
    pub fn take_output(&self) -> bool {
        self.output.swap(false, Ordering::Relaxed)
    }

    pub fn is_alive(&self) -> bool {
        self.exited.is_none()
    }

    /// How long this pane has been dead, for the expiry countdown.
    pub fn finished_for(&self) -> Option<Duration> {
        self.exited_at.map(|at| at.elapsed())
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// The application-cursor mode decides whether arrow keys are encoded as
    /// CSI or SS3, so key encoding has to ask the screen.
    pub fn application_cursor(&self) -> bool {
        self.parser
            .read()
            .map(|p| p.screen().application_cursor())
            .unwrap_or(false)
    }

    /// Whether the child asked to be told about the mouse, and in which
    /// encoding. A child that did owns its own scrolling: the wheel has to be
    /// forwarded to it instead of moving the emulator's scrollback.
    pub fn mouse_reporting(&self) -> Option<vt100::MouseProtocolEncoding> {
        self.parser
            .read()
            .ok()
            .filter(|p| p.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None)
            .map(|p| p.screen().mouse_protocol_encoding())
    }

    /// The text between two visible cells, both inclusive, as `(row, col)`.
    /// Trailing blanks are the empty rest of a row, not something anyone
    /// meant to copy.
    pub fn text_between(&self, start: (u16, u16), end: (u16, u16)) -> String {
        let Ok(p) = self.parser.read() else {
            return String::new();
        };
        let text = p
            .screen()
            .contents_between(start.0, start.1, end.0, end.1.saturating_add(1));
        text.lines().map(str::trim_end).collect::<Vec<_>>().join("
")
    }

    /// Hand bytes to the child without touching the scroll position. A wheel
    /// notch is not a keystroke: it must not yank the view back to the tail.
    pub fn write_passthrough(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.queued.fetch_add(bytes.len(), Ordering::Relaxed);
        self.input_tx
            .send(bytes.to_vec())
            .map_err(|_| anyhow::anyhow!("the PTY writer is gone"))?;
        Ok(())
    }

    pub fn bracketed_paste(&self) -> bool {
        self.parser
            .read()
            .map(|p| p.screen().bracketed_paste())
            .unwrap_or(false)
    }

    /// Whether the cursor sits at the very start of the input box, where a
    /// left arrow has nowhere further to go.
    pub fn cursor_at_prompt_start(&self) -> bool {
        let shell = self.shell;
        self.parser
            .read()
            .map(|p| {
                if shell {
                    shell_cursor_at_start(p.screen())
                } else {
                    cursor_at_prompt_start(p.screen())
                }
            })
            .unwrap_or(false)
    }

    /// Whether the cursor sits past the last character of the input box,
    /// where a right arrow has nowhere further to go.
    pub fn cursor_at_prompt_end(&self) -> bool {
        let shell = self.shell;
        self.parser
            .read()
            .map(|p| {
                if shell {
                    shell_cursor_at_end(p.screen())
                } else {
                    cursor_at_prompt_end(p.screen())
                }
            })
            .unwrap_or(false)
    }

    pub fn cwd_label(&self) -> String {
        self.cwd
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.cwd.display().to_string())
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// The single place PTY input is written from. Chunking keeps one huge paste
/// from monopolising the writer lock, which is what lets the reader thread slip
/// its DSR replies in between chunks.
fn spawn_writer(writer: SharedWriter, queued: Arc<AtomicUsize>) -> Sender<Vec<u8>> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        while let Ok(bytes) = rx.recv() {
            for chunk in bytes.chunks(WRITE_CHUNK) {
                let Ok(mut w) = writer.lock() else { return };
                if w.write_all(chunk).is_err() || w.flush().is_err() {
                    // The child is gone; drop the rest instead of spinning.
                    queued.store(0, Ordering::Relaxed);
                    return;
                }
                queued.fetch_sub(chunk.len(), Ordering::Relaxed);
            }
        }
    });
    tx
}

/// Put `before`'s history behind what `p` shows now, dropping whatever
/// scrolled up since `before` was taken.
fn drop_reprint(p: &mut vt100::Parser, mut before: vt100::Screen) {
    // Either screen on the alternate buffer: its main one is not what is shown,
    // and the two could not be told apart anyway.
    if before.alternate_screen() || p.screen().alternate_screen() {
        return;
    }
    let (rows, cols) = p.screen().size();
    p.screen_mut().set_scrollback(0);
    let now = p.screen().state_formatted();
    before.set_scrollback(0);
    before.set_size(rows, cols);
    *p.screen_mut() = before;
    // Erasing the screen leaves the history alone, so what `before` showed
    // does not end up in it.
    p.process(b"\x1b[H\x1b[2J");
    p.process(&now);
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    parser: Arc<RwLock<vt100::Parser>>,
    writer: SharedWriter,
    dirty: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        // Carries the tail of the previous read so a control sequence split
        // across two reads is still recognised.
        let mut pending: Vec<u8> = Vec::new();

        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let chunk = &buf[..n];

            // The cursor position must be read after the chunk is applied, so
            // process first and answer second.
            let cursor = match parser.write() {
                Ok(mut p) => {
                    p.process(chunk);
                    p.screen().cursor_position()
                }
                Err(_) => break,
            };

            pending.extend_from_slice(chunk);
            let (queries, consumed) = dsr::scan(&pending);
            for q in queries {
                if let Ok(mut w) = writer.lock() {
                    let _ = w.write_all(&q.reply(cursor));
                    let _ = w.flush();
                }
            }

            // Drop what was fully scanned, then keep only enough trailing bytes
            // to complete a sequence that straddles the next read.
            pending.drain(..consumed.min(pending.len()));
            if pending.len() > dsr::TAIL {
                let excess = pending.len() - dsr::TAIL;
                pending.drain(..excess);
            }

            dirty.store(true, Ordering::Relaxed);
        }
        // One last redraw so the pane shows the child's final output.
        dirty.store(true, Ordering::Relaxed);
    });
}

/// A short, unique-ish label derived from a working directory, matching the way
/// Claude Code derives its own session names.
pub fn label_for(cwd: &Path, taken: &[String]) -> String {
    let base = cwd
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "claude".to_string());

    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|c| !taken.contains(c))
        .expect("an unbounded range always finds a free name")
}

/// Where Claude Code's cursor is, if the screen shows one on a prompt row.
///
/// Claude Code may either park the terminal's own cursor in the input or hide
/// it and paint the cursor as an inverse-video cell, so both are looked for.
/// Only rows carrying the prompt caret are searched for the painted one: a
/// cursor anywhere else is not at the start of the input anyway.
fn prompt_cursor(screen: &vt100::Screen) -> Option<(u16, u16)> {
    if !screen.hide_cursor() {
        return Some(screen.cursor_position());
    }
    let (rows, cols) = screen.size();
    (0..rows).find_map(|row| {
        let caret = (0..cols).find(|&c| {
            screen
                .cell(row, c)
                .is_some_and(|cell| cell.contents() == PROMPT_CARET.to_string())
        })?;
        (caret + 1..cols)
            .find(|&c| screen.cell(row, c).is_some_and(|cell| cell.inverse()))
            .map(|c| (row, c))
    })
}

/// True when everything left of the cursor on its row is the prompt caret
/// and the blanks or box border around it — the first position of the input.
///
/// A continuation row of a multi-line prompt has no caret in front of it, and
/// a left arrow there wraps to the line above, so it does not count.
fn cursor_at_prompt_start(screen: &vt100::Screen) -> bool {
    // A scrolled-back view does not show the rows the cursor refers to.
    if screen.scrollback() > 0 {
        return false;
    }
    let Some((row, col)) = prompt_cursor(screen) else {
        return false;
    };
    let before: String = (0..col)
        .filter_map(|c| screen.cell(row, c))
        .map(|cell| cell.contents())
        .collect();
    let rest = before.trim_matches(|ch: char| ch.is_whitespace() || ch == '│');
    rest == PROMPT_CARET.to_string() || rest == ">"
}

/// A cell that holds no input: empty, blank, the box border, or dim — the
/// placeholder and suggestion text Claude paints into an empty prompt.
fn blank_cell(cell: &vt100::Cell) -> bool {
    cell.dim()
        || cell
            .contents()
            .trim_matches(|ch: char| ch.is_whitespace() || ch == '│')
            .is_empty()
}

/// A horizontal rule or box corner: the edge of the input box.
fn rule_row(screen: &vt100::Screen, row: u16) -> bool {
    let (_, cols) = screen.size();
    (0..cols)
        .filter_map(|c| screen.cell(row, c))
        .map(|cell| cell.contents())
        .find(|s| !s.trim().is_empty())
        .and_then(|s| s.chars().next())
        .is_some_and(|ch| matches!(ch, '─' | '╭' | '╰'))
}

/// True when nothing of the input follows the cursor: the rest of its row is
/// blank, and so is every row below it down to the edge of the input box.
///
/// The cursor must be in the input itself — on the caret's row or on a
/// continuation row under it.
fn cursor_at_prompt_end(screen: &vt100::Screen) -> bool {
    if screen.scrollback() > 0 {
        return false;
    }
    let Some((row, col)) = prompt_cursor(screen) else {
        return false;
    };
    let (rows, cols) = screen.size();
    let row_blank_from = |r: u16, from: u16| {
        (from..cols)
            .filter_map(|c| screen.cell(r, c))
            .all(blank_cell)
    };

    let caret_above = (0..=row)
        .rev()
        .take_while(|&r| r == row || !rule_row(screen, r))
        .any(|r| {
            (0..cols).any(|c| {
                screen
                    .cell(r, c)
                    .is_some_and(|cell| cell.contents() == PROMPT_CARET.to_string())
            })
        });
    if !caret_above || !row_blank_from(row, col) {
        return false;
    }
    (row + 1..rows)
        .take_while(|&r| !rule_row(screen, r))
        .all(|r| row_blank_from(r, 0))
}

/// The column just past a shell prompt at the start of `row`: `C:\dir>`
/// (cmd), `PS C:\dir> ` (PowerShell), or up to the first `$ `, `# `, `% ` or
/// `> ` (a POSIX shell). `None` when the row does not start with one — output,
/// or the second row of a long command.
fn shell_prompt_end(screen: &vt100::Screen, row: u16) -> Option<u16> {
    let (_, cols) = screen.size();
    // One char per column, so an index into this is a column.
    let line: Vec<char> = (0..cols)
        .map(|c| {
            screen
                .cell(row, c)
                .and_then(|cell| cell.contents().chars().next())
                .unwrap_or(' ')
        })
        .collect();
    let windows = line.starts_with(&['P', 'S', ' '])
        || (line.len() > 2 && line[0].is_ascii_alphabetic() && line[1] == ':' && line[2] == '\\')
        || line.starts_with(&['\\', '\\']);
    let end = if windows {
        line.iter().position(|&ch| ch == '>')?
    } else {
        line.windows(2)
            .position(|w| matches!(w[0], '$' | '#' | '%' | '>') && w[1] == ' ')?
    };
    Some(end as u16 + 1)
}

/// The shell's cursor sits on a prompt row with nothing typed before it.
/// A full-screen program (an editor, a pager) owns the arrows, so its
/// alternate screen never counts.
fn shell_cursor_at_start(screen: &vt100::Screen) -> bool {
    if screen.scrollback() > 0 || screen.alternate_screen() {
        return false;
    }
    let (row, col) = screen.cursor_position();
    let Some(end) = shell_prompt_end(screen, row) else {
        return false;
    };
    col >= end
        && (end..col)
            .filter_map(|c| screen.cell(row, c))
            .all(|cell| cell.contents().trim().is_empty())
}

/// The shell's cursor sits on a prompt row past everything typed on it.
fn shell_cursor_at_end(screen: &vt100::Screen) -> bool {
    if screen.scrollback() > 0 || screen.alternate_screen() {
        return false;
    }
    let (row, col) = screen.cursor_position();
    let (rows, cols) = screen.size();
    let blank_from = |r: u16, from: u16| {
        (from..cols)
            .filter_map(|c| screen.cell(r, c))
            .all(|cell| cell.contents().trim().is_empty())
    };
    shell_prompt_end(screen, row).is_some_and(|end| col >= end)
        && blank_from(row, col)
        && (row + 1..rows).all(|r| blank_from(r, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(bytes: &str) -> vt100::Parser {
        let mut p = vt100::Parser::new(10, 40, 0);
        p.process(bytes.as_bytes());
        p
    }

    #[test]
    fn a_reprint_after_a_resize_leaves_the_history() {
        let mut p = vt100::Parser::new(3, 20, SCROLLBACK);
        p.process(b"old 1\r\nold 2\r\nold 3\r\nold 4\r\nold 5");
        let before = p.screen().clone();
        p.screen_mut().set_size(3, 10);
        // The child prints itself again for the new width, and the copy
        // scrolls up.
        p.process(b"\r\nold 1\r\nold 2\r\nold 3\r\nold 4\r\n\x1b[1mold\x1b[m 5");
        drop_reprint(&mut p, before);
        assert_eq!(p.screen().size(), (3, 10));
        assert_eq!(p.screen().contents(), "old 3\nold 4\nold 5");
        assert_eq!(p.screen().cursor_position(), (2, 5));
        assert!(p.screen().cell(2, 0).unwrap().bold());
        p.screen_mut().set_scrollback(usize::MAX);
        assert_eq!(p.screen().scrollback(), 2);
        assert_eq!(p.screen().contents(), "old 1\nold 2\nold 3");
    }

    #[test]
    fn a_shell_prompt_bounds_the_arrows() {
        // cmd: cursor right after `>`, at the start and the end alike.
        let p = screen(r"C:\Users\p>");
        assert!(shell_cursor_at_start(p.screen()));
        assert!(shell_cursor_at_end(p.screen()));
        // Something typed: past it is the end, not the start.
        let p = screen(r"C:\Users\p>dir");
        assert!(!shell_cursor_at_start(p.screen()));
        assert!(shell_cursor_at_end(p.screen()));
        // In the middle of it: neither.
        let p = screen("C:\\Users\\p>dir\x1b[2D");
        assert!(!shell_cursor_at_start(p.screen()));
        assert!(!shell_cursor_at_end(p.screen()));
        // PowerShell and a POSIX shell, with the blank after the prompt.
        assert!(shell_cursor_at_start(screen(r"PS C:\x> ").screen()));
        assert!(shell_cursor_at_start(screen("me@box:~$ ").screen()));
        assert!(!shell_cursor_at_start(screen("me@box:~$ ls").screen()));
        // Output of a running program is not a prompt.
        assert!(!shell_cursor_at_end(screen("building...").screen()));
        // Nor is anything on the alternate screen.
        assert!(!shell_cursor_at_end(screen("\x1b[?1049hC:\\x>").screen()));
    }

    #[test]
    fn a_shell_runs_what_is_typed_into_it() {
        let dir = std::env::temp_dir();
        let output = Arc::new(AtomicBool::new(false));
        let mut s = PtySession::spawn_shell("sh".into(), dir, 24, 80, output).unwrap();
        assert!(s.shell);
        s.write_input(b"echo fleet-$((6*7))-%OS%\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let seen = loop {
            let text = s.parser.read().unwrap().screen().contents();
            // cmd.exe expands `%OS%`, a POSIX shell the arithmetic.
            if text.contains("-Windows_NT") || text.contains("fleet-42") {
                break true;
            }
            if Instant::now() > deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(100));
        };
        s.kill();
        assert!(seen, "the shell never ran the echo");
    }

    #[test]
    fn empty_prompt_is_at_the_start() {
        let p = screen("\x1b[5;1H❯ ");
        assert!(cursor_at_prompt_start(p.screen()));
    }

    #[test]
    fn text_before_the_cursor_is_not_the_start() {
        let p = screen("\x1b[5;1H❯ hello");
        assert!(!cursor_at_prompt_start(p.screen()));
        let p = screen("\x1b[5;1H❯ hello\x1b[5;3H");
        assert!(cursor_at_prompt_start(p.screen()));
    }

    #[test]
    fn old_boxed_prompt_counts() {
        let p = screen("\x1b[5;1H│ > ");
        assert!(cursor_at_prompt_start(p.screen()));
    }

    #[test]
    fn continuation_row_is_not_the_start() {
        let p = screen("\x1b[5;1H❯ first\r\n  ");
        assert!(!cursor_at_prompt_start(p.screen()));
    }

    #[test]
    fn end_of_input_is_the_end() {
        let p = screen("\x1b[4;1H────\x1b[5;1H❯ hello\x1b[6;1H────\x1b[5;8H");
        assert!(cursor_at_prompt_end(p.screen()));
        let p = screen("\x1b[4;1H────\x1b[5;1H❯ hello\x1b[6;1H────\x1b[5;5H");
        assert!(!cursor_at_prompt_end(p.screen()));
    }

    #[test]
    fn a_later_row_of_input_is_not_the_end() {
        let p = screen("\x1b[4;1H────\x1b[5;1H❯ first\r\n  second\r\n────\x1b[5;8H");
        assert!(!cursor_at_prompt_end(p.screen()));
        let p = screen("\x1b[4;1H────\x1b[5;1H❯ first\r\n  second\r\n────\x1b[6;9H");
        assert!(cursor_at_prompt_end(p.screen()));
    }

    #[test]
    fn placeholder_does_not_count_as_input() {
        let p = screen("\x1b[5;1H❯ \x1b[2mTry something\x1b[22m\x1b[5;3H");
        assert!(cursor_at_prompt_end(p.screen()));
    }

    #[test]
    fn painted_cursor_is_found_when_the_real_one_is_hidden() {
        let p = screen("\x1b[?25l\x1b[5;1H❯ \x1b[7mh\x1b[27mello\x1b[9;1H");
        assert!(cursor_at_prompt_start(p.screen()));
        let p = screen("\x1b[?25l\x1b[5;1H❯ he\x1b[7ml\x1b[27mlo\x1b[9;1H");
        assert!(!cursor_at_prompt_start(p.screen()));
    }
}
