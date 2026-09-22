//! claude-fleet: a terminal multiplexer for Claude Code sessions.
//!
//! Sessions the fleet spawns run under a PTY it owns, so their panes are fully
//! interactive. Sessions started elsewhere are listed read-only: their PTY
//! belongs to another terminal and cannot be adopted.

mod app;
mod bigbrother;
mod clipimg;
mod commitmsg;
mod config;
mod dsr;
mod git;
mod gitview;
mod history;
mod input;
mod keys;
mod msgedit;
mod registry;
mod repos;
mod session;
mod supervise;
mod theme;
mod ui;
mod update;
mod usage;

use std::{
    env,
    io::{self, Stdout},
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    app::{App, Drag, GitHit, Mode, SpawnKind},
    input::Input,
};

const FRAME_POLL: Duration = Duration::from_millis(16);
const SCROLL_STEP: isize = 3;

/// Above this many characters in one burst the input is a paste, not typing.
const BURST_MIN: usize = 8;
/// How long a burst is allowed to go quiet before it counts as finished.
///
/// A paste is not one uninterrupted stream of records: it comes in waves with
/// gaps between them. Ending the burst at the first empty queue chopped one
/// paste into dozens of pieces, and every piece was a prompt of its own —
/// anything the child reads outside paste brackets submits on its newlines.
const PASTE_GRACE: Duration = Duration::from_millis(60);
/// The same wait before a burst is long enough to be called a paste. It keeps
/// the opening characters together without making an echo feel sluggish.
const TYPING_GRACE: Duration = Duration::from_millis(3);
/// Cap on one coalesced burst, so a multi-megabyte paste is handed over in
/// pieces and the loop still gets to redraw between them.
const BURST_MAX: usize = 64 * 1024;

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("--list" | "-l") => return print_registry(),
        // The `fleet` command a Big Brother runs: a client of the fleet
        // that started it, never a TUI of its own.
        Some("bb") => {
            if let Err(e) = bigbrother::cli(&args[1..]) {
                eprintln!("fleet: {e:#}");
                std::process::exit(1);
            }
            return Ok(());
        }
        Some("--pipes") => {
            for p in registry::debug_pipe_names() {
                println!("{p}");
            }
            return Ok(());
        }
        Some("--selftest") => return selftest(args.get(1).map(String::as_str)),
        Some("--raw") => return raw_probe(&args[1..]),
        Some("--orphan-probe") => return orphan_probe(),
        Some("--mouse") => return mouse_probe(),
        Some("--usage") => return print_usage_limits(),
        Some("--history") => return print_history(),
        Some("--repos") => return print_repos(),
        Some("--help" | "-h") => {
            print_usage();
            return Ok(());
        }
        _ => {}
    }

    // The process the user started supervises; the TUI runs in a copy of it,
    // so a build can replace the binary underneath a running fleet.
    if !supervise::is_inner() {
        return supervise::supervise();
    }

    let cwd = args
        .first()
        .map(PathBuf::from)
        .or_else(|| env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    if let Some(path) = config::write_default_if_missing() {
        println!("wrote a default config: {}", path.display());
    }
    // A broken config is worth saying here, with the terminal still readable,
    // rather than as a status line under a screen the user just opened.
    let config_error = config::init().err();

    let mut terminal = setup_terminal().context("could not set the terminal up")?;
    let result = run(&mut terminal, cwd, config_error);
    restore_terminal(&mut terminal)?;

    let restart = result?;
    if restart {
        // The supervisor is waiting on exactly this code.
        std::process::exit(supervise::RESTART_EXIT);
    }
    Ok(())
}

fn print_usage() {
    println!(
        "claude-fleet — a multiplexer for Claude Code sessions\n\
         \n\
         USAGE:\n\
         \x20 claude-fleet [DIR]       run the TUI (default: current directory)\n\
         \x20 claude-fleet --list      print the running sessions and exit\n\
         \x20 claude-fleet bb help     commands a BIG BROTHER drives the fleet with\n\
         \x20 claude-fleet --help      this help\n\
         \n\
         DIAGNOSTICS:\n\
         \x20 --pipes                  names of the open cc-msg pipes\n\
         \x20 --selftest [ui]          run `claude` under a PTY and show its screen\n\
         \x20 --selftest understand    check that queued text reaches the prompt\n\
         \x20 --selftest config        whether a fleet.toml edit arrives live\n\
         \x20 --selftest source        which file fleet treats as the build output\n\
         \x20 --selftest restart       full restart: presses `r` and counts starts\n\
         \x20 --orphan-probe           does a child outlive its parent (it does not)\n\
         \x20 --raw <prog> [args...]   raw bytes from any program under a PTY\n\
         \x20 --mouse                  does this terminal hand over the mouse wheel\n\
         \x20 --usage                  account limits as the sidebar reads them\n\
         \x20 --history                conversations to resume, as the list reads them\n\
         \x20 --repos                  repositories the remote form offers\n"
    );
}

/// Dump the registry without touching the terminal, which makes the reader
/// testable outside a TTY.
fn print_registry() -> Result<()> {
    let entries = registry::read_all();
    if entries.is_empty() {
        println!("no Claude Code sessions are running");
        return Ok(());
    }
    println!("{:<24} {:<8} {:>8}  DIRECTORY", "NAME", "STATUS", "PID");
    for e in &entries {
        // `waiting` alone says nothing about what is being waited for, and
        // that is the whole point of the status.
        let status = if e.is_waiting() && !e.waiting_for.is_empty() {
            format!("{} ({})", e.status, e.waiting_for)
        } else {
            e.status.clone()
        };
        println!("{:<24} {:<8} {:>8}  {}", e.name, status, e.pid, e.cwd);
    }
    println!("\n{} sessions (all foreign to this process)", entries.len());
    Ok(())
}

/// Print the limits exactly as the sidebar reads them, so the reader can be
/// checked without a terminal in the way.
/// The repositories the remote form offers, and where each would run.
fn print_repos() -> Result<()> {
    let listing = repos::list();
    if let Some(note) = &listing.note {
        println!("({note})");
    }
    for r in &listing.repos {
        let at = match &r.local {
            Some(p) => p.display().to_string(),
            None => "clone on start".to_string(),
        };
        let private = if r.private { "private" } else { "" };
        println!("{:<44} {:<8} {at}", r.full_name, private);
    }
    println!("{} repositories", listing.repos.len());
    Ok(())
}

/// The resume list as the picker builds it. Answers "why is that conversation
/// not on the list" without opening the TUI.
fn print_history() -> Result<()> {
    let items = history::recent(20);
    if items.is_empty() {
        println!("no transcripts in ~/.claude/projects");
        return Ok(());
    }
    for c in items {
        let age = SystemTime::now()
            .duration_since(c.modified)
            .unwrap_or_default();
        println!(
            "{:<8} {:<20} {}",
            fmt_ago(age),
            clip(&c.cwd_label(), 20),
            clip(&c.summary, 58),
        );
        println!("{:>8} claude --resume {}", "", c.id);
    }
    Ok(())
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}~", s.chars().take(max - 1).collect::<String>())
}

fn print_usage_limits() -> Result<()> {
    let watch = usage::Watch::new();
    let Some(u) = watch.current else {
        println!("no limit cache in ~/.claude.json");
        return Ok(());
    };
    println!("refreshed {} ago", fmt_ago(u.fetched_ago));
    for (name, w) in [("session (5h)", u.session), ("weekly (7d)", u.weekly)] {
        match w {
            None => println!("{name:<14} no data"),
            Some(w) if w.expired => {
                println!("{:<14} {:>3}%  window has already reset", name, w.pct)
            }
            Some(w) => println!(
                "{:<14} {:>3}%  resets in {}",
                name,
                w.pct,
                w.resets_in.map(fmt_ago).unwrap_or_else(|| "?".into())
            ),
        }
    }
    Ok(())
}

fn fmt_ago(d: Duration) -> String {
    let mins = d.as_secs() / 60;
    match mins {
        0..=59 => format!("{mins}m"),
        60..=1439 => format!("{}h{:02}m", mins / 60, mins % 60),
        _ => format!("{}d{}h", mins / 1440, (mins % 1440) / 60),
    }
}

/// Does this terminal hand the wheel over at all?
///
/// Scrolling has two halves that fail the same way from the outside: a wheel
/// notch that never reaches us, and one that reaches us but moves a scrollback
/// the child does not keep. This settles the first half — twenty seconds of
/// whatever the terminal sends, counted by kind.
fn mouse_probe() -> Result<()> {
    use crossterm::event::{MouseEventKind, poll};

    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnableMouseCapture)?;
    // Raw mode means a bare newline does not return the carriage.
    print!("scroll the wheel for 20 s (Esc ends it sooner)\r\n");
    let _ = io::Write::flush(&mut io::stdout());

    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let (mut wheel, mut other_mouse, mut keys) = (0usize, 0usize, 0usize);
    while std::time::Instant::now() < deadline {
        if !poll(Duration::from_millis(200))? {
            continue;
        }
        match crossterm::event::read()? {
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    wheel += 1;
                    print!("wheel {:?} @ {},{}\r\n", m.kind, m.column, m.row);
                    let _ = io::Write::flush(&mut io::stdout());
                }
                _ => other_mouse += 1,
            },
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                keys += 1;
                if k.code == KeyCode::Esc {
                    break;
                }
            }
            _ => {}
        }
    }

    execute!(io::stdout(), DisableMouseCapture)?;
    disable_raw_mode()?;
    println!("wheel={wheel} other mouse events={other_mouse} keys={keys}");
    if wheel == 0 {
        println!(
            "No wheel events at all: this terminal does not hand them to the \
             application, so no panel will ever see them."
        );
    }
    Ok(())
}

/// Spawn an arbitrary command under a PTY and dump the raw bytes it writes.
/// Diagnostic only: it answers "what did the child actually send" without the
/// parser or the UI in the way.
fn raw_probe(args: &[String]) -> Result<()> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::{Read, Write};

    let Some((program, rest)) = args.split_first() else {
        anyhow::bail!("usage: claude-fleet --raw <program> [args...]");
    };

    let pair = native_pty_system().openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new(program);
    for a in rest {
        cmd.arg(a);
    }
    cmd.cwd(env::current_dir()?);

    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    // Same trap as the real panes: a child whose DSR goes unanswered stops at
    // four bytes, so the probe would only ever dump `ESC[6n`.
    let mut writer = pair.master.take_writer()?;

    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut pending: Vec<u8> = Vec::new();
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let chunk = &buf[..n];
            pending.extend_from_slice(chunk);
            let (queries, consumed) = dsr::scan(&pending);
            for q in queries {
                let _ = writer.write_all(&q.reply((0, 0)));
                let _ = writer.flush();
            }
            pending.drain(..consumed.min(pending.len()));
            if pending.len() > dsr::TAIL {
                let excess = pending.len() - dsr::TAIL;
                pending.drain(..excess);
            }
            if tx.send(chunk.to_vec()).is_err() {
                break;
            }
        }
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut total = Vec::new();
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(chunk) => total.extend_from_slice(&chunk),
            Err(_) => {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
            }
        }
    }

    println!("bytes={} exit={:?}", total.len(), child.try_wait()?);
    println!("--- escaped ---");
    println!("{}", String::from_utf8_lossy(&total).escape_debug());
    let _ = child.kill();
    Ok(())
}

/// Drive a whole restart: run the supervisor under a PTY, press `r`, and check
/// that the fleet came back.
///
/// Every other part of the restart has a unit test, but the chain from a
/// keypress through the exit code to a second run exists only end to end, and
/// a break anywhere along it looks the same from outside: a key that does
/// nothing.
fn restart_selftest() -> Result<()> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    let own = env::current_exe()?;
    let pair = native_pty_system().openpty(PtySize {
        rows: 24,
        cols: 100,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new(&own);
    cmd.cwd(env::current_dir()?);
    // The supervisor is what is being tested, so it must not start up thinking
    // it is already the inner process.
    cmd.env_remove(supervise::RUN_MARKER);
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = Arc::clone(&seen);
    let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
    let answerer = Arc::clone(&writer);
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut pending: Vec<u8> = Vec::new();
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            // Without answers to its DSR queries the child never gets going.
            let (queries, consumed) = dsr::scan(&pending);
            for q in queries {
                if let Ok(mut w) = answerer.lock() {
                    let _ = w.write_all(&q.reply((1, 1)));
                    let _ = w.flush();
                }
            }
            pending.drain(..consumed.min(pending.len()));
            if let Ok(mut s) = sink.lock() {
                s.extend_from_slice(&buf[..n]);
            }
        }
    });

    // One alternate screen per run of the inner process, which makes counting
    // them the plainest possible answer to "how many times did the fleet run".
    const ALT: &str = "[?1049h";
    let runs = || {
        seen.lock()
            .map(|s| String::from_utf8_lossy(&s).matches(ALT).count())
            .unwrap_or(0)
    };

    if !wait_for(|| runs() >= 1, Duration::from_secs(20)) {
        let _ = child.kill();
        anyhow::bail!("fleet did not start");
    }
    // Let the first frame settle before typing into it.
    std::thread::sleep(Duration::from_millis(1500));

    println!("pressed: r");
    if let Ok(mut w) = writer.lock() {
        w.write_all(b"r")?;
        w.flush()?;
    }

    let again = wait_for(|| runs() >= 2, Duration::from_secs(25));
    println!("fleet starts: {}", runs());

    // Close the fleet that came back, so the probe leaves nothing behind.
    if let Ok(mut w) = writer.lock() {
        let _ = w.write_all(b"q");
        let _ = w.flush();
    }
    std::thread::sleep(Duration::from_millis(500));
    let _ = child.kill();

    if !again {
        anyhow::bail!("fleet did not come back after `r`");
    }
    println!("restart works");
    Ok(())
}

/// Poll a condition until it holds or the deadline passes.
fn wait_for(mut done: impl FnMut() -> bool, limit: Duration) -> bool {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    done()
}

/// Say which binary this fleet treats as the build output, and what it would
/// hand the inner process. Answers "why does it not see my rebuild".
fn source_selftest() -> Result<()> {
    let own = env::current_exe()?;
    let source = supervise::resolve_source(&own);
    println!("exe:        {}", own.display());
    println!("source:     {}", source.display());
    println!(
        "stamp:      {:?}",
        std::fs::metadata(&source)
            .ok()
            .and_then(|m| m.modified().ok())
    );
    println!(
        "{}:  {:?}",
        supervise::BUILD_VAR,
        env::var_os(supervise::BUILD_VAR)
    );
    println!(
        "{}: {:?}",
        supervise::ORIGIN_VAR,
        env::var_os(supervise::ORIGIN_VAR)
    );
    println!(
        "{}:  {:?}",
        supervise::RUN_MARKER,
        env::var_os(supervise::RUN_MARKER)
    );
    match supervise::source_warning(&own, &source) {
        Some(w) => println!("warning: {w}"),
        None => println!("warning: none"),
    }
    Ok(())
}

/// Drive the hidden `/usage` session once and say whether the cache moved.
/// Answers "can fleet refresh the limits itself" without waiting for the panel
/// to decide the numbers are stale.
fn usage_selftest() -> Result<()> {
    use std::sync::atomic::AtomicU32;

    let watch = usage::Watch::new();
    match watch.current.as_ref() {
        Some(u) => println!("before: refreshed {} ago", fmt_ago(u.fetched_ago)),
        None => println!("before: no cache at all"),
    }

    let age_before = watch.current.as_ref().map(|u| u.fetched_ago);

    let pid = AtomicU32::new(0);
    let r = usage::refresh_via_claude(&env::current_dir()?, &pid)?;
    println!("cache moved={} after {:?}", r.moved, r.waited);

    let watch = usage::Watch::new();
    match watch.current.as_ref() {
        Some(u) => println!("after:  refreshed {} ago", fmt_ago(u.fetched_ago)),
        None => println!("after:  no cache at all"),
    }
    if !r.moved {
        // Claude Code answers `/usage` from numbers of its own for a few
        // minutes, so a cache that was recent to begin with is expected to sit
        // still. Only an old one that would not move is a failure.
        println!("--- hidden session screen ---");
        println!("{}", r.screen.trim_end());
        println!("--- end ---");
        if age_before.is_some_and(|a| a < TTL_GUESS) {
            println!(
                "numbers were only {} old; Claude Code answered from its own cache",
                fmt_ago(age_before.unwrap_or_default())
            );
            return Ok(());
        }
        anyhow::bail!("the cache did not move");
    }
    Ok(())
}

/// About how long Claude Code serves `/usage` from numbers it already has. Not
/// documented anywhere, so it is an observation, and it is only used to tell an
/// expected non-refresh from a broken one.
const TTL_GUESS: Duration = Duration::from_secs(5 * 60);

/// Watch the config file the way the running fleet does, and report every
/// reload it sees. Answers "is my edit reaching it at all" without a TUI in
/// the way.
fn config_selftest() -> Result<()> {
    println!("file: {}", config::config_path().display());
    if let Some(p) = config::write_default_if_missing() {
        println!("wrote the default: {}", p.display());
    }
    match config::init() {
        Ok(()) => println!("loaded, labels.starting={:?}", config::labels().starting),
        Err(e) => println!("not loaded: {e}"),
    }

    println!("watching the file for 10 s...");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut seen = 0;
    while std::time::Instant::now() < deadline {
        match config::reload_if_changed() {
            Some(config::Reload::Applied) => {
                seen += 1;
                println!(
                    "  reloaded: labels.starting={:?} ask={:?}",
                    config::labels().starting,
                    config::theme().ask
                );
            }
            Some(config::Reload::Failed(e)) => {
                seen += 1;
                println!("  rejected: {e}");
            }
            None => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("changes seen: {seen}");
    if seen == 0 {
        anyhow::bail!("no change to the file arrived");
    }
    Ok(())
}

/// Does a child under our ConPTY outlive the process that created it?
///
/// The answer decides whether sessions can be handed to a restarted fleet by
/// passing handles, or whether the PTY has to live somewhere that does not
/// restart. Spawns a long-running child, leaks every PTY object so nothing is
/// closed tidily, and exits: exactly what a process being replaced looks like.
/// Whoever runs it then checks whether the printed pid is still there.
fn orphan_probe() -> Result<()> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let pair = native_pty_system().openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new("ping");
    cmd.arg("-n");
    cmd.arg("60");
    cmd.arg("127.0.0.1");
    cmd.cwd(env::current_dir()?);

    let child = pair.slave.spawn_command(cmd)?;
    println!("child_pid={}", child.process_id().unwrap_or(0));
    drop(pair.slave);

    // No `ClosePseudoConsole`, no kill: the handles go only because the process
    // goes, which is the case being tested.
    std::mem::forget(pair.master);
    std::mem::forget(child);
    Ok(())
}

/// Exercise the PTY layer without a TTY: spawn a real `claude --version` under
/// a pseudo-terminal, let the reader thread feed the parser, and print what the
/// emulated screen ended up holding.
fn selftest(kind: Option<&str>) -> Result<()> {
    use std::sync::{Arc, atomic::AtomicBool};

    if kind == Some("config") {
        return config_selftest();
    }
    if kind == Some("source") {
        return source_selftest();
    }
    if kind == Some("restart") {
        return restart_selftest();
    }
    if kind == Some("usage") {
        return usage_selftest();
    }

    let interactive = matches!(kind, Some("ui" | "understand"));
    let understand = kind == Some("understand");

    let cwd = env::current_dir()?;
    let dirty = Arc::new(AtomicBool::new(false));
    let args: Vec<String> = if interactive {
        Vec::new()
    } else {
        vec!["--version".to_string()]
    };

    let mut s =
        session::PtySession::spawn("selftest".into(), cwd, 24, 80, Arc::clone(&dirty), &args)?;

    println!("spawned pid={:?} interactive={interactive}", s.child_pid);

    if understand {
        // Exactly what the `u` chord does: queue text against a child that has
        // no prompt box yet, and let the flush decide when it has one.
        let prompt = config::understand_prompt();
        s.queue_prompt(&prompt);
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while s.prompt_pending() && std::time::Instant::now() < deadline {
            s.poll_alive();
            s.flush_prompt();
            std::thread::sleep(Duration::from_millis(50));
        }
        println!(
            "prompt handed over after {:?}, queue empty={}",
            s.started.elapsed(),
            !s.prompt_pending()
        );
        // The writer thread and the child both need a moment before the text
        // turns up on the emulated screen.
        std::thread::sleep(Duration::from_secs(3));
    } else if interactive {
        // Let the real UI paint, then look at what the pane would show.
        std::thread::sleep(Duration::from_secs(12));
    } else {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while s.poll_alive() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        // Give the reader thread a moment to drain the last chunk after exit.
        std::thread::sleep(Duration::from_millis(200));
    }

    let contents = s
        .parser
        .read()
        .map(|p| p.screen().contents())
        .unwrap_or_default();

    println!("exit={:?}", s.exited);
    println!("dirty={}", dirty.load(Ordering::Relaxed));
    println!("--- PTY screen ---");
    println!("{}", contents.trim_end());
    println!("--- end ---");

    if understand && !contents.contains(&config::understand_prompt()) {
        anyhow::bail!("the text never reached the child's prompt");
    }
    if contents.trim().is_empty() {
        anyhow::bail!("the PTY produced no output at all");
    }
    Ok(())
}

fn setup_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;

    // Without this a panic leaves the terminal in raw mode and unusable.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = io::stdout();
        let _ = execute!(
            out,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
        default_hook(info);
    }));

    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// The TUI. Returns whether the fleet should be started again.
fn run(terminal: &mut Tui, cwd: PathBuf, config_error: Option<String>) -> Result<bool> {
    ui::load_widths();
    let mut app = App::new(cwd);
    let input = Input::spawn();

    // Sizes are only known after the first render, so reopening waits for one:
    // sessions started against a 24x80 guess would paint at the wrong width.
    let restore = supervise::take_restore();
    let mut restore = (!restore.is_empty()).then_some(restore);

    if let Some(e) = config_error {
        app.notify(format!("config rejected: {e}"));
    } else if let Some(w) = supervise::warning() {
        app.notify(w);
    }

    loop {
        app.tick();
        sync_pane_size(terminal, &mut app)?;

        if app.dirty.swap(false, Ordering::Relaxed) {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            if let Some(cwds) = restore.take() {
                app.restore_sessions(cwds)?;
            }
        }

        if let Some(ev) = input.next_within(FRAME_POLL) {
            dispatch(&mut app, &input, ev)?;
        }

        if app.should_quit {
            return Ok(app.restart_requested);
        }
    }
}

/// Route one event, coalescing a burst of character keys into a single paste.
///
/// Not every terminal turns a paste into `Event::Paste` — on Windows crossterm
/// never does, since that event is parsed out of the unix input stream only.
/// There a paste arrives as a flood of key presses, and handling those one by
/// one meant a write and a full redraw per character, with every newline in the
/// text submitting a prompt. Anything arriving faster than a person types is
/// collected and handed over as one paste instead.
fn dispatch(app: &mut App, input: &Input, ev: Event) -> Result<()> {
    // The burst drain hands back the event that ended it, so this is a loop
    // rather than recursion: a long alternating stream must not grow the stack.
    let mut next = Some(ev);
    while let Some(ev) = next.take() {
        match ev {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let key = keys::normalize(key);
                if let Some(text) = typed_text(key)
                    && matches!(app.mode, Mode::Focus | Mode::Commit)
                {
                    let burst = drain_key_burst(input, text);
                    // One character is someone typing; a burst is a paste. So
                    // is a short run while a paste is still open: a tail that
                    // fell just past the grace window is the end of the paste,
                    // not the start of typing, and letting its newline through
                    // as a key press would submit half a prompt.
                    if app.paste_open || burst.text.chars().count() > BURST_MIN {
                        handle_paste(app, &burst.text, burst.capped);
                    } else if app.mode == Mode::Focus && burst.text == "u" && understand_chord(app)
                    {
                        // A `u` on its own, moments after landing in a session,
                        // is the second half of the chord. A `u` that starts a
                        // word is not: the burst carries the rest of it.
                    } else {
                        for k in burst.text.chars() {
                            let code = if k == '\n' {
                                KeyCode::Enter
                            } else {
                                KeyCode::Char(k)
                            };
                            handle_key(app, KeyEvent::new(code, KeyModifiers::NONE))?;
                        }
                    }
                    next = burst.carry;
                    continue;
                }
                handle_key(app, key)?;
            }
            Event::Mouse(m) => handle_mouse(app, m),
            Event::Paste(text) => handle_paste(app, &text, false),
            Event::Resize(_, _) => app.dirty.store(true, Ordering::Relaxed),
            _ => {}
        }
    }
    Ok(())
}

/// The character a key press stands for when it is plain text, which is all a
/// paste-as-keystrokes flood consists of.
fn typed_text(key: KeyEvent) -> Option<char> {
    let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
    match key.code {
        KeyCode::Char(c) if plain => Some(c),
        KeyCode::Enter if key.modifiers.is_empty() => Some('\n'),
        KeyCode::Tab if key.modifiers.is_empty() => Some('\t'),
        _ => None,
    }
}

/// One run of text keys, plus what the caller still owes the rest of the loop.
struct Burst {
    text: String,
    /// The first event that was not part of the run. `read` consumes, so there
    /// is no peeking it back — whoever drained it has to pass it on.
    carry: Option<Event>,
    /// The run hit the size cap with the flood still going, so the paste this
    /// belongs to is not over.
    capped: bool,
}

/// Collect the text keys arriving now, treating a short silence as part of the
/// run rather than the end of it.
fn drain_key_burst(input: &Input, first: char) -> Burst {
    let mut text = String::from(first);
    let mut carry = None;

    loop {
        if text.len() >= BURST_MAX {
            return Burst {
                text,
                carry: None,
                capped: true,
            };
        }
        let quiet = if text.chars().count() > BURST_MIN {
            PASTE_GRACE
        } else {
            TYPING_GRACE
        };
        let Some(ev) = input.next_within(quiet) else {
            break;
        };
        match ev {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                match typed_text(keys::normalize(key)) {
                    Some(c) => text.push(c),
                    None => {
                        carry = Some(Event::Key(key));
                        break;
                    }
                }
            }
            // A real bracketed paste in the middle of the burst is still text.
            Event::Paste(p) => text.push_str(&p),
            Event::Key(_) => {}
            other => {
                carry = Some(other);
                break;
            }
        }
    }

    Burst {
        text,
        carry,
        capped: false,
    }
}

/// Keep every PTY the same size as the pane, so switching sessions never shows
/// a stale layout.
fn sync_pane_size(terminal: &Tui, app: &mut App) -> Result<()> {
    let size = terminal.size()?.into();
    app.term = size;
    app.git_fits = ui::git_fits(size);
    let area = ui::pane_area(size, app.show_git);
    let inner = ui::pane_inner_rect(area);
    app.pane_x = inner.x;
    app.pane_y = inner.y;
    let (rows, cols) = ui::pane_inner(area);
    if (rows, cols) == (app.pane_rows, app.pane_cols) {
        return Ok(());
    }
    app.pane_rows = rows;
    app.pane_cols = cols;
    for s in &mut app.sessions {
        s.resize(rows, cols);
    }
    app.dirty.store(true, Ordering::Relaxed);
    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    close_open_paste(app);
    // What was marked is on the clipboard already; a key moves on from it.
    app.selection = None;
    app.dirty.store(true, Ordering::Relaxed);

    // Reserved function keys work in every mode, so a pane that swallows all
    // other input still has a guaranteed way out. They are deliberately not
    // chords: Claude Code binds plenty of Ctrl combinations itself.
    // An armed `u` has first claim on the function keys: they are what says
    // which session it meant, so they go through the chord, not around it.
    if let KeyCode::F(n) = key.code
        && key.modifiers.is_empty()
        && app.mode != Mode::Understand
        && handle_reserved_fkey(app, n, false)?
    {
        return Ok(());
    }

    // Alt+Shift+G hides or shows the panel from anywhere, a session included.
    // Terminals disagree on whether the shift shows up as the modifier, the
    // capital, or both, so any of them counts.
    if key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && (key.code == KeyCode::Char('G')
            || (key.code == KeyCode::Char('g') && key.modifiers.contains(KeyModifiers::SHIFT)))
        && matches!(app.mode, Mode::Nav | Mode::Focus | Mode::Git | Mode::Commit)
    {
        app.toggle_git();
        return Ok(());
    }

    // Alt+G reaches the git panel from inside a session too, where a plain `g`
    // is only a letter typed into Claude. Pressed on the panel, it goes back.
    if key.code == KeyCode::Char('g')
        && key.modifiers == KeyModifiers::ALT
        && matches!(app.mode, Mode::Nav | Mode::Focus | Mode::Git | Mode::Commit)
    {
        if app.mode.on_git() {
            leave_git(app);
        } else {
            app.focus_git();
        }
        return Ok(());
    }

    match app.mode {
        Mode::Nav => handle_nav(app, key),
        Mode::Focus => handle_focus(app, key)?,
        Mode::Understand => handle_understand(app, key)?,
        Mode::NewSession => handle_form(app, key)?,
        Mode::Help => app.mode = Mode::Nav,
        Mode::ConfirmKill => match key.code {
            KeyCode::Char('t') | KeyCode::Char('y') => {
                app.kill_selected();
                app.mode = Mode::Nav;
            }
            _ => app.mode = Mode::Nav,
        },
        Mode::ConfirmRestart => match key.code {
            KeyCode::Char('t') | KeyCode::Char('y') => app.restart(),
            _ => app.mode = Mode::Nav,
        },
        Mode::ConfirmMkdir => match key.code {
            KeyCode::Char('t') | KeyCode::Char('y') | KeyCode::Enter => app.resolve_mkdir(true)?,
            _ => app.resolve_mkdir(false)?,
        },
        Mode::Resume => handle_resume(app, key)?,
        Mode::Git => handle_git(app, key),
        Mode::Branch => handle_branch(app, key),
        Mode::Commit => handle_commit(app, key),
        Mode::Tag => match key.code {
            KeyCode::Char(c) if c.is_ascii_alphabetic() => {
                app.set_group(Some(c.to_ascii_lowercase()));
            }
            KeyCode::Char('-' | ' ') | KeyCode::Backspace | KeyCode::Delete => {
                app.set_group(None);
            }
            _ => app.mode = Mode::Nav,
        },
        Mode::BigBrother => match key.code {
            KeyCode::Char(c) if c.is_ascii_alphabetic() => {
                app.spawn_big_brother(bigbrother::Scope::Group(c.to_ascii_lowercase()))?;
            }
            KeyCode::Char('*') => app.spawn_big_brother(bigbrother::Scope::All)?,
            KeyCode::Enter => {
                let scope = app.default_scope();
                app.spawn_big_brother(scope)?;
            }
            _ => app.mode = Mode::Nav,
        },
        Mode::Reports => match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                app.reports_scroll = app.reports_scroll.saturating_sub(1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.reports_scroll =
                    (app.reports_scroll + 1).min(app.reports.len().saturating_sub(1));
            }
            _ => app.mode = Mode::Nav,
        },
        Mode::PushFailed => match key.code {
            KeyCode::Char('f') | KeyCode::Char('c') | KeyCode::Enter => app.fix_push()?,
            KeyCode::Char('r') | KeyCode::Char('p') => app.retry_push(),
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(p) = app.push_failed.as_mut() {
                    p.scroll += 1;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(p) = app.push_failed.as_mut() {
                    p.scroll = p.scroll.saturating_sub(1);
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => app.close_push_failed(),
            _ => {}
        },
    }
    Ok(())
}

/// The always-available function keys. Returns whether the key was consumed.
///
/// `understand` marks the press as the target half of the `u` chord: whichever
/// session it lands on gets the prompt typed into it.
fn handle_reserved_fkey(app: &mut App, n: u8, understand: bool) -> Result<bool> {
    match n {
        1..=9 => {
            let idx = (n - 1) as usize;
            if idx == app.sessions.len() {
                // The first free slot is the "one more session" key: F3 with two
                // sessions open spawns the third instead of complaining.
                app.form = None;
                app.pending_mkdir = None;
                if understand {
                    app.arm_understand_spawn();
                }
                let cwd = app.default_cwd();
                app.spawn_session(cwd)?;
                return Ok(true);
            }
            if idx > app.sessions.len() {
                // Silence here reads as a dropped keypress, so say it plainly.
                app.notify(format!(
                    "no session on F{n} — the next free slot is F{}",
                    app.sessions.len() + 1
                ));
                return Ok(true);
            }
            // The jump wins over whatever overlay was up, so drop a half-filled
            // form instead of leaving it behind the pane.
            app.form = None;
            app.pending_mkdir = None;
            app.select_index(idx);
            if understand {
                app.understand(idx);
                return Ok(true);
            }
            // Jumping to a live session lands you inside it, ready to type.
            if app.sessions[idx].is_alive() {
                app.mode = Mode::Focus;
                // Landing here arms the other half of the chord: `u` next.
                app.open_understand_window(idx);
            } else {
                app.mode = Mode::Nav;
                app.notify("session finished — selected, but there is nowhere to type");
            }
            Ok(true)
        }
        // The escape hatch out of a focused pane.
        10 => {
            app.mode = Mode::Nav;
            Ok(true)
        }
        11 => {
            app.open_new_session_form_with(understand);
            Ok(true)
        }
        12 => {
            app.mode = Mode::Help;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn handle_nav(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => app.select(1),
        KeyCode::Char('k') | KeyCode::Up => app.select(-1),
        KeyCode::Enter | KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right => {
            if app.selected_session().is_some_and(|s| s.is_alive()) {
                app.mode = Mode::Focus;
            }
        }
        KeyCode::Char('n') => app.open_new_session_form(),
        KeyCode::Char('u') => app.arm_understand(),
        KeyCode::Char('r') => app.request_restart(),
        // Shift, because `r` is the restart and the two are one keypress apart
        // in a list where one of them costs every running session.
        KeyCode::Char('R') => app.open_resume_picker(),
        KeyCode::Char('x') => {
            if app.selected_session().is_some_and(|s| s.is_alive()) {
                app.mode = Mode::ConfirmKill;
            }
        }
        KeyCode::Char('w') => {
            // Only close a pane whose process is already gone; killing is `x`.
            if app.selected_session().is_some_and(|s| !s.is_alive()) {
                app.close_selected();
            } else {
                app.notify("the session is still alive — x first");
            }
        }
        // Shift, for the same reason `R` is: `u` next to it arms the chord, and
        // this one spawns a process rather than typing into one.
        KeyCode::Char('U') => {
            if app.usage_refreshing() {
                app.notify("limits are already being refreshed");
            } else {
                app.refresh_usage();
                app.notify("asking Claude Code for fresh limits");
            }
        }
        // `U` would read better, but it has belonged to the limits for longer.
        KeyCode::Char('i') => app.install_update(),
        KeyCode::Char('g') => app.focus_git(),
        KeyCode::Char('G') => app.toggle_git(),
        KeyCode::Char('b') => app.open_branch_picker(),
        KeyCode::Char('t') => {
            if app.selected_session().is_some() {
                app.mode = Mode::Tag;
            }
        }
        KeyCode::Char('B') => app.mode = Mode::BigBrother,
        KeyCode::Char('A') => app.open_reports(),
        KeyCode::Char('?') => app.mode = Mode::Help,
        KeyCode::Char(c @ ('[' | ']' | '{' | '}')) => resize_by_key(app, c),
        _ => {}
    }
}

/// The git panel with the keyboard on it. The pane beside it shows the diff or
/// the commit under the cursor, and scrolls with the page keys.
fn handle_git(app: &mut App, key: KeyEvent) {
    let page = app.pane_rows.saturating_sub(2).max(1) as isize;
    match key.code {
        // An open preview closes first, giving the pane back to the session.
        KeyCode::Esc if app.git_view.preview_open => app.git_view.preview_open = false,
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('g') => app.mode = Mode::Nav,
        KeyCode::Char('M') => app.cycle_commit_model(),
        KeyCode::Char('G') => app.toggle_git(),
        KeyCode::Char('b') => app.open_branch_picker(),
        KeyCode::Char('c') => app.focus_commit(),
        KeyCode::Char('p') => app.push(),
        KeyCode::Char('m') => app.generate_message(),
        KeyCode::Char('j') | KeyCode::Down => app.with_git(|v, s| {
            v.move_cursor(s, 1);
        }),
        KeyCode::Char('k') | KeyCode::Up => {
            // The message box sits above the list: up off its top goes there.
            let mut moved = true;
            app.with_git(|v, s| moved = v.move_cursor(s, -1));
            if !moved {
                app.focus_commit();
            }
        }
        KeyCode::Home => app.with_git(|v, s| v.home(s)),
        KeyCode::End => app.with_git(|v, s| v.end(s)),
        KeyCode::Enter | KeyCode::Char(' ') => app.with_git(|v, _| v.toggle()),
        KeyCode::Char('l') | KeyCode::Right => app.with_git(|v, _| v.open()),
        KeyCode::Char('h') | KeyCode::Left => {
            let mut closed = false;
            app.with_git(|v, _| closed = v.close());
            // Nothing left to close: left leaves the panel, the way it leaves
            // the input box of a session.
            if !closed {
                app.mode = Mode::Nav;
            }
        }
        KeyCode::PageDown => app.git_view.scroll_preview(page),
        KeyCode::PageUp => app.git_view.scroll_preview(-page),
        KeyCode::Char('J') => app.git_view.scroll_preview(1),
        KeyCode::Char('K') => app.git_view.scroll_preview(-1),
        KeyCode::Char(c @ ('[' | ']' | '{' | '}')) => resize_by_key(app, c),
        _ => {}
    }
    app.dirty.store(true, Ordering::Relaxed);
}

/// The commit message box. Enter commits; a new line is Shift+Enter or Ctrl+J,
/// since Alt+Enter is the terminal's own full-screen key on Windows.
fn handle_commit(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let word = ctrl || alt;
    let (text, cur) = (&mut app.commit_msg, &mut app.commit_cursor);
    match key.code {
        KeyCode::Esc => app.mode = Mode::Git,
        KeyCode::Enter if shift || alt || ctrl => msgedit::insert(text, cur, "\n"),
        KeyCode::Char('j') if ctrl => msgedit::insert(text, cur, "\n"),
        KeyCode::Enter => app.commit(),
        KeyCode::Backspace if word => msgedit::delete_word_back(text, cur),
        KeyCode::Backspace => msgedit::backspace(text, cur),
        KeyCode::Delete => msgedit::delete(text, cur),
        KeyCode::Left if word => msgedit::word_left(text, cur),
        KeyCode::Right if word => msgedit::word_right(text, cur),
        KeyCode::Left => msgedit::left(text, cur),
        KeyCode::Right => msgedit::right(text, cur),
        KeyCode::Home => msgedit::home(text, cur),
        KeyCode::End => msgedit::end(text, cur),
        KeyCode::Up => {
            msgedit::up(text, cur);
        }
        // The list sits under the box: down off its last line goes there.
        KeyCode::Down => {
            if !msgedit::down(text, cur) {
                app.mode = Mode::Git;
            }
        }
        KeyCode::Char('g') if ctrl => app.generate_message(),
        KeyCode::Char('o') if ctrl => app.cycle_commit_model(),
        KeyCode::Char('p') if ctrl => app.push(),
        KeyCode::Char('u') if ctrl => {
            text.clear();
            *cur = 0;
        }
        KeyCode::Tab => app.mode = Mode::Git,
        KeyCode::Char(c) if !ctrl => msgedit::insert(text, cur, c.encode_utf8(&mut [0; 4])),
        _ => {}
    }
    app.dirty.store(true, Ordering::Relaxed);
}

/// `[` `]` narrow and widen the sidebar, `{` `}` the git panel, a few
/// columns at a time.
fn resize_by_key(app: &mut App, c: char) {
    const STEP: u16 = 4;
    match c {
        '[' => ui::set_sidebar_width(
            ui::sidebar_width().saturating_sub(STEP),
            app.term,
            app.show_git,
        ),
        ']' => ui::set_sidebar_width(ui::sidebar_width() + STEP, app.term, app.show_git),
        '{' => ui::set_git_width(ui::git_width().saturating_sub(STEP), app.term),
        _ => ui::set_git_width(ui::git_width() + STEP, app.term),
    }
    ui::save_widths();
    app.dirty.store(true, Ordering::Relaxed);
}

/// The column border under the pointer, if it is on one. Either of the two
/// border columns that meet there counts, so the grab is not a one-cell hunt.
fn border_at(app: &App, column: u16) -> Option<Drag> {
    let sidebar = ui::sidebar_width();
    if column + 1 == sidebar || column == sidebar {
        return Some(Drag::Sidebar);
    }
    if app.show_git && app.git_fits {
        let git_x = app.term.width.saturating_sub(ui::git_width());
        if column + 1 == git_x || column == git_x {
            return Some(Drag::Git);
        }
    }
    None
}

/// Move the border being dragged to the pointer's column.
fn drag_to(app: &mut App, drag: Drag, column: u16) {
    match drag {
        Drag::Sidebar => ui::set_sidebar_width(column + 1, app.term, app.show_git),
        Drag::Git => ui::set_git_width(app.term.width.saturating_sub(column), app.term),
    }
    app.dirty.store(true, Ordering::Relaxed);
}

/// Back from the git panel to the session it was about, or to the list when
/// there is no live one to type into.
fn leave_git(app: &mut App) {
    app.mode = if app.selected_session().is_some_and(|s| s.is_alive()) {
        Mode::Focus
    } else {
        Mode::Nav
    };
}

/// A left click puts the keyboard where the pointer is: a card in the sidebar
/// selects it, the pane goes into the session, the git panel takes the keys.
/// Dialogs and prompts keep theirs — a stray click must not answer them.
fn handle_click(app: &mut App, m: MouseEvent) {
    let hit = app
        .git_hits
        .iter()
        .find(|(r, _)| r.contains(ratatui::layout::Position::new(m.column, m.row)))
        .map(|(_, h)| *h);
    if app.mode == Mode::PushFailed {
        match hit {
            Some(GitHit::FixPush) => {
                if let Err(e) = app.fix_push() {
                    app.notify(format!("could not start a session: {e}"));
                }
            }
            Some(GitHit::RetryPush) => app.retry_push(),
            Some(GitHit::CloseDialog) => app.close_push_failed(),
            _ => {}
        }
        app.dirty.store(true, Ordering::Relaxed);
        return;
    }
    if !matches!(app.mode, Mode::Nav | Mode::Focus | Mode::Git | Mode::Commit) {
        return;
    }
    if let Some(hit) = hit {
        match hit {
            // Only the failed-push dialog draws these.
            GitHit::FixPush | GitHit::RetryPush | GitHit::CloseDialog => {}
            GitHit::Message => app.focus_commit(),
            GitHit::Commit => app.commit(),
            GitHit::Push => app.push(),
            GitHit::Generate => app.generate_message(),
            GitHit::Model => app.cycle_commit_model(),
        }
        // A button pressed from inside a session still leaves the keyboard on
        // the panel it was pressed on, not on the session behind.
        if !app.mode.on_git() && app.show_git {
            app.focus_git();
        }
        app.dirty.store(true, Ordering::Relaxed);
        return;
    }
    if m.column < app.pane_x {
        // Each card is two rows, from the top of the list. The list scrolls
        // only once there are more cards than rows, which nine rarely are.
        let Some(row) = m.row.checked_sub(app.pane_y) else {
            return;
        };
        let idx = (row / 2) as usize;
        if idx < app.sessions.len() {
            app.select_index(idx);
            app.mode = Mode::Nav;
        }
    } else if m.column > app.pane_x + app.pane_cols {
        if app.show_git && app.git_fits {
            // Out of the message box and back to the list, or onto the panel.
            app.focus_git();
        }
    } else if app.selected_session().is_some_and(|s| s.is_alive()) {
        app.mode = Mode::Focus;
    } else {
        app.mode = Mode::Nav;
    }
    app.dirty.store(true, Ordering::Relaxed);
}

/// The branch list. Letters narrow it down rather than doing their usual job,
/// so a branch name can be typed straight in.
fn handle_branch(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.close_branch_picker(),
        KeyCode::Enter => app.switch_selected_branch(),
        KeyCode::Up => {
            if let Some(p) = app.branches.as_mut() {
                p.move_cursor(-1);
            }
        }
        KeyCode::Down | KeyCode::Tab => {
            if let Some(p) = app.branches.as_mut() {
                p.move_cursor(1);
            }
        }
        KeyCode::Backspace => {
            if let Some(p) = app.branches.as_mut() {
                p.pop();
            }
        }
        // A branch name has no spaces; one typed is a slip, not a request.
        KeyCode::Char(c) if c != ' ' && !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(p) = app.branches.as_mut() {
                p.push(c);
            }
        }
        _ => {}
    }
}

/// The list of past conversations. Moves, opens, or closes; nothing here
/// reaches the fleet underneath.
fn handle_resume(app: &mut App, key: KeyEvent) -> Result<()> {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            if let Some(p) = app.resume.as_mut() {
                p.move_cursor(1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if let Some(p) = app.resume.as_mut() {
                p.move_cursor(-1);
            }
        }
        KeyCode::Enter => app.resume_selected()?,
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('R') => app.close_resume_picker(),
        _ => {}
    }
    Ok(())
}

/// The armed `u` waiting to be told which session it is for.
///
/// Every key answers it, one way or another: a target key does the thing, and
/// anything else calls the chord off rather than doing its usual job, so a
/// stray press cannot start a session or kill one by accident.
fn handle_understand(app: &mut App, key: KeyEvent) -> Result<()> {
    app.mode = Mode::Nav;
    match key.code {
        KeyCode::F(n) => {
            handle_reserved_fkey(app, n, true)?;
        }
        // The dialog picks the directory; the prompt rides along with it.
        KeyCode::Char('n') => app.open_new_session_form_with(true),
        // `u u` is the quick one: a new session in the first free slot, no
        // dialog, exactly like the free-slot function key.
        KeyCode::Char('u') => {
            let slot = app.sessions.len();
            if slot >= 9 {
                app.notify("every F1-F9 slot is taken");
            } else {
                handle_reserved_fkey(app, (slot + 1) as u8, true)?;
            }
        }
        KeyCode::Enter | KeyCode::Tab => {
            if app.sessions.is_empty() {
                app.notify("no sessions — u u starts one");
            } else {
                app.understand(app.selected);
            }
        }
        _ => app.notify("understand project cancelled"),
    }
    Ok(())
}

/// The other half of the chord: a lone `u` typed just after landing in a
/// session. Returns whether it was the chord and has been dealt with.
fn understand_chord(app: &mut App) -> bool {
    let Some(idx) = app.take_understand_window() else {
        return false;
    };
    app.understand(idx);
    true
}

fn handle_focus(app: &mut App, key: KeyEvent) -> Result<()> {
    let git_fits = app.git_fits;
    let Some(session) = app.selected_session_mut() else {
        app.mode = Mode::Nav;
        return Ok(());
    };
    if !session.is_alive() {
        app.mode = Mode::Nav;
        app.notify("session finished — its card goes by itself after a minute, w closes it now");
        return Ok(());
    }

    // A left arrow with nowhere left to go in the input steps out to the
    // session list, which sits to the left — the same as F10.
    if key.code == KeyCode::Left && key.modifiers.is_empty() && session.cursor_at_prompt_start() {
        app.mode = Mode::Nav;
        return Ok(());
    }
    // And a right arrow past the end of the input steps over to the git
    // panel on the right.
    if key.code == KeyCode::Right
        && key.modifiers.is_empty()
        && git_fits
        && session.cursor_at_prompt_end()
    {
        app.focus_git();
        return Ok(());
    }

    // Ctrl+V only gets here when the terminal found no text to paste, which
    // is exactly when the clipboard may hold a screenshot. Alt+V is the same
    // ask for terminals that keep Ctrl+V to themselves.
    if key.code == KeyCode::Char('v')
        && (key.modifiers == KeyModifiers::CONTROL || key.modifiers == KeyModifiers::ALT)
        && let Some(text) = clipimg::paste_text()
    {
        handle_paste(app, &text, false);
        return Ok(());
    }

    let app_cursor = session.application_cursor();
    if let Some(bytes) = keys::encode(key, app_cursor) {
        session.write_input(&bytes)?;
    }
    Ok(())
}

fn handle_form(app: &mut App, key: KeyEvent) -> Result<()> {
    let Some(form) = app.form.as_mut() else {
        app.mode = Mode::Nav;
        return Ok(());
    };

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // Ctrl+R: local, a new remote session, or teleport one down. A plain
    // letter would land in the path.
    if key.code == KeyCode::Char('r') && ctrl {
        form.kind = form.kind.next();
        if form.kind == SpawnKind::Remote {
            app.load_repos();
        }
        return Ok(());
    }
    // Remote picks a repository: typing filters them, the arrows move.
    if form.kind == SpawnKind::Remote {
        match key.code {
            KeyCode::Esc => {
                app.form = None;
                app.mode = Mode::Nav;
                app.disarm_understand_spawn();
            }
            KeyCode::Enter => app.start_remote()?,
            KeyCode::Up => form.repo_cursor = form.repo_cursor.saturating_sub(1),
            KeyCode::Down => {
                let last = app.filtered_repos().len().saturating_sub(1);
                if let Some(form) = app.form.as_mut() {
                    form.repo_cursor = (form.repo_cursor + 1).min(last);
                }
            }
            KeyCode::Backspace => {
                form.repo_filter.pop();
                form.repo_cursor = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                form.repo_filter.push(c);
                form.repo_cursor = 0;
            }
            _ => {}
        }
        return Ok(());
    }

    match key.code {
        KeyCode::Esc => {
            app.form = None;
            app.mode = Mode::Nav;
            // The abandoned form takes its armed prompt with it.
            app.disarm_understand_spawn();
        }
        KeyCode::Enter => {
            // The form stays up until the path is settled: a missing directory
            // asks first, and a "no" lands back on the same typed path.
            let path = form.selected_path();
            app.request_spawn(path)?;
        }
        KeyCode::Up => form.move_cursor(-1),
        KeyCode::Down => form.move_cursor(1),
        // Browse the tree without leaving the form: right/tab steps into the
        // highlighted subdirectory (or the single match of what was typed),
        // left steps back up to the parent.
        KeyCode::Right | KeyCode::Tab => form.descend(),
        KeyCode::Left => form.ascend(),
        KeyCode::Backspace if form.cursor == 0 => form.pop(),
        KeyCode::Char(c) if form.cursor == 0 && !key.modifiers.contains(KeyModifiers::CONTROL) => {
            form.push(c);
        }
        _ => {}
    }
    Ok(())
}

/// A wheel notch either moves our own scrollback or belongs to the child.
///
/// Claude Code runs on the alternate screen and does its own mouse tracking, so
/// its history is inside the child and the emulator's scrollback stays empty —
/// the alternate grid keeps none. Scrolling only works if the notch is handed
/// over the way a real terminal hands it over. A child that never asked for the
/// mouse still gets the old behaviour, where the scrollback is ours to move.
fn handle_mouse(app: &mut App, m: MouseEvent) {
    let up = match m.kind {
        MouseEventKind::ScrollUp => true,
        MouseEventKind::ScrollDown => false,
        MouseEventKind::Down(MouseButton::Left) => {
            // A press on a column border picks it up rather than clicking.
            if matches!(app.mode, Mode::Nav | Mode::Focus | Mode::Git | Mode::Commit)
                && let Some(drag) = border_at(app, m.column)
            {
                app.drag = Some(drag);
                return;
            }
            start_selection(app, m);
            return handle_click(app, m);
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(drag) = app.drag {
                drag_to(app, drag, m.column);
            } else if let Some(sel) = app.selection.as_mut().filter(|s| s.dragging) {
                sel.head = pane_cell(app.pane_x, app.pane_y, app.pane_rows, app.pane_cols, m);
                app.dirty.store(true, Ordering::Relaxed);
            }
            return;
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(drag) = app.drag.take() {
                drag_to(app, drag, m.column);
                ui::save_widths();
            } else {
                finish_selection(app, m);
            }
            return;
        }
        _ => return,
    };

    // The failed-push dialog scrolls its own text, whatever is under it.
    if let Some(p) = app
        .push_failed
        .as_mut()
        .filter(|_| app.mode == Mode::PushFailed)
    {
        p.scroll = if up {
            p.scroll.saturating_sub(SCROLL_STEP as usize)
        } else {
            p.scroll + SCROLL_STEP as usize
        };
        app.dirty.store(true, Ordering::Relaxed);
        return;
    }
    // A notch over the sidebar moves the selection, not the terminal.
    if m.column < app.pane_x {
        app.select(if up { -1 } else { 1 });
        app.dirty.store(true, Ordering::Relaxed);
        return;
    }
    // With the keyboard on the git panel, the wheel moves its cursor over the
    // panel and scrolls the preview over the pane.
    let over_git = m.column > app.pane_x + app.pane_cols;
    if app.mode.on_git() {
        let delta = if up { -SCROLL_STEP } else { SCROLL_STEP };
        if over_git {
            app.with_git(|v, s| {
                v.move_cursor(s, delta.signum());
            });
            app.dirty.store(true, Ordering::Relaxed);
            return;
        }
        if app.git_view.preview_open {
            app.git_view.scroll_preview(delta);
            app.dirty.store(true, Ordering::Relaxed);
            return;
        }
        // No preview open: the session is on the pane and scrolls as usual.
    }
    // Otherwise the panel does not scroll.
    if over_git {
        return;
    }

    let (px, py) = (app.pane_x, app.pane_y);
    let (rows, cols) = (app.pane_rows, app.pane_cols);
    let Some(s) = app.selected_session_mut() else {
        return;
    };

    if let Some(encoding) = s.mouse_reporting() {
        // Coordinates are 1-based and pane-relative, clamped to the pane: a
        // notch just off its edge still belongs to the pane under the pointer.
        let col = m.column.saturating_sub(px).min(cols.saturating_sub(1)) + 1;
        let row = m.row.saturating_sub(py).min(rows.saturating_sub(1)) + 1;
        let mut bytes = Vec::new();
        for _ in 0..SCROLL_STEP {
            let Some(one) = keys::encode_wheel(up, col, row, encoding) else {
                return;
            };
            bytes.extend_from_slice(&one);
        }
        let _ = s.write_passthrough(&bytes);
    } else {
        let delta = if up { SCROLL_STEP } else { -SCROLL_STEP };
        s.scroll_by(delta);
    }
    app.dirty.store(true, Ordering::Relaxed);
}

/// The pane cell under the pointer, clamped to the pane: a drag that runs off
/// its edge keeps marking up to that edge.
fn pane_cell(px: u16, py: u16, rows: u16, cols: u16, m: MouseEvent) -> (u16, u16) {
    let row = m.row.saturating_sub(py).min(rows.saturating_sub(1));
    let col = m.column.saturating_sub(px).min(cols.saturating_sub(1));
    (row, col)
}

/// A press inside the pane may be the start of a selection; one anywhere else
/// drops the last one.
fn start_selection(app: &mut App, m: MouseEvent) {
    let inside = m.column >= app.pane_x
        && m.column < app.pane_x + app.pane_cols
        && m.row >= app.pane_y
        && m.row < app.pane_y + app.pane_rows;
    if app.selection.take().is_some() {
        app.dirty.store(true, Ordering::Relaxed);
    }
    if !inside
        || !matches!(app.mode, Mode::Nav | Mode::Focus | Mode::Git | Mode::Commit)
        || app.selected_session().is_none()
    {
        return;
    }
    let cell = pane_cell(app.pane_x, app.pane_y, app.pane_rows, app.pane_cols, m);
    app.selection = Some(app::Selection {
        session: app.selected,
        anchor: cell,
        head: cell,
        dragging: true,
    });
}

/// The button came up: a drag is copied, the way terminals copy on release;
/// a plain click was only a click and leaves nothing marked.
fn finish_selection(app: &mut App, m: MouseEvent) {
    let head = pane_cell(app.pane_x, app.pane_y, app.pane_rows, app.pane_cols, m);
    let Some(sel) = app.selection.as_mut().filter(|s| s.dragging) else {
        return;
    };
    sel.head = head;
    sel.dragging = false;
    let sel = *sel;
    app.dirty.store(true, Ordering::Relaxed);
    if sel.anchor == sel.head {
        app.selection = None;
        return;
    }
    let (start, end) = sel.ordered();
    let Some(text) = app
        .sessions
        .get(sel.session)
        .map(|s| s.text_between(start, end))
    else {
        return;
    };
    if text.trim().is_empty() {
        return;
    }
    let chars = text.chars().count();
    if clipimg::copy_text(&text) {
        app.notify(format!("copied {chars} characters"));
    } else {
        app.notify("copying to the clipboard failed");
    }
}

fn handle_paste(app: &mut App, text: &str, keep_open: bool) {
    if app.mode == Mode::Commit {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        msgedit::insert(&mut app.commit_msg, &mut app.commit_cursor, &text);
        app.dirty.store(true, Ordering::Relaxed);
        return;
    }
    if app.mode == Mode::NewSession {
        if let Some(form) = app.form.as_mut()
            && form.kind == SpawnKind::Remote
        {
            form.repo_filter.push_str(text.trim());
            form.repo_cursor = 0;
            app.dirty.store(true, Ordering::Relaxed);
        } else if let Some(form) = app.form.as_mut()
            && form.cursor == 0
        {
            form.push_str(text.trim());
            app.dirty.store(true, Ordering::Relaxed);
        }
        return;
    }
    if !matches!(app.mode, Mode::Focus) {
        return;
    }
    let opening = !app.paste_open;
    let Some(s) = app.selected_session_mut() else {
        return;
    };
    let bytes = keys::encode_paste_chunk(text, s.bracketed_paste(), opening, !keep_open);
    let big = bytes.len() >= 4096;
    let _ = s.write_input(&bytes);
    app.paste_open = keep_open;
    if big && opening {
        // A paste this size takes visible time to reach the child; saying so
        // beats a pane that looks stuck.
        let kb = bytes.len() / 1024;
        let lines = text.lines().count();
        app.notify(format!("pasting {kb} KB ({lines} lines) — F10 still works"));
    }
    app.dirty.store(true, Ordering::Relaxed);
}

/// Close a paste that was handed over in pieces.
///
/// Anything that is not more of that paste — a key press, leaving focus — has
/// to wait for its closing marker, or the child would read the rest of the
/// session as pasted text.
fn close_open_paste(app: &mut App) {
    if !app.paste_open {
        return;
    }
    app.paste_open = false;
    if let Some(s) = app.selected_session_mut()
        && s.bracketed_paste()
    {
        let _ = s.write_input(keys::PASTE_END);
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn wheel_is_encoded_the_way_a_terminal_encodes_it() {
        let up = keys::encode_wheel(true, 3, 7, vt100::MouseProtocolEncoding::Sgr).unwrap();
        assert_eq!(up, b"[<64;3;7M".to_vec());
        let down = keys::encode_wheel(false, 1, 1, vt100::MouseProtocolEncoding::Sgr).unwrap();
        assert_eq!(down, b"[<65;1;1M".to_vec());
        let legacy = keys::encode_wheel(true, 1, 1, vt100::MouseProtocolEncoding::Default).unwrap();
        assert_eq!(legacy, vec![0x1b, b'[', b'M', 96, 33, 33]);
        // The one-byte encoding cannot address a far-right column at all.
        assert!(keys::encode_wheel(true, 300, 1, vt100::MouseProtocolEncoding::Default).is_none());
    }
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn plain_characters_are_burst_material() {
        assert_eq!(
            typed_text(key(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some('a')
        );
        assert_eq!(
            typed_text(key(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            Some('A')
        );
        assert_eq!(
            typed_text(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some('\n')
        );
    }

    #[test]
    fn chords_and_control_keys_are_not() {
        // These must stay real key presses: folding them into a paste would
        // strip their meaning.
        assert_eq!(
            typed_text(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(typed_text(key(KeyCode::Enter, KeyModifiers::SHIFT)), None);
        assert_eq!(typed_text(key(KeyCode::F(10), KeyModifiers::NONE)), None);
        assert_eq!(typed_text(key(KeyCode::Up, KeyModifiers::NONE)), None);
    }
}
