//! Several Claude accounts on one machine, swapped between without a logout.
//!
//! Claude Code keeps who it is signed in as in two places: the OAuth tokens
//! (`claudeAiOauth` in `~/.claude/.credentials.json`, or the login keychain on
//! macOS) and the account they belong to (`oauthAccount` in `~/.claude.json`).
//! Everything else — transcripts, settings, the session registry, MCP servers
//! and their own tokens — is the machine's, not the account's.
//!
//! So an account here is a profile holding a copy of exactly those two
//! things, one file per account under `~/.claude/fleet-accounts/`, and
//! switching is putting a profile's copy back where Claude Code reads it. The
//! rest of `.credentials.json` (`mcpOAuth`, notably) is left alone, and so is
//! every other key of `~/.claude.json`.
//!
//! The copy of the account in use has to be kept current: Claude Code rotates
//! the refresh token whenever it refreshes, and a profile holding the one it
//! replaced could no longer sign in. Both files are therefore watched, and a
//! change is copied into the profile of whoever is signed in once the files
//! have settled.
//!
//! An account is added by logging in to it — `/login` in any session. The
//! fleet notices a new `accountUuid` and makes it a profile like the others.
//!
//! Several accounts at once: a `claude` started with `CLAUDE_CONFIG_DIR`
//! pointing at an account's own home (`~/.claude/fleet-accounts/<uuid>/`, see
//! [`Accounts::config_dir_for`]) is that account whoever is signed in. The
//! home holds its own `.credentials.json` and `.claude.json`; everything else
//! in it is a link back to `~/.claude` (a symlink, or on Windows a junction
//! for a folder and a hard link for a file), so transcripts, the session
//! registry, settings and history stay the machine's. A token one home's
//! `claude` refreshes is carried to the profile and the other places the
//! account lives by [`Accounts::poll`], before anyone else needs it.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::usage;

/// Where the profiles live, under `~/.claude`.
const PROFILES: &str = "fleet-accounts";

/// How long both files have to sit still before a change is read. `/login`
/// writes the tokens and the account one after the other, and reading between
/// the two would file the new tokens under the old account.
const SETTLE: Duration = Duration::from_secs(2);

/// Files that never sit still — `~/.claude.json` is written by every session
/// for reasons of its own — are read after this long anyway.
const SETTLE_MAX: Duration = Duration::from_secs(10);

/// New tokens under an unchanged account, when the old ones were nowhere near
/// expiring, look like a login to another account that has not written its
/// `oauthAccount` yet (that takes a profile fetch). They wait this long for
/// the account to change before they are taken as the same account's.
const HOLD: Duration = Duration::from_secs(20);

/// Claude Code refreshes a token shortly before it runs out. A rotation this
/// close to the old token's expiry is that refresh.
const REFRESH_DUE_MS: u64 = 15 * 60 * 1000;

/// The keychain has no mtime to watch, so on macOS it is simply read again
/// this often.
#[cfg(target_os = "macos")]
const KEYCHAIN_POLL: Duration = Duration::from_secs(30);

/// One saved account, as the dropdown shows it.
#[derive(Clone, Debug)]
pub struct Account {
    pub uuid: String,
    pub email: String,
    /// What the person is called, as the account says; may be empty.
    pub name: String,
    /// The organisation the login is for; personal ones are named after the
    /// email by Anthropic, which makes them noise.
    pub org: String,
    /// `max`, `pro`, `team`… as the tokens say.
    pub plan: String,
    /// The last limits seen while this account was the one signed in.
    pub usage: Option<usage::Usage>,
    /// The saved refresh token has run out: only `/login` brings it back.
    pub needs_login: bool,
}

/// What reading the live files came to.
#[derive(Debug, PartialEq)]
enum Synced {
    /// Saved, or nothing to save. Carries the email of an account seen for the
    /// first time.
    Done(Option<String>),
    /// Unexplained new tokens; see [`HOLD`].
    Hold,
}

/// Whether live tokens may be filed under the profile that holds `saved`.
#[derive(Debug, PartialEq)]
enum Decision {
    Take,
    Hold,
}

fn decide(saved: Option<&Value>, live: &Value, now_ms: u64) -> Decision {
    let Some(saved) = saved else {
        return Decision::Take;
    };
    let token = |v: &Value| {
        v.get("refreshToken")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    if token(saved) == token(live) {
        return Decision::Take;
    }
    let due = saved
        .get("expiresAt")
        .and_then(Value::as_u64)
        .is_none_or(|at| at <= now_ms + REFRESH_DUE_MS);
    if due { Decision::Take } else { Decision::Hold }
}

pub struct Accounts {
    /// Every saved account, by email.
    pub list: Vec<Account>,
    /// The account Claude Code is signed in as right now.
    pub current: Option<String>,
    /// What went wrong the last time the files were read, once.
    pub problem: Option<String>,
    paths: Paths,
    stamp: (Option<SystemTime>, Option<SystemTime>),
    first_change: Option<Instant>,
    last_change: Instant,
    #[cfg(target_os = "macos")]
    last_keychain: Instant,
}

impl Default for Accounts {
    fn default() -> Self {
        Self::new()
    }
}

impl Accounts {
    pub fn new() -> Self {
        Self::with(Paths::real())
    }

    /// Accounts that read and write nowhere: for an app built in a test,
    /// which must not file the developer's own login.
    pub fn detached() -> Self {
        Self::with(Paths {
            claude: std::env::temp_dir().join("fleet-accounts-nowhere"),
            global: std::env::temp_dir().join("fleet-accounts-nowhere.json"),
            keychain: false,
        })
    }

    fn with(paths: Paths) -> Self {
        let mut a = Self {
            list: Vec::new(),
            current: None,
            problem: None,
            stamp: paths.stamps(),
            paths,
            first_change: None,
            last_change: Instant::now(),
            #[cfg(target_os = "macos")]
            last_keychain: Instant::now(),
        };
        // Whatever is signed in at start is taken as it is: there is no
        // half-written login to wait out from before we were running.
        a.sync_reporting(true);
        a
    }

    pub fn current_account(&self) -> Option<&Account> {
        let uuid = self.current.as_deref()?;
        self.list.iter().find(|a| a.uuid == uuid)
    }

    /// Look at the files and copy a settled change into its profile. Returns
    /// the email of an account that showed up for the first time.
    pub fn poll(&mut self) -> Option<String> {
        if let Err(e) = self.reconcile() {
            self.problem = Some(format!("{e:#}"));
        }
        let now = self.paths.stamps();
        if now != self.stamp {
            self.stamp = now;
            self.last_change = Instant::now();
            self.first_change.get_or_insert_with(Instant::now);
        }
        #[cfg(target_os = "macos")]
        if self.paths.keychain && self.last_keychain.elapsed() >= KEYCHAIN_POLL {
            self.last_keychain = Instant::now();
            self.first_change.get_or_insert_with(Instant::now);
        }
        let first = self.first_change?;
        if self.last_change.elapsed() < SETTLE && first.elapsed() < SETTLE_MAX {
            return None;
        }
        match self.sync_reporting(first.elapsed() >= HOLD) {
            Synced::Hold => None,
            Synced::Done(added) => {
                self.first_change = None;
                added
            }
        }
    }

    fn sync_reporting(&mut self, force: bool) -> Synced {
        match self.sync(force) {
            Ok(s) => s,
            Err(e) => {
                self.problem = Some(format!("{e:#}"));
                self.first_change = None;
                Synced::Done(None)
            }
        }
    }

    /// Copy the live tokens and account into that account's profile.
    fn sync(&mut self, force: bool) -> Result<Synced> {
        let Some(global) = read_json(&self.paths.global).ok().flatten() else {
            self.current = None;
            self.reload();
            return Ok(Synced::Done(None));
        };
        let account = global.get("oauthAccount").filter(|v| v.is_object());
        let uuid = account
            .and_then(|a| a.get("accountUuid"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.current = uuid.clone();
        let (Some(uuid), Some(account)) = (uuid, account) else {
            // Logged out, or on an API key: nothing to keep.
            self.reload();
            return Ok(Synced::Done(None));
        };
        let Some((store, _)) = self.paths.read_store()? else {
            self.reload();
            return Ok(Synced::Done(None));
        };
        let Some(creds) = store.get("claudeAiOauth").filter(|v| v.is_object()) else {
            self.reload();
            return Ok(Synced::Done(None));
        };

        let path = self.paths.profile(&uuid).context("not an account id")?;
        let old = read_json(&path)?;
        // The account's own home refreshed its token since: the live one is
        // the stale copy, and gets the new one rather than replace it.
        if let Some(saved) = old.as_ref().and_then(|o| o.get("claudeAiOauth"))
            && refresh_token(saved) != refresh_token(creds)
            && expires_at(saved) > expires_at(creds)
        {
            let (mut store, kind) = self.paths.read_store()?.unwrap_or((json!({}), Store::default()));
            store["claudeAiOauth"] = saved.clone();
            self.paths.write_store(kind, &store)?;
            self.stamp = self.paths.stamps();
            self.reload();
            return Ok(Synced::Done(None));
        }
        if !force
            && decide(
                old.as_ref().and_then(|o| o.get("claudeAiOauth")),
                creds,
                now_ms(),
            ) == Decision::Hold
        {
            return Ok(Synced::Hold);
        }

        let mut profile = old.clone().unwrap_or_else(|| json!({}));
        profile["claudeAiOauth"] = creds.clone();
        profile["oauthAccount"] = account.clone();
        if let Some(cached) = global
            .get("cachedUsageUtilization")
            .filter(|c| c.get("accountUuid").and_then(Value::as_str) == Some(uuid.as_str()))
        {
            profile["usage"] = cached.clone();
        }
        // The profile started as a copy of the old one, so equal means
        // nothing moved — the common case, as `~/.claude.json` changes for
        // a hundred reasons that are not the account.
        if old.as_ref() != Some(&profile) {
            profile["savedAtMs"] = json!(now_ms());
            write_atomic(
                &path,
                serde_json::to_string_pretty(&profile)?.as_bytes(),
                true,
            )?;
        }
        let added = old
            .is_none()
            .then(|| summarise(&profile).map(|a| a.email))
            .flatten();
        self.reload();
        Ok(Synced::Done(added))
    }

    /// Read the profiles back from disk.
    fn reload(&mut self) {
        let mut list: Vec<Account> = fs::read_dir(self.paths.profiles())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| read_json(&e.path()).ok().flatten())
            .filter_map(|v| summarise(&v))
            .collect();
        list.sort_by_key(|a| a.email.to_lowercase());
        self.list = list;
    }

    /// Sign Claude Code in as `uuid`. New `claude` processes start as that
    /// account from here on; the running ones hold their tokens in memory and
    /// have to be started again to follow (see `App::choose_account`).
    pub fn switch(&mut self, uuid: &str) -> Result<String> {
        // The account's own home may hold a newer token than its profile.
        self.reconcile()?;
        // What is signed in now is about to be overwritten: keep it first,
        // tokens refreshed a moment ago included.
        if let Err(e) = self.sync(true) {
            bail!("could not save the current account first: {e:#}");
        }
        if self.current.as_deref() == Some(uuid) {
            bail!("already signed in as that account");
        }
        let path = self.paths.profile(uuid).context("not an account id")?;
        let profile = read_json(&path)?.context("that account has no saved profile")?;
        let creds = profile
            .get("claudeAiOauth")
            .filter(|v| v.is_object())
            .context("the profile holds no tokens")?;
        let account = profile
            .get("oauthAccount")
            .filter(|v| v.is_object())
            .context("the profile holds no account")?;
        let email = summarise(&profile).map(|a| a.email).unwrap_or_default();

        let (mut store, kind) = self
            .paths
            .read_store()?
            .unwrap_or((json!({}), Store::default()));
        let before = store.clone();
        store["claudeAiOauth"] = creds.clone();
        self.paths.write_store(kind, &store)?;

        let global = (|| -> Result<()> {
            let path = &self.paths.global;
            let mut cfg = read_json(path)?.context("~/.claude.json is missing")?;
            cfg["oauthAccount"] = account.clone();
            write_atomic(path, serde_json::to_string_pretty(&cfg)?.as_bytes(), false)
        })();
        if let Err(e) = global {
            // Tokens of one account under the name of another is the one
            // state worse than not switching.
            let _ = self.paths.write_store(kind, &before);
            return Err(e);
        }

        self.current = Some(uuid.to_string());
        self.stamp = self.paths.stamps();
        self.first_change = None;
        self.reload();
        Ok(email)
    }

    /// Drop a saved account. The one signed in stays: its profile would be
    /// written again on the next token refresh anyway.
    pub fn forget(&mut self, uuid: &str) -> Result<()> {
        if self.current.as_deref() == Some(uuid) {
            bail!("that account is signed in — switch to another one first");
        }
        let path = self.paths.profile(uuid).context("not an account id")?;
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        // Its home goes too. Removing a directory does not follow the links
        // in it, so `~/.claude` itself is untouched.
        if let Some(home) = self.paths.home(uuid)
            && home.is_dir()
        {
            let _ = fs::remove_dir_all(&home);
        }
        self.reload();
        Ok(())
    }

    /// A Claude Code config dir that is `uuid`'s whoever is signed in: run
    /// `claude` with `CLAUDE_CONFIG_DIR` set to it. Made, or brought up to
    /// date, on every call: the links to `~/.claude`, the account's freshest
    /// tokens, and `~/.claude.json` as it is now under the account's name.
    pub fn config_dir_for(&mut self, uuid: &str) -> Result<PathBuf> {
        self.reconcile()?;
        let path = self.paths.profile(uuid).context("not an account id")?;
        let profile = read_json(&path)?.context("that account has no saved profile — log in to it again")?;
        let creds = profile
            .get("claudeAiOauth")
            .filter(|v| v.is_object())
            .context("the profile holds no tokens")?;
        let account = profile
            .get("oauthAccount")
            .filter(|v| v.is_object())
            .context("the profile holds no account")?;
        let home = self.paths.home(uuid).context("not an account id")?;
        fs::create_dir_all(&home).with_context(|| format!("creating {}", home.display()))?;
        link_shared(&self.paths.claude, &home)?;

        // The tokens: the profile's or the home's own, whichever is newer,
        // next to the machine's MCP logins.
        let cred_path = home.join(".credentials.json");
        let old = read_json(&cred_path).ok().flatten();
        let global = self.paths.read_store()?.map(|(v, _)| v).unwrap_or_else(|| json!({}));
        let mut store = old.clone().unwrap_or_else(|| json!({}));
        if let (Some(dst), Some(src)) = (store.as_object_mut(), global.as_object()) {
            for (k, v) in src {
                if k == "claudeAiOauth" {
                    continue;
                }
                match (dst.get_mut(k), v) {
                    // Per MCP server: a login the home made itself stays.
                    (Some(Value::Object(mine)), Value::Object(theirs)) => {
                        for (server, login) in theirs {
                            mine.entry(server.clone()).or_insert_with(|| login.clone());
                        }
                    }
                    (Some(_), _) => {}
                    (None, _) => {
                        dst.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        let fresh = match old.as_ref().and_then(|o| o.get("claudeAiOauth")) {
            Some(own) if expires_at(own) > expires_at(creds) => own.clone(),
            _ => creds.clone(),
        };
        store["claudeAiOauth"] = fresh;
        if old.as_ref() != Some(&store) {
            write_atomic(&cred_path, serde_json::to_string(&store)?.as_bytes(), true)?;
        }

        // `.claude.json`: the machine's, under this account's name, keeping
        // what only the home knows (its limits, folders trusted there).
        let cfg_path = home.join(".claude.json");
        let old = read_json(&cfg_path).ok().flatten();
        let mut cfg = read_json(&self.paths.global)?.unwrap_or_else(|| json!({}));
        cfg["oauthAccount"] = account.clone();
        let mine = |c: &Value| c.get("accountUuid").and_then(Value::as_str) == Some(uuid);
        if cfg.get("cachedUsageUtilization").is_some_and(|c| !mine(c))
            && let Some(o) = cfg.as_object_mut()
        {
            o.remove("cachedUsageUtilization");
        }
        if let Some(old) = &old {
            if let Some(c) = old.get("cachedUsageUtilization").filter(|c| mine(c)) {
                cfg["cachedUsageUtilization"] = c.clone();
            }
            if let Some(projects) = old.get("projects").and_then(Value::as_object) {
                for (dir, p) in projects {
                    if p.get("hasTrustDialogAccepted") != Some(&Value::Bool(true)) {
                        continue;
                    }
                    if !cfg.get("projects").is_some_and(Value::is_object) {
                        cfg["projects"] = json!({});
                    }
                    let all = &mut cfg["projects"];
                    if all.get(dir).is_some_and(Value::is_object) {
                        all[dir]["hasTrustDialogAccepted"] = Value::Bool(true);
                    } else {
                        all[dir] = p.clone();
                    }
                }
            }
        }
        if old.as_ref() != Some(&cfg) {
            write_atomic(&cfg_path, serde_json::to_string_pretty(&cfg)?.as_bytes(), false)?;
        }
        Ok(home)
    }

    /// Carry tokens refreshed in an account's own home to its profile, and
    /// the other way; and the limits the home's sessions fetched. An account
    /// that is also the one signed in gets them in `~/.claude` too, by
    /// [`Self::sync`].
    fn reconcile(&mut self) -> Result<()> {
        let mut moved = false;
        let homes = fs::read_dir(self.paths.profiles()).into_iter().flatten().flatten();
        for entry in homes {
            let home = entry.path();
            if !home.is_dir() {
                continue;
            }
            let uuid = entry.file_name().to_string_lossy().into_owned();
            let Some(path) = self.paths.profile(&uuid) else { continue };
            let Some(mut profile) = read_json(&path).ok().flatten() else { continue };
            let cfg = read_json(&home.join(".claude.json")).ok().flatten();
            // A `/login` to another account inside the home: none of ours.
            let named = cfg
                .as_ref()
                .and_then(|c| c.pointer("/oauthAccount/accountUuid"))
                .and_then(Value::as_str);
            if named.is_some_and(|n| n != uuid) {
                continue;
            }
            let before = profile.clone();
            let cred_path = home.join(".credentials.json");
            let store = read_json(&cred_path).ok().flatten();
            let own = store.as_ref().and_then(|s| s.get("claudeAiOauth")).filter(|v| v.is_object());
            let saved = profile.get("claudeAiOauth").filter(|v| v.is_object()).cloned();
            match (own, saved) {
                (Some(own), Some(saved)) if refresh_token(own) != refresh_token(&saved) => {
                    if expires_at(own) > expires_at(&saved) {
                        profile["claudeAiOauth"] = own.clone();
                    } else if expires_at(&saved) > expires_at(own) {
                        let mut s = store.clone().unwrap_or_else(|| json!({}));
                        s["claudeAiOauth"] = saved;
                        write_atomic(&cred_path, serde_json::to_string(&s)?.as_bytes(), true)?;
                    }
                }
                _ => {}
            }
            if let Some(c) = cfg
                .as_ref()
                .and_then(|c| c.get("cachedUsageUtilization"))
                .filter(|c| c.get("accountUuid").and_then(Value::as_str) == Some(uuid.as_str()))
            {
                let newer = |v: &Value| v.get("fetchedAt").and_then(Value::as_u64).unwrap_or(0);
                if profile.get("usage").is_none_or(|u| newer(c) >= newer(u)) {
                    profile["usage"] = c.clone();
                }
            }
            if profile != before {
                profile["savedAtMs"] = json!(now_ms());
                write_atomic(&path, serde_json::to_string_pretty(&profile)?.as_bytes(), true)?;
                moved = true;
            }
        }
        if moved {
            self.reload();
        }
        Ok(())
    }
}

fn refresh_token(creds: &Value) -> Option<&str> {
    creds.get("refreshToken").and_then(Value::as_str)
}

fn expires_at(creds: &Value) -> u64 {
    creds.get("expiresAt").and_then(Value::as_u64).unwrap_or(0)
}

/// Folders Claude Code fills that have to be the machine's for the fleet to
/// see sessions started in an account's home. Made in `~/.claude` before
/// they are linked, so a home never starts one of its own.
const SHARED_DIRS: &[&str] = &[
    "projects",
    "sessions",
    "todos",
    "shell-snapshots",
    "plans",
    "file-history",
    "session-env",
];

/// What an account's home keeps for itself.
fn own_to_home(name: &str) -> bool {
    name == ".credentials.json" || name == ".claude.json" || name == PROFILES || name.ends_with(".tmp")
}

/// Link everything in `~/.claude` into `home`, but what the home keeps.
fn link_shared(claude: &Path, home: &Path) -> Result<()> {
    for d in SHARED_DIRS {
        let _ = fs::create_dir_all(claude.join(d));
    }
    for entry in fs::read_dir(claude).with_context(|| format!("reading {}", claude.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if own_to_home(&name.to_string_lossy()) {
            continue;
        }
        let target = entry.path();
        let at = home.join(&name);
        let is_dir = target.is_dir();
        if let Ok(meta) = fs::symlink_metadata(&at) {
            if !is_dir && stale_copy(&target, &at, &meta) {
                let _ = fs::remove_file(&at);
            } else {
                continue;
            }
        }
        if let Err(e) = link(&target, &at, is_dir) {
            // One file that would not link is not worth refusing to start.
            if is_dir {
                return Err(e).with_context(|| format!("linking {} to {}", at.display(), target.display()));
            }
        }
    }
    Ok(())
}

/// A file in the home that is a hard link or a copy (Windows) and no longer
/// the file in `~/.claude`: that one was replaced since.
fn stale_copy(target: &Path, at: &Path, meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return false;
    }
    let (Ok(t), Ok(a)) = (fs::metadata(target), fs::metadata(at)) else {
        return false;
    };
    t.len() != a.len() || t.modified().ok() != a.modified().ok()
}

#[cfg(unix)]
fn link(target: &Path, at: &Path, _dir: bool) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(windows)]
fn link(target: &Path, at: &Path, dir: bool) -> std::io::Result<()> {
    if dir {
        junction::create(target, at)
    } else {
        fs::hard_link(target, at).or_else(|_| fs::copy(target, at).map(|_| ()))
    }
}

/// The parts of a profile the dropdown shows.
fn summarise(profile: &Value) -> Option<Account> {
    let account = profile.get("oauthAccount")?;
    let s = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or("").to_string();
    let uuid = s(account.get("accountUuid"));
    if uuid.is_empty() {
        return None;
    }
    let email = s(account.get("emailAddress"));
    let mut name = s(account.get("displayName"));
    if name.is_empty() {
        name = s(account.get("fullName"));
    }
    let mut org = s(account.get("organizationName"));
    if org.contains(&email) || org.ends_with("'s Organization") {
        org.clear();
    }
    let creds = profile.get("claudeAiOauth");
    let plan = s(creds.and_then(|c| c.get("subscriptionType")));
    let needs_login = creds
        .and_then(|c| c.get("refreshTokenExpiresAt"))
        .and_then(Value::as_u64)
        .is_some_and(|at| at <= now_ms());
    Some(Account {
        uuid,
        email,
        name,
        org,
        plan,
        usage: profile.get("usage").and_then(usage::from_cache),
        needs_login,
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Where Claude Code keeps what an account is, and where the profiles go.
/// Real ones in the home directory; a test points them at a scratch one.
struct Paths {
    /// `~/.claude`: the credentials file and the profiles under it.
    claude: PathBuf,
    /// `~/.claude.json`.
    global: PathBuf,
    /// Whether the tokens may live in the macOS keychain rather than the file.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    keychain: bool,
}

impl Paths {
    fn real() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            claude: home.join(".claude"),
            global: usage::cache_path().unwrap_or_else(|| home.join(".claude.json")),
            keychain: cfg!(target_os = "macos"),
        }
    }

    fn credentials(&self) -> PathBuf {
        self.claude.join(".credentials.json")
    }

    fn profiles(&self) -> PathBuf {
        self.claude.join(PROFILES)
    }

    fn profile(&self, uuid: &str) -> Option<PathBuf> {
        // The uuid comes out of a file; it names a file only once it is
        // plainly one.
        if uuid.is_empty() || !uuid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return None;
        }
        Some(self.profiles().join(format!("{uuid}.json")))
    }

    /// The account's own Claude Code config dir, next to its profile.
    fn home(&self, uuid: &str) -> Option<PathBuf> {
        self.profile(uuid).map(|p| p.with_extension(""))
    }

    fn stamps(&self) -> (Option<SystemTime>, Option<SystemTime>) {
        (mtime(&self.credentials()), mtime(&self.global))
    }

    /// The whole credentials object, `mcpOAuth` and all, and where it came
    /// from.
    fn read_store(&self) -> Result<Option<(Value, Store)>> {
        #[cfg(target_os = "macos")]
        if self.keychain
            && let Some(raw) = keychain::read()
        {
            let v = serde_json::from_str(&raw).context("the keychain entry is not JSON")?;
            return Ok(Some((v, Store::Keychain)));
        }
        Ok(read_json(&self.credentials())?.map(|v| (v, Store::File)))
    }

    fn write_store(&self, kind: Store, v: &Value) -> Result<()> {
        let raw = serde_json::to_string(v)?;
        match kind {
            Store::File => write_atomic(&self.credentials(), raw.as_bytes(), true),
            #[cfg(target_os = "macos")]
            Store::Keychain => keychain::write(&raw),
        }
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn read_json(path: &Path) -> Result<Option<Value>> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(Some(
            serde_json::from_str(&raw).with_context(|| format!("reading {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Write next to the target and rename over it, so a Claude Code reading the
/// file at that moment sees the old one or the new one, never half of either.
/// `private` keeps it to the owner on Unix: these files hold tokens.
fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("fleet-{}.tmp", std::process::id()));
    {
        use std::io::Write;
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = private;
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    // Windows refuses the rename while another process has the file open
    // without sharing deletes; Claude Code only ever holds it for a moment.
    let mut last = None;
    for _ in 0..20 {
        match fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
    let _ = fs::remove_file(&tmp);
    Err(last.expect("the loop ran")).with_context(|| format!("replacing {}", path.display()))
}

/// Where Claude Code keeps its tokens on this machine.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
enum Store {
    #[default]
    File,
    #[cfg(target_os = "macos")]
    Keychain,
}

/// The login keychain entry Claude Code keeps its tokens in on macOS.
#[cfg(target_os = "macos")]
mod keychain {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };

    use anyhow::{Context, Result, bail};

    const SERVICE: &str = "Claude Code-credentials";

    fn user() -> String {
        std::env::var("USER").unwrap_or_default()
    }

    pub fn read() -> Option<String> {
        let out = Command::new("security")
            .args(["find-generic-password", "-a", &user(), "-w", "-s", SERVICE])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8(out.stdout).ok()?;
        let s = s.trim();
        (!s.is_empty()).then(|| s.to_string())
    }

    /// Written the way Claude Code writes it: hex through `security -i` on
    /// stdin, so the tokens are not on a command line for `ps` to show.
    /// `security -i` reads lines of up to 4 KB; a longer entry goes as an
    /// argument after all.
    pub fn write(data: &str) -> Result<()> {
        let hex: String = data.bytes().map(|b| format!("{b:02x}")).collect();
        let user = user();
        let line = format!("add-generic-password -U -a \"{user}\" -s \"{SERVICE}\" -X \"{hex}\"\n");
        let status = if line.len() < 4000 {
            let mut child = Command::new("security")
                .arg("-i")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("running security")?;
            child
                .stdin
                .take()
                .context("no stdin")?
                .write_all(line.as_bytes())?;
            child.wait()?
        } else {
            Command::new("security")
                .args([
                    "add-generic-password",
                    "-U",
                    "-a",
                    &user,
                    "-s",
                    SERVICE,
                    "-X",
                    &hex,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("running security")?
        };
        if !status.success() {
            bail!("the keychain refused the new tokens");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(token: &str, expires_at: u64) -> Value {
        json!({"refreshToken": token, "expiresAt": expires_at})
    }

    #[test]
    fn a_first_sighting_is_always_kept() {
        assert_eq!(decide(None, &creds("a", 0), 0), Decision::Take);
    }

    #[test]
    fn the_same_tokens_are_kept() {
        let now = 1_000_000_000;
        let c = creds("a", now + 8 * 3_600_000);
        assert_eq!(decide(Some(&c), &c, now), Decision::Take);
    }

    #[test]
    fn a_rotation_when_the_old_token_was_running_out_is_a_refresh() {
        let now = 1_000_000_000;
        let saved = creds("a", now + 60_000);
        assert_eq!(
            decide(Some(&saved), &creds("b", now + 8 * 3_600_000), now),
            Decision::Take
        );
        let expired = creds("a", now - 60_000);
        assert_eq!(
            decide(Some(&expired), &creds("b", now), now),
            Decision::Take
        );
    }

    #[test]
    fn new_tokens_out_of_the_blue_wait_for_the_account_to_change() {
        let now = 1_000_000_000;
        let saved = creds("a", now + 6 * 3_600_000);
        assert_eq!(
            decide(Some(&saved), &creds("b", now + 8 * 3_600_000), now),
            Decision::Hold
        );
    }

    #[test]
    fn a_profile_reads_as_an_account() {
        let p = json!({
            "oauthAccount": {
                "accountUuid": "54ac1b76-1280-49b0-8599-e9e6ef005775",
                "emailAddress": "me@example.com",
                "organizationName": "me@example.com's Organization",
            },
            "claudeAiOauth": {"subscriptionType": "max", "refreshTokenExpiresAt": 1},
        });
        let a = summarise(&p).unwrap();
        assert_eq!(a.email, "me@example.com");
        assert_eq!(a.org, "", "a personal organisation is only the email again");
        assert_eq!(a.plan, "max");
        assert!(a.needs_login);
    }

    #[test]
    fn only_a_plain_uuid_names_a_file() {
        let p = Paths::real();
        assert!(p.profile("54ac1b76-1280-49b0-8599-e9e6ef005775").is_some());
        assert!(p.profile("../../etc/passwd").is_none());
        assert!(p.profile("").is_none());
    }

    /// A home directory of its own, with Claude Code signed in as `uuid`.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fleet-accounts-{name}-{}-{}",
                std::process::id(),
                now_ms()
            ));
            fs::create_dir_all(dir.join(".claude")).unwrap();
            Self(dir)
        }

        fn paths(&self) -> Paths {
            Paths {
                claude: self.0.join(".claude"),
                global: self.0.join(".claude.json"),
                keychain: false,
            }
        }

        fn sign_in(&self, uuid: &str, email: &str, token: &str) {
            let p = self.paths();
            let mut store = read_json(&p.credentials())
                .unwrap()
                .unwrap_or_else(|| json!({"mcpOAuth": {"server": {"accessToken": "mcp"}}}));
            store["claudeAiOauth"] = json!({
                "accessToken": format!("at-{token}"),
                "refreshToken": format!("rt-{token}"),
                "expiresAt": now_ms() + 8 * 3_600_000,
                "subscriptionType": "max",
            });
            fs::write(p.credentials(), store.to_string()).unwrap();
            let mut global = read_json(&p.global).unwrap().unwrap_or_else(
                || json!({"numStartups": 3, "projects": {"C:/x": {}}, "userID": "device"}),
            );
            global["oauthAccount"] = json!({"accountUuid": uuid, "emailAddress": email});
            fs::write(&p.global, serde_json::to_string_pretty(&global).unwrap()).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn switching_puts_back_the_tokens_and_the_account_and_nothing_else() {
        let home = Scratch::new("switch");
        home.sign_in("aaaa-1", "a@example.com", "a1");
        let mut accounts = Accounts::with(home.paths());
        assert_eq!(accounts.list.len(), 1);

        // `/login` to a second account; the first stays saved.
        home.sign_in("bbbb-2", "b@example.com", "b1");
        assert_eq!(
            accounts.sync(true).unwrap(),
            Synced::Done(Some("b@example.com".into()))
        );
        assert_eq!(accounts.list.len(), 2);
        assert_eq!(accounts.current.as_deref(), Some("bbbb-2"));

        // Back to the first one.
        assert_eq!(accounts.switch("aaaa-1").unwrap(), "a@example.com");
        let p = home.paths();
        let store = read_json(&p.credentials()).unwrap().unwrap();
        assert_eq!(store["claudeAiOauth"]["refreshToken"], "rt-a1");
        assert_eq!(
            store["mcpOAuth"]["server"]["accessToken"], "mcp",
            "the MCP servers' own tokens are the machine's, not the account's"
        );
        let global = read_json(&p.global).unwrap().unwrap();
        assert_eq!(global["oauthAccount"]["accountUuid"], "aaaa-1");
        assert_eq!(global["numStartups"], 3);
        let keys: Vec<&String> = global.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["numStartups", "projects", "userID", "oauthAccount"],
            "the file keeps Claude Code's order"
        );

        // The second account's tokens were kept on the way out, refreshed
        // ones included.
        let b = read_json(&p.profile("bbbb-2").unwrap()).unwrap().unwrap();
        assert_eq!(b["claudeAiOauth"]["refreshToken"], "rt-b1");
        assert_eq!(accounts.switch("bbbb-2").unwrap(), "b@example.com");
        let store = read_json(&p.credentials()).unwrap().unwrap();
        assert_eq!(store["claudeAiOauth"]["refreshToken"], "rt-b1");
    }

    #[test]
    fn a_refresh_is_kept_so_switching_back_still_signs_in() {
        let home = Scratch::new("refresh");
        home.sign_in("aaaa-1", "a@example.com", "a1");
        let mut accounts = Accounts::with(home.paths());
        home.sign_in("bbbb-2", "b@example.com", "b1");
        accounts.sync(true).unwrap();
        accounts.switch("aaaa-1").unwrap();

        // Claude Code refreshes the first account's token, rotating it.
        let p = home.paths();
        let mut store = read_json(&p.credentials()).unwrap().unwrap();
        store["claudeAiOauth"]["refreshToken"] = json!("rt-a2");
        fs::write(p.credentials(), store.to_string()).unwrap();

        // Switching away saves the rotated one before anything is overwritten.
        accounts.switch("bbbb-2").unwrap();
        let a = read_json(&p.profile("aaaa-1").unwrap()).unwrap().unwrap();
        assert_eq!(a["claudeAiOauth"]["refreshToken"], "rt-a2");
    }

    #[test]
    fn an_account_home_is_that_account_and_shares_the_rest() {
        let home = Scratch::new("home");
        home.sign_in("aaaa-1", "a@example.com", "a1");
        let mut accounts = Accounts::with(home.paths());
        home.sign_in("bbbb-2", "b@example.com", "b1");
        accounts.sync(true).unwrap();
        let p = home.paths();
        fs::write(p.claude.join("settings.json"), "{\"model\":\"x\"}").unwrap();

        // B is signed in; a session for A gets A's home.
        let dir = accounts.config_dir_for("aaaa-1").unwrap();
        let creds = read_json(&dir.join(".credentials.json")).unwrap().unwrap();
        assert_eq!(creds["claudeAiOauth"]["refreshToken"], "rt-a1");
        assert_eq!(creds["mcpOAuth"]["server"]["accessToken"], "mcp");
        let cfg = read_json(&dir.join(".claude.json")).unwrap().unwrap();
        assert_eq!(cfg["oauthAccount"]["accountUuid"], "aaaa-1");
        assert_eq!(cfg["numStartups"], 3);
        // The machine's state is shared, not copied.
        assert!(p.claude.join("projects").is_dir());
        fs::write(p.claude.join("projects").join("t.jsonl"), "x").unwrap();
        assert!(dir.join("projects").join("t.jsonl").exists());
        assert_eq!(fs::read_to_string(dir.join("settings.json")).unwrap(), "{\"model\":\"x\"}");
        // And `~/.claude` is still B.
        let live = read_json(&p.credentials()).unwrap().unwrap();
        assert_eq!(live["claudeAiOauth"]["refreshToken"], "rt-b1");

        // A's session refreshes its token: the profile gets it.
        let mut own = creds.clone();
        own["claudeAiOauth"]["refreshToken"] = json!("rt-a2");
        own["claudeAiOauth"]["expiresAt"] = json!(now_ms() + 9 * 3_600_000);
        fs::write(dir.join(".credentials.json"), own.to_string()).unwrap();
        accounts.reconcile().unwrap();
        let a = read_json(&p.profile("aaaa-1").unwrap()).unwrap().unwrap();
        assert_eq!(a["claudeAiOauth"]["refreshToken"], "rt-a2");

        // Signing A in uses the refreshed token, and then `~/.claude` and
        // A's home stay in step.
        accounts.switch("aaaa-1").unwrap();
        let live = read_json(&p.credentials()).unwrap().unwrap();
        assert_eq!(live["claudeAiOauth"]["refreshToken"], "rt-a2");
        own["claudeAiOauth"]["refreshToken"] = json!("rt-a3");
        own["claudeAiOauth"]["expiresAt"] = json!(now_ms() + 10 * 3_600_000);
        fs::write(dir.join(".credentials.json"), own.to_string()).unwrap();
        accounts.reconcile().unwrap();
        accounts.sync(false).unwrap();
        let live = read_json(&p.credentials()).unwrap().unwrap();
        assert_eq!(
            live["claudeAiOauth"]["refreshToken"], "rt-a3",
            "a refresh in the home reaches the signed-in copy"
        );

        // Forgetting B removes its home but not what it linked to.
        let b_dir = accounts.config_dir_for("bbbb-2").unwrap();
        accounts.forget("bbbb-2").unwrap();
        assert!(!b_dir.exists());
        assert!(p.claude.join("projects").join("t.jsonl").exists());
    }

    #[test]
    fn the_signed_in_account_cannot_be_forgotten() {
        let home = Scratch::new("forget");
        home.sign_in("aaaa-1", "a@example.com", "a1");
        let mut accounts = Accounts::with(home.paths());
        home.sign_in("bbbb-2", "b@example.com", "b1");
        accounts.sync(true).unwrap();
        assert!(accounts.forget("bbbb-2").is_err());
        accounts.forget("aaaa-1").unwrap();
        assert_eq!(accounts.list.len(), 1);
        assert!(accounts.switch("aaaa-1").is_err());
    }
}
