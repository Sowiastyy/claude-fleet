//! Running the TUI out of a copy of the build output, and restarting it into a
//! newer one without leaving the terminal.
//!
//! The process one starts becomes a supervisor: it copies the build output to
//! a scratch file, runs that copy in the same console and waits. A copy asking
//! for a restart exits with `RESTART_EXIT` and the loop comes round again with
//! whatever the build output holds by then. The two never run at once, so
//! nothing fights over the terminal.
//!
//! The copy is the point. A running executable is locked on Windows, so a fleet
//! run straight out of `target` makes the next `cargo build` fail until it is
//! closed — the very thing one wants to do while changing the fleet. Note that
//! the supervisor itself holds its *own* file open the whole time, so the two
//! paths have to differ: keep a `claude-fleet.exe` outside `target` and run
//! that one. It is found automatically when the layout is the usual one, and
//! `source_warning` says so when it is not.
//!
//! Sessions do not survive this. A child under our ConPTY dies with the process
//! that created it — `--orphan-probe` demonstrates it — and the pseudoconsole
//! handle cannot be passed on, so there is nothing to hand over. What the fleet
//! can do is note which directories were open, and which conversation each one
//! was holding, and start them again against the same transcripts — which is
//! what the restore file is for.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result};

/// Exit code by which the inner process asks to be started again.
pub const RESTART_EXIT: i32 = 75;

/// Set on the inner process, so it knows it is the copy and not the supervisor.
pub const RUN_MARKER: &str = "CLAUDE_FLEET_INNER";
/// The path the supervisor copies from, which is the file a build replaces.
/// Also set on the inner process, which watches it for a rebuild.
pub const ORIGIN_VAR: &str = "CLAUDE_FLEET_ORIGIN";

/// Overrides which binary is treated as the build output.
pub const BUILD_VAR: &str = "CLAUDE_FLEET_BUILD";
/// Where the inner process leaves the directories to reopen after a restart.
pub const RESTORE_VAR: &str = "CLAUDE_FLEET_RESTORE";
/// Carries `source_warning` to the inner process. Printing it in the
/// supervisor is not enough: the TUI covers the screen a moment later.
pub const WARN_VAR: &str = "CLAUDE_FLEET_WARN";

/// A restore file with no copy beside it is swept once it is older than this.
/// The copies themselves are swept by a surer rule — see `sweep`.
const SWEEP_AGE: Duration = Duration::from_secs(24 * 60 * 60);

pub fn is_inner() -> bool {
    env::var_os(RUN_MARKER).is_some()
}

/// The binary the supervisor runs, which is the one a rebuild replaces.
///
/// A fleet kept next to its own `target` directory — the shape of this repo —
/// watches the build output rather than itself, so `cargo build` never has to
/// win a fight over a locked file and a restart picks the new binary up with
/// nothing copied by hand. Anywhere else it is just the file one started.
pub fn resolve_source(own: &Path) -> PathBuf {
    if let Some(p) = env::var_os(BUILD_VAR) {
        return PathBuf::from(p);
    }
    let Some(dir) = own.parent() else {
        return own.to_path_buf();
    };
    let name = own.file_name().unwrap_or_default();
    let candidates = [
        dir.join("target").join("release").join(name),
        dir.join("target").join("debug").join(name),
    ];
    // Newest wins, so switching between debug and release builds needs no
    // configuration either.
    let newest = candidates
        .into_iter()
        .filter(|p| p.is_file() && p != own)
        .filter_map(|p| Some((fs::metadata(&p).ok()?.modified().ok()?, p)))
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p);
    newest.unwrap_or_else(|| own.to_path_buf())
}

/// Said once at startup when the fleet is running out of the same file a build
/// writes to, because then `cargo build` fails on a locked file and no amount
/// of copying downstream helps.
pub fn source_warning(own: &Path, source: &Path) -> Option<String> {
    (own == source).then(|| {
        format!(
            "fleet is running from {} — a cargo build into that path will fail; keep a copy outside target/ or set {BUILD_VAR}",
            own.display()
        )
    })
}

/// What the supervisor wants the user told, if anything.
pub fn warning() -> Option<String> {
    env::var(WARN_VAR).ok().filter(|w| !w.is_empty())
}

/// The binary a build replaces, as told to us by the supervisor.
pub fn origin() -> Option<PathBuf> {
    env::var_os(ORIGIN_VAR).map(PathBuf::from)
}

/// When the origin binary was last written, or `None` if it cannot be read.
pub fn origin_stamp() -> Option<SystemTime> {
    fs::metadata(origin()?).ok()?.modified().ok()
}

fn work_dir() -> PathBuf {
    env::temp_dir().join("claude-fleet")
}

/// Run the TUI from a copy, again and again for as long as it asks to be.
pub fn supervise() -> Result<()> {
    let own = env::current_exe().context("cannot tell where fleet was started from")?;
    let origin = resolve_source(&own);
    let warning = source_warning(&own, &origin);
    let dir = work_dir();
    fs::create_dir_all(&dir).with_context(|| format!("cannot use {}", dir.display()))?;
    sweep(&dir);

    // Named after this process, so two fleets running at once never copy over
    // each other's binary.
    let copy = dir.join(format!("fleet-{}.exe", std::process::id()));
    let restore = dir.join(format!("restore-{}.txt", std::process::id()));
    let args: Vec<_> = env::args_os().skip(1).collect();

    // Copying happens inside the loop: a restart is asked for precisely because
    // the file changed, so the next run has to take it again.
    let result = run_until_done(|| {
        fs::copy(&origin, &copy).with_context(|| {
            format!(
                "could not copy {} to {}",
                origin.display(),
                copy.display()
            )
        })?;

        let mut child = Command::new(&copy);
        child
            .args(&args)
            .env(RUN_MARKER, "1")
            .env(ORIGIN_VAR, &origin)
            .env(RESTORE_VAR, &restore);
        match &warning {
            Some(w) => child.env(WARN_VAR, w),
            None => child.env_remove(WARN_VAR),
        };
        let status = child.status().context("could not start the copy")?;
        Ok(status.code())
    });

    let _ = fs::remove_file(&copy);
    let _ = fs::remove_file(&restore);
    result.map(|_| ())
}

/// Run something over and over for as long as it exits asking to be run again,
/// and say how many times it ran.
///
/// Split out from the spawning so the rule itself can be tested: a loop that
/// either never comes back or never stops is the whole risk here, and both
/// shapes are invisible in a one-shot manual run.
fn run_until_done(mut launch: impl FnMut() -> Result<Option<i32>>) -> Result<u32> {
    let mut runs = 0;
    loop {
        runs += 1;
        if launch()? != Some(RESTART_EXIT) {
            return Ok(runs);
        }
    }
}

/// Drop copies and restore files left by runs that never got to tidy up.
///
/// A running executable cannot be deleted on Windows, which turns the delete
/// itself into the liveness test: the copies that refuse to go are the ones
/// still in use. That beats an age rule, which would either leave a dead
/// fleet's copy lying about for a day or race a live one. A restore file is
/// only dropped once its own copy has gone, so a fleet that restarts while
/// this runs still finds the list it wrote.
fn sweep(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut leftover_restores = Vec::new();

    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(pid) = name
            .strip_prefix("fleet-")
            .and_then(|r| r.strip_suffix(".exe"))
        {
            if fs::remove_file(e.path()).is_ok() {
                let _ = fs::remove_file(dir.join(format!("restore-{pid}.txt")));
            }
            continue;
        }
        if name.starts_with("restore-") {
            leftover_restores.push(e);
        }
    }

    // Whatever is left has no copy to speak for it either way; age decides.
    for e in leftover_restores {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t.elapsed().unwrap_or_default() > SWEEP_AGE)
            .unwrap_or(false);
        if old {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// A session to bring back: where it was working and, when it is known, the
/// conversation it was holding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restore {
    pub cwd: PathBuf,
    /// The id `claude --resume` takes. `None` means start a fresh session
    /// there, which is all the fleet could do before transcripts were read.
    pub session: Option<String>,
    /// The group the session was in.
    pub group: Option<char>,
    /// Set when the session was a Big Brother: the scope it watched, as
    /// `Scope::name` writes it.
    pub watch: Option<String>,
}

impl Restore {
    pub fn new(cwd: PathBuf, session: Option<String>) -> Self {
        Self {
            cwd,
            session,
            group: None,
            watch: None,
        }
    }
}

/// Hand the next run what was open, one session per line: the conversation id,
/// a tab, and the directory. A line with no tab is a directory on its own,
/// which is what earlier versions wrote and still means "start fresh here".
pub fn write_restore(items: &[Restore]) {
    let Some(path) = env::var_os(RESTORE_VAR).map(PathBuf::from) else {
        return;
    };
    let body: String = items
        .iter()
        .map(|r| {
            // A session in a group, or a Big Brother, adds two more fields:
            // the group and the scope watched, either empty when not wanted.
            let tagged = r.group.is_some() || r.watch.is_some();
            match (&r.session, tagged) {
                (id, true) => format!(
                    "{}\t{}\t{}\t{}\n",
                    id.as_deref().unwrap_or_default(),
                    r.cwd.display(),
                    r.group.map(String::from).unwrap_or_default(),
                    r.watch.as_deref().unwrap_or_default(),
                ),
                (Some(id), false) => format!("{id}\t{}\n", r.cwd.display()),
                (None, false) => format!("{}\n", r.cwd.display()),
            }
        })
        .collect();
    let _ = fs::write(path, body);
}

/// Read back what the previous run left, and clear it.
///
/// Clearing matters: the list describes one restart, and a leftover file would
/// reopen those directories on every later start.
pub fn take_restore() -> Vec<Restore> {
    let Some(path) = env::var_os(RESTORE_VAR).map(PathBuf::from) else {
        return Vec::new();
    };
    let Ok(body) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let _ = fs::remove_file(&path);
    body.lines()
        // Only line ends are trimmed: a tab at either end is an empty field.
        .map(|l| l.trim_end_matches(['\r', ' ']))
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                [id, cwd, group, watch, ..] => {
                    let mut r = Restore::new(
                        PathBuf::from(cwd),
                        (!id.is_empty()).then(|| id.to_string()),
                    );
                    r.group = group.chars().next();
                    r.watch = (!watch.is_empty()).then(|| watch.to_string());
                    r
                }
                [id, cwd, ..] => Restore::new(PathBuf::from(cwd), Some(id.to_string())),
                _ => Restore::new(PathBuf::from(line), None),
            }
        })
        .filter(|r| r.cwd.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fleet_next_to_its_target_dir_watches_the_build_output() {
        let dir = env::temp_dir().join(format!("fleet-src-{}", std::process::id()));
        let own = dir.join("claude-fleet.exe");
        let built = dir.join("target").join("release").join("claude-fleet.exe");
        fs::create_dir_all(built.parent().unwrap()).unwrap();
        fs::write(&own, b"a").unwrap();
        fs::write(&built, b"b").unwrap();

        assert_eq!(resolve_source(&own), built);
        // Running out of the build output itself has nothing newer to watch.
        assert_eq!(resolve_source(&built), built);
        assert!(source_warning(&built, &built).is_some());
        assert!(source_warning(&own, &built).is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fleet_on_its_own_watches_itself() {
        let dir = env::temp_dir().join(format!("fleet-lone-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let own = dir.join("claude-fleet.exe");
        fs::write(&own, b"a").unwrap();

        assert_eq!(resolve_source(&own), own);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restart_request_runs_it_again_and_anything_else_stops() {
        let mut codes = vec![Some(RESTART_EXIT), Some(RESTART_EXIT), Some(0)].into_iter();
        let runs = run_until_done(|| Ok(codes.next().unwrap())).unwrap();
        assert_eq!(runs, 3);
    }

    #[test]
    fn an_ordinary_exit_runs_it_once() {
        let runs = run_until_done(|| Ok(Some(0))).unwrap();
        assert_eq!(runs, 1);
        // A child killed outright reports no code at all, which is not a
        // request to come back.
        let runs = run_until_done(|| Ok(None)).unwrap();
        assert_eq!(runs, 1);
    }

    #[test]
    fn a_failure_to_launch_stops_rather_than_spinning() {
        let e = run_until_done(|| anyhow::bail!("cannot be done"));
        assert!(e.is_err());
    }

    #[test]
    fn the_restore_list_is_handed_over_once() {
        let dir = env::temp_dir().join(format!("fleet-restore-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("restore.txt");
        // SAFETY: single-threaded test, and the variable is this test's own.
        unsafe { env::set_var(RESTORE_VAR, &file) };

        let mut grouped = Restore::new(dir.clone(), None);
        grouped.group = Some('a');
        let mut watcher = Restore::new(dir.clone(), Some("bb-1".into()));
        watcher.watch = Some("*".into());
        let items = vec![
            Restore::new(dir.clone(), Some("abc-123".into())),
            Restore::new(dir.clone(), None),
            grouped,
            watcher,
        ];
        write_restore(&items);
        assert_eq!(take_restore(), items);
        // Gone now: the list describes one restart, not every later start.
        assert!(take_restore().is_empty());

        // A list written by an older fleet is a plain directory per line.
        fs::write(&file, format!("{}\n", dir.display())).unwrap();
        assert_eq!(take_restore(), vec![Restore::new(dir.clone(), None)]);

        unsafe { env::remove_var(RESTORE_VAR) };
        let _ = fs::remove_dir_all(&dir);
    }
}
