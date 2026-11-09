//! The limits reader lives in `fleet_core::usage`; what is here is the part
//! that needs a PTY: asking Claude Code for fresh numbers by driving a hidden
//! session (see [`refresh_via_claude`]).

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};

pub use fleet_core::usage::*;

/// What the hidden session is told to run. A slash command, so it costs no
/// tokens: Claude Code answers it itself, and refreshing the cache is what it
/// does on the way.
const REFRESH_COMMAND: &str = "/usage";

/// How long the hidden session gets before it is given up on and killed.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(12);

/// What one attempt did.
pub struct Refresh {
    /// Whether `fetchedAtMs` moved, i.e. whether the numbers are actually new.
    ///
    /// False is an ordinary outcome, not an error: Claude Code serves `/usage`
    /// from numbers of its own for a few minutes before it asks again, so an
    /// attempt against a cache that is already recent answers from that and
    /// leaves the stamp alone.
    pub moved: bool,
    pub waited: Duration,
    /// What the hidden session had on screen when it was killed. Only worth
    /// looking at when nothing moved.
    pub screen: String,
}

/// Ask Claude Code to put fresh numbers in the cache, by driving a session of
/// its own the way a person would.
///
/// Fleet stays a reader: it does not know the endpoint, it holds no token and
/// it writes nothing into the file. It spawns `claude` under a PTY exactly as
/// it spawns any session, types `/usage`, and waits for the file to move. The
/// child is killed as soon as it has, so the whole thing lasts a few seconds.
///
/// `-p "/usage"` would be cheaper, but print mode answers on stdout without
/// touching the cache, which would leave fleet parsing prose for numbers it can
/// otherwise read as JSON with absolute reset times in it.
///
/// `pid_slot` carries the child's pid out while it runs, so the session list can
/// leave it out rather than showing a session nobody started. It is zero when no
/// refresh is in flight.
///
/// Returns whether the file actually moved.
pub fn refresh_via_claude(cwd: &Path, pid_slot: &AtomicU32) -> Result<Refresh> {
    let path = cache_path().context("no home directory to find the cache in")?;
    // The file's mtime is the wrong thing to watch: Claude Code writes to it for
    // its own reasons — a starting session alone moves it within a second — so
    // an mtime that moved says nothing about the numbers. `fetchedAtMs` is
    // stamped by the fetch itself, and only by it.
    let before = fetched_at(&path);

    let mut session = crate::session::PtySession::spawn(
        "usage".into(),
        cwd.to_path_buf(),
        24,
        80,
        Arc::new(AtomicBool::new(false)),
        &[],
    )?;
    if let Some(pid) = session.child_pid {
        pid_slot.store(pid, Ordering::Relaxed);
    }

    let started = Instant::now();
    let moved = drive(&mut session, &path, before, started + REFRESH_TIMEOUT);

    let screen = session
        .parser
        .read()
        .map(|p| p.screen().contents())
        .unwrap_or_default();
    session.kill();
    pid_slot.store(0, Ordering::Relaxed);

    Ok(Refresh {
        moved: moved?,
        waited: started.elapsed(),
        screen,
    })
}

/// Type the command into the child and watch the file until it moves.
fn drive(
    session: &mut crate::session::PtySession,
    path: &Path,
    before: Option<u64>,
    deadline: Instant,
) -> Result<bool> {
    // The same wait any typed text goes through: a session spawned a moment ago
    // has no prompt box yet and drops whatever is written before it has one.
    session.queue_prompt(REFRESH_COMMAND);
    while session.prompt_pending() && Instant::now() < deadline {
        session.poll_alive();
        session.flush_prompt();
        std::thread::sleep(Duration::from_millis(50));
    }
    if session.prompt_pending() {
        anyhow::bail!("the hidden session never grew a prompt box");
    }
    // The text leaves through the writer thread, so the newline that submits it
    // has to wait for the queue behind it to drain.
    while session.queued_input() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    session.write_input(b"\r")?;

    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        let now = fetched_at(path);
        if now.is_some() && now != before {
            return Ok(true);
        }
    }
    Ok(false)
}

