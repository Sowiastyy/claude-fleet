//! Claude Fleet as a desktop app.
//!
//! The window is a web page (`web/`); this side runs the Claude Code
//! processes, reads Claude Code's local state through `fleet_core`, and talks
//! to the page with Tauri commands one way and events the other.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod hub;
mod settings;

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime},
};

use base64::Engine;
use fleet_core::{accounts::Accounts, commitmsg, git, history, registry, usage};
use hub::{NewSession, SessionView, Shared, View};
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{Emitter, Manager, State};

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

struct App {
    hub: Shared,
    accounts: Mutex<Accounts>,
    limits: Mutex<usage::Watch>,
    settings_path: PathBuf,
}

// ---------- sessions

#[tauri::command]
fn sessions_list(app: State<'_, Arc<App>>) -> Vec<SessionView> {
    app.hub.lock().unwrap().list()
}

#[tauri::command]
fn session_log(app: State<'_, Arc<App>>, id: String) -> Vec<Value> {
    app.hub.lock().unwrap().log(&id)
}

#[tauri::command]
fn session_create(app: State<'_, Arc<App>>, req: NewSession) -> Res<String> {
    hub::create(&app.hub, req).map_err(err)
}

#[tauri::command]
fn session_send(app: State<'_, Arc<App>>, id: String, text: String, images: Option<Vec<hub::Image>>) -> Res<()> {
    hub::send(&app.hub, &id, &text, &images.unwrap_or_default()).map_err(err)
}

#[tauri::command]
fn session_interrupt(app: State<'_, Arc<App>>, id: String) -> Res<()> {
    hub::interrupt(&app.hub, &id).map_err(err)
}

#[tauri::command]
fn session_set_view(app: State<'_, Arc<App>>, id: String, view: View, rows: u16, cols: u16) -> Res<()> {
    hub::set_view(&app.hub, &id, view, rows, cols).map_err(err)
}

#[tauri::command]
fn session_allow(app: State<'_, Arc<App>>, id: String, tool: String) -> Res<()> {
    hub::allow_tool(&app.hub, &id, &tool).map_err(err)
}

#[tauri::command]
fn session_configure(
    app: State<'_, Arc<App>>,
    id: String,
    model: Option<String>,
    permission_mode: Option<String>,
) -> Res<()> {
    hub::configure(&app.hub, &id, model, permission_mode).map_err(err)
}

#[tauri::command]
fn session_close(app: State<'_, Arc<App>>, id: String) -> Res<()> {
    hub::close(&app.hub, &id).map_err(err)
}

#[tauri::command]
fn term_write(app: State<'_, Arc<App>>, id: String, data: String) -> Res<()> {
    hub::term_write(&app.hub, &id, &data).map_err(err)
}

#[tauri::command]
fn term_resize(app: State<'_, Arc<App>>, id: String, rows: u16, cols: u16) -> Res<()> {
    hub::term_resize(&app.hub, &id, rows, cols).map_err(err)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Foreign {
    name: String,
    cwd: String,
    status: String,
}

/// Sessions started outside this app: shown, not driven.
#[tauri::command]
fn foreign_sessions(app: State<'_, Arc<App>>) -> Vec<Foreign> {
    let ours = app.hub.lock().unwrap().our_pids();
    registry::read_all()
        .into_iter()
        .filter(|e| !ours.contains(&e.pid))
        .map(|e| Foreign {
            name: e.name.clone(),
            cwd: e.cwd.clone(),
            status: e.status.clone(),
        })
        .collect()
}

// ---------- accounts and limits

#[derive(Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct WindowView {
    pct: u8,
    resets_in_secs: Option<u64>,
    expired: bool,
}

#[derive(Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct LimitsView {
    session: Option<WindowView>,
    weekly: Option<WindowView>,
    fetched_ago_secs: u64,
}

fn limits_view(u: &usage::Usage) -> LimitsView {
    let w = |w: &Option<usage::Window>| {
        w.as_ref().map(|w| WindowView {
            pct: w.pct,
            resets_in_secs: w.resets_in.map(|d| d.as_secs()),
            expired: w.expired,
        })
    };
    LimitsView {
        session: w(&u.session),
        weekly: w(&u.weekly),
        fetched_ago_secs: u.fetched_ago.as_secs(),
    }
}

#[derive(Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct AccountView {
    uuid: String,
    email: String,
    name: String,
    org: String,
    plan: String,
    needs_login: bool,
    current: bool,
    limits: Option<LimitsView>,
}

#[derive(Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct AccountsView {
    current: Option<String>,
    accounts: Vec<AccountView>,
    /// The signed-in account's limits as Claude Code cached them last.
    live: Option<LimitsView>,
}

fn accounts_view(app: &App) -> AccountsView {
    let accounts = app.accounts.lock().unwrap();
    let mut limits = app.limits.lock().unwrap();
    limits.refresh();
    let live = limits.current.as_ref().map(limits_view);
    AccountsView {
        current: accounts.current.clone(),
        accounts: accounts
            .list
            .iter()
            .map(|a| {
                let current = accounts.current.as_deref() == Some(a.uuid.as_str());
                AccountView {
                    uuid: a.uuid.clone(),
                    email: a.email.clone(),
                    name: a.name.clone(),
                    org: a.org.clone(),
                    plan: a.plan.clone(),
                    needs_login: a.needs_login,
                    current,
                    limits: if current {
                        live.clone().or_else(|| a.usage.as_ref().map(limits_view))
                    } else {
                        a.usage.as_ref().map(limits_view)
                    },
                }
            })
            .collect(),
        live,
    }
}

#[tauri::command]
fn accounts_state(app: State<'_, Arc<App>>) -> AccountsView {
    accounts_view(&app)
}

/// Sign in as another saved account. With `carry`, every running session
/// restarts as it on its own conversation.
#[tauri::command]
fn accounts_switch(handle: tauri::AppHandle, app: State<'_, Arc<App>>, uuid: String, carry: bool) -> Res<String> {
    let email = app.accounts.lock().unwrap().switch(&uuid).map_err(err)?;
    let moved = if carry { hub::restart_all(&app.hub) } else { 0 };
    let _ = handle.emit("fleet://accounts", accounts_view(&app));
    Ok(match moved {
        0 => format!("Signed in as {email}"),
        1 => format!("Signed in as {email}, 1 session moved over"),
        n => format!("Signed in as {email}, {n} sessions moved over"),
    })
}

#[tauri::command]
fn accounts_forget(handle: tauri::AppHandle, app: State<'_, Arc<App>>, uuid: String) -> Res<()> {
    app.accounts.lock().unwrap().forget(&uuid).map_err(err)?;
    let _ = handle.emit("fleet://accounts", accounts_view(&app));
    Ok(())
}

/// A terminal session with `/login` typed into it.
#[tauri::command]
fn accounts_add(app: State<'_, Arc<App>>, cwd: String) -> Res<String> {
    hub::create(
        &app.hub,
        NewSession {
            cwd,
            terminal: true,
            type_in: Some("/login".to_string()),
            ..Default::default()
        },
    )
    .map_err(err)
}

// ---------- history and projects

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Recent {
    id: String,
    cwd: String,
    project: String,
    summary: String,
    ago_secs: u64,
}

#[tauri::command]
fn recent_conversations() -> Vec<Recent> {
    let now = SystemTime::now();
    history::recent(12)
        .into_iter()
        .map(|c| Recent {
            id: c.id.clone(),
            cwd: c.cwd.display().to_string(),
            project: c.cwd_label(),
            summary: c.summary.clone(),
            ago_secs: now.duration_since(c.modified).unwrap_or_default().as_secs(),
        })
        .collect()
}

#[tauri::command]
fn recent_projects() -> Vec<String> {
    registry::recent_cwds(12)
        .into_iter()
        .map(|p| p.display().to_string())
        .collect()
}

#[tauri::command]
fn home_dir() -> String {
    dirs::home_dir().unwrap_or_default().display().to_string()
}

// ---------- git

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GitChange {
    code: String,
    path: String,
    added: Option<u32>,
    removed: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GitCommit {
    hash: String,
    subject: String,
    ago_secs: u64,
    author: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GitView {
    root: String,
    branch: Option<String>,
    upstream: Option<String>,
    ahead: u32,
    behind: u32,
    changes: Vec<GitChange>,
    changes_total: usize,
    added: u32,
    removed: u32,
    log: Vec<GitCommit>,
}

#[tauri::command]
async fn git_state(cwd: String) -> Option<GitView> {
    let git::State::Repo(s) = git::read(Path::new(&cwd)) else {
        return None;
    };
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Some(GitView {
        root: s.root.display().to_string(),
        branch: s.branch,
        upstream: s.upstream,
        ahead: s.ahead,
        behind: s.behind,
        changes: s
            .changes
            .into_iter()
            .map(|c| GitChange {
                code: c.code,
                path: c.path,
                added: c.added,
                removed: c.removed,
            })
            .collect(),
        changes_total: s.changes_total,
        added: s.added,
        removed: s.removed,
        log: s
            .log
            .into_iter()
            .take(30)
            .map(|c| GitCommit {
                hash: c.hash,
                subject: c.subject,
                ago_secs: now.saturating_sub(c.time),
                author: c.author,
            })
            .collect(),
    })
}

#[tauri::command]
async fn git_commit(cwd: String, message: String) -> Res<String> {
    let root = git::root_of(Path::new(&cwd));
    let message = if message.trim().is_empty() {
        commitmsg::generate(&root, commit_model())?
    } else {
        message
    };
    git::commit(&root, &message)
}

#[tauri::command]
async fn git_generate(cwd: String) -> Res<String> {
    commitmsg::generate(&git::root_of(Path::new(&cwd)), commit_model())
}

#[tauri::command]
async fn git_push(cwd: String) -> Res<()> {
    let root = git::root_of(Path::new(&cwd));
    let git::State::Repo(s) = git::read(&root) else {
        return Err("not a repository".into());
    };
    let branch = s.branch.ok_or("HEAD is detached")?;
    git::push(&root, &branch, s.upstream.is_some())
        .map_err(|f| format!("{}: {}", f.kind.name(), f.reason))
}

#[tauri::command]
async fn git_pull(cwd: String) -> Res<()> {
    git::pull(&git::root_of(Path::new(&cwd)))
}

fn commit_model() -> &'static str {
    "claude-haiku-4-5"
}

// ---------- settings

#[tauri::command]
fn settings_get(app: State<'_, Arc<App>>) -> settings::Settings {
    settings::load(&app.settings_path)
}

#[tauri::command]
fn settings_set(app: State<'_, Arc<App>>, settings: settings::Settings) -> Res<()> {
    settings::save(&app.settings_path, &settings).map_err(err)
}

/// Copy a picked image next to the settings, so moving or deleting the
/// original does not take the background with it. Returns its data URL.
#[tauri::command]
fn background_import(app: State<'_, Arc<App>>, path: String) -> Res<String> {
    let src = PathBuf::from(&path);
    let ext = src
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if !["png", "jpg", "jpeg", "webp", "gif", "svg"].contains(&ext.as_str()) {
        return Err("pick a png, jpg, webp, gif or svg".into());
    }
    let dir = app.settings_path.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::create_dir_all(&dir).map_err(err)?;
    let dest = dir.join(format!("background.{ext}"));
    std::fs::copy(&src, &dest).map_err(err)?;
    let mut s = settings::load(&app.settings_path);
    s.background = "custom".into();
    s.custom_image = Some(dest.display().to_string());
    settings::save(&app.settings_path, &s).map_err(err)?;
    data_url(&dest)
}

#[tauri::command]
fn background_custom(app: State<'_, Arc<App>>) -> Option<String> {
    let s = settings::load(&app.settings_path);
    data_url(Path::new(s.custom_image.as_deref()?)).ok()
}

fn data_url(path: &Path) -> Res<String> {
    let bytes = std::fs::read(path).map_err(err)?;
    let mime = match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    };
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

// ---------- the poller

/// Once a second: Claude Code's registry for terminal sessions' status and
/// the sessions started elsewhere, the account files, the limits.
fn poll(handle: tauri::AppHandle, app: Arc<App>) {
    let mut last_accounts: Option<AccountsView> = None;
    loop {
        let entries = registry::read_all();
        hub::sync_registry(&app.hub, &entries);
        let added = app.accounts.lock().unwrap().poll();
        if let Some(email) = added {
            let _ = handle.emit("fleet://toast", format!("Account saved: {email}"));
        }
        let view = accounts_view(&app);
        if last_accounts.as_ref() != Some(&view) {
            let _ = handle.emit("fleet://accounts", &view);
            last_accounts = Some(view);
        }
        let _ = handle.emit("fleet://tick", json!({}));
        thread::sleep(Duration::from_secs(1));
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|tauri_app| {
            let handle = tauri_app.handle().clone();
            let dir = tauri_app
                .path()
                .app_config_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("claude-fleet-gui"));
            let app = Arc::new(App {
                hub: hub::Hub::new(handle.clone()),
                accounts: Mutex::new(Accounts::new()),
                limits: Mutex::new(usage::Watch::new()),
                settings_path: dir.join("settings.json"),
            });
            tauri_app.manage(app.clone());
            thread::spawn(move || poll(handle, app));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            sessions_list,
            session_log,
            session_create,
            session_send,
            session_interrupt,
            session_set_view,
            session_allow,
            session_configure,
            session_close,
            term_write,
            term_resize,
            foreign_sessions,
            accounts_state,
            accounts_switch,
            accounts_forget,
            accounts_add,
            recent_conversations,
            recent_projects,
            home_dir,
            git_state,
            git_commit,
            git_generate,
            git_push,
            git_pull,
            settings_get,
            settings_set,
            background_import,
            background_custom,
        ])
        .run(tauri::generate_context!())
        .expect("the window could not be opened");
}
