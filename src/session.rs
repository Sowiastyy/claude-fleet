//! A `claude` process running under a pseudo-terminal we own.
//!
//! Owning the PTY is what makes the pane fully interactive: keystrokes are
//! written straight to the child's stdin, and its output is fed through a
//! `vt100` parser whose screen the UI renders.

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Sender},
        Arc, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use crate::dsr;

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
    pub label: String,
    pub cwd: PathBuf,
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
    /// Text waiting for the child to grow a prompt box to put it in.
    prompt_queue: Option<String>,
    prompt_since: Option<Instant>,
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

impl PtySession {
    pub fn spawn(
        label: String,
        cwd: PathBuf,
        rows: u16,
        cols: u16,
        dirty: Arc<AtomicBool>,
        extra_args: &[String],
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

        let mut cmd = CommandBuilder::new(claude_binary());
        for arg in extra_args {
            cmd.arg(arg);
        }
        cmd.cwd(&cwd);
        // Claude Code renders 24-bit colour; announce a terminal that supports it.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("could not start `claude`")?;
        // The slave handle must be dropped or the reader never sees EOF.
        drop(pair.slave);

        let child_pid = child.process_id();
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let writer: SharedWriter = Arc::new(Mutex::new(writer));
        let parser = Arc::new(RwLock::new(vt100::Parser::new(rows, cols, SCROLLBACK)));
        spawn_reader(reader, Arc::clone(&parser), Arc::clone(&writer), dirty);

        let queued = Arc::new(AtomicUsize::new(0));
        let input_tx = spawn_writer(Arc::clone(&writer), Arc::clone(&queued));

        Ok(Self {
            label,
            cwd,
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
            prompt_queue: None,
            prompt_since: None,
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
    }

    /// True while queued text has not reached the child yet.
    pub fn prompt_pending(&self) -> bool {
        self.prompt_queue.is_some()
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
        let _ = self.write_input(text.as_bytes());
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
            p.screen_mut().set_size(rows, cols);
        }
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
            .filter(|p| {
                p.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None
            })
            .map(|p| p.screen().mouse_protocol_encoding())
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
        self.parser
            .read()
            .map(|p| cursor_at_prompt_start(p.screen()))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(bytes: &str) -> vt100::Parser {
        let mut p = vt100::Parser::new(10, 40, 0);
        p.process(bytes.as_bytes());
        p
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
    fn painted_cursor_is_found_when_the_real_one_is_hidden() {
        let p = screen("\x1b[?25l\x1b[5;1H❯ \x1b[7mh\x1b[27mello\x1b[9;1H");
        assert!(cursor_at_prompt_start(p.screen()));
        let p = screen("\x1b[?25l\x1b[5;1H❯ he\x1b[7ml\x1b[27mlo\x1b[9;1H");
        assert!(!cursor_at_prompt_start(p.screen()));
    }
}
