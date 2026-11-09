//! Keeping the GUI up to date, the way the terminal app keeps itself: the
//! same GitHub release carries `claude-fleet-gui.exe` next to the terminal
//! app, and `fleet_core::update` checks for it and swaps it in.
//!
//! A restart closes the window's sessions and starts the new build on the
//! same conversations: what was open is written down first and opened again
//! by the next start.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use fleet_core::{
    history,
    update::{self, Release},
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::hub::{self, NewSession, Shared, View};

/// The asset `release.yml` uploads for the GUI.
const ASSET: &str = "claude-fleet-gui.exe";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// How often to ask GitHub while the window stays open.
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// A restore file older than this is from a crash or an old run, not a
/// restart, and is ignored.
const RESTORE_FRESH_MS: u64 = 5 * 60 * 1000;

#[derive(Serialize, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateView {
    pub current: String,
    /// The newer release's tag, once one is found.
    pub available: Option<String>,
    pub installing: bool,
    /// The new build is on disk; a restart runs it.
    pub installed: bool,
    pub checking: bool,
}

pub struct Updater {
    /// The executable as it was started, before any update moved it aside.
    origin: PathBuf,
    release: Option<Release>,
    pub view: UpdateView,
}

pub type SharedUpdater = Arc<Mutex<Updater>>;

impl Updater {
    pub fn new() -> SharedUpdater {
        let origin = std::env::current_exe().unwrap_or_default();
        update::sweep(&origin);
        Arc::new(Mutex::new(Self {
            origin,
            release: None,
            view: UpdateView {
                current: CURRENT.to_string(),
                ..Default::default()
            },
        }))
    }
}

fn emit(app: &AppHandle, u: &Updater) {
    let _ = app.emit("fleet://update", &u.view);
}

/// Ask GitHub once. Returns the view after.
pub fn check(app: &AppHandle, up: &SharedUpdater) -> Result<UpdateView> {
    let origin = {
        let mut u = up.lock().unwrap();
        if u.view.installed || u.view.installing {
            return Ok(u.view.clone());
        }
        u.view.checking = true;
        emit(app, &u);
        u.origin.clone()
    };
    let found = update::check(ASSET, CURRENT, update::dev_repo(&origin).as_deref());
    let mut u = up.lock().unwrap();
    u.view.checking = false;
    match found {
        Ok(rel) => {
            u.view.available = rel.as_ref().map(|r| r.tag.clone());
            u.release = rel;
        }
        Err(e) => {
            emit(app, &u);
            return Err(e);
        }
    }
    emit(app, &u);
    Ok(u.view.clone())
}

/// Check a little after start, then every few hours.
pub fn watch(app: AppHandle, up: SharedUpdater) {
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(15));
        loop {
            let _ = check(&app, &up);
            thread::sleep(EVERY);
        }
    });
}

/// Download the release found and put it in place of the running file.
pub fn install(app: &AppHandle, up: &SharedUpdater) -> Result<UpdateView> {
    let (rel, origin) = {
        let mut u = up.lock().unwrap();
        let rel = u.release.clone().context("no newer release")?;
        u.view.installing = true;
        emit(app, &u);
        (rel, u.origin.clone())
    };
    let done = update::install(&rel, &origin, CURRENT);
    let mut u = up.lock().unwrap();
    u.view.installing = false;
    u.view.installed = done.is_ok();
    emit(app, &u);
    done.map(|()| u.view.clone())
}

/// One session as the next start reopens it.
#[derive(Serialize, Deserialize)]
struct Reopen {
    cwd: String,
    conversation: String,
    view: View,
    model: String,
    permission_mode: String,
    /// A cloud session comes back attached to its id; one whose id never
    /// showed does not come back (it would start a second cloud session).
    #[serde(default)]
    cloud: bool,
    #[serde(default)]
    cloud_id: String,
}

#[derive(Serialize, Deserialize)]
struct Restore {
    written_ms: u64,
    sessions: Vec<Reopen>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Write down the open sessions, close them, start the executable on disk
/// (the new build, after an install) and leave.
pub fn restart(app: &AppHandle, up: &SharedUpdater, shared: &Shared, restore_path: &Path) -> Result<()> {
    let origin = up.lock().unwrap().origin.clone();
    let sessions: Vec<Reopen> = {
        let hub = shared.lock().unwrap();
        hub.list()
            .into_iter()
            .map(|s| Reopen {
                cwd: s.cwd,
                conversation: s.conversation,
                view: s.view,
                model: s.model,
                permission_mode: s.permission_mode,
                cloud: s.cloud,
                cloud_id: s.cloud_id,
            })
            .collect()
    };
    let restore = Restore {
        written_ms: now_ms(),
        sessions,
    };
    if let Some(dir) = restore_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(restore_path, serde_json::to_string(&restore)?)?;
    std::process::Command::new(&origin)
        .spawn()
        .with_context(|| format!("could not start {}", origin.display()))?;
    let ids: Vec<String> = shared.lock().unwrap().list().into_iter().map(|s| s.id).collect();
    for id in ids {
        let _ = hub::close(shared, &id);
    }
    app.exit(0);
    Ok(())
}

/// Reopen what a restart wrote down, once.
pub fn reopen(shared: &Shared, restore_path: &Path) {
    let Ok(raw) = std::fs::read_to_string(restore_path) else {
        return;
    };
    let _ = std::fs::remove_file(restore_path);
    let Ok(r) = serde_json::from_str::<Restore>(&raw) else {
        return;
    };
    if now_ms().saturating_sub(r.written_ms) > RESTORE_FRESH_MS {
        return;
    }
    for s in r.sessions {
        if s.cloud {
            if !s.cloud_id.is_empty() {
                let _ = hub::create(
                    shared,
                    NewSession {
                        cwd: s.cwd,
                        cloud: true,
                        resume: Some(s.cloud_id),
                        ..Default::default()
                    },
                );
            }
            continue;
        }
        // A session that never got to say anything has no transcript to
        // resume; it comes back fresh.
        let resume = history::exists(&s.conversation).then_some(s.conversation);
        let _ = hub::create(
            shared,
            NewSession {
                cwd: s.cwd,
                resume,
                model: (!s.model.is_empty()).then_some(s.model),
                permission_mode: (!s.permission_mode.is_empty()).then_some(s.permission_mode),
                terminal: s.view == View::Terminal,
                ..Default::default()
            },
        );
    }
}
