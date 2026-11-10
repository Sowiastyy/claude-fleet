//! A cloud session's claude.ai page inside the window, where its terminal
//! would be.
//!
//! The page is a second webview laid over the main one: the main page keeps
//! an empty box where the preview goes and sends its place here whenever it
//! moves, and asks for it hidden when the box goes away or one of the main
//! page's menus would open under it (a webview always draws above the page
//! it sits on).
//!
//! Each account has its own webview with its own data folder, so each keeps
//! its own claude.ai login, the way each runs its own Claude Code. A cloud
//! session only opens for the account that owns it.
//!
//! claude.ai gets no way into the app: the window's capability is for the
//! app's own page only, so a remote page cannot call a command.

use std::{collections::HashMap, path::PathBuf, sync::Mutex};

use anyhow::{Context, Result, bail};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, Rect, Url, Webview, WebviewUrl,
    webview::{Color, NewWindowResponse, WebviewBuilder},
};

pub struct Previews {
    /// Each account's webview data folder goes in here.
    dir: PathBuf,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// The newest request seen. Commands run side by side, so an older one
    /// can arrive after a newer one: it is dropped.
    seq: u64,
    /// By account.
    pages: HashMap<String, Page>,
    /// One of them is on the screen.
    visible: bool,
}

struct Page {
    webview: Webview,
    /// The session link it was last sent to. Moving the box keeps whatever
    /// the user went to inside claude.ai since; another session, or asking
    /// to go back, loads the link again.
    asked: String,
}

/// Where the preview goes, in the main page's CSS pixels.
#[derive(serde::Deserialize, Clone, Copy)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Previews {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Show `url`, a cloud session's claude.ai link, at `at` as `account`.
    /// With `reload`, go back to the link even if it is the one shown.
    pub fn show(&self, app: &AppHandle, seq: u64, account: &str, url: &str, at: Bounds, reload: bool) -> Result<()> {
        let url = session_link(url)?;
        let mut guard = self.inner.lock().unwrap();
        let inner = &mut *guard;
        if seq < inner.seq {
            return Ok(());
        }
        inner.seq = seq;
        if at.w < 1.0 || at.h < 1.0 {
            hide_all(inner);
            return Ok(());
        }
        let key = key_of(account);
        for (k, p) in &inner.pages {
            if *k != key {
                let _ = p.webview.hide();
            }
        }
        let bounds = Rect {
            position: LogicalPosition::new(at.x, at.y).into(),
            size: LogicalSize::new(at.w, at.h).into(),
        };
        if let Some(page) = inner.pages.get_mut(&key) {
            if reload || page.asked != url.as_str() {
                page.webview.navigate(url.clone())?;
                page.asked = url.to_string();
            }
            page.webview.set_bounds(bounds)?;
            page.webview.show()?;
            inner.visible = true;
            return Ok(());
        }
        let window = app.get_window("main").context("no main window")?;
        let builder = WebviewBuilder::new(format!("cloud-{key}"), WebviewUrl::External(url.clone()))
            .data_directory(self.dir.join(&key))
            .background_color(Color(0x0D, 0x13, 0x1D, 0xFF))
            // The app keeps the keys until the page is clicked.
            .focused(false)
            .on_new_window(|to, _| {
                // A sign-in popup has to stay with the page that opened it;
                // any other link is for the browser.
                if signs_in(&to) {
                    NewWindowResponse::Allow
                } else {
                    let _ = crate::open_in_browser(to.as_str());
                    NewWindowResponse::Deny
                }
            });
        let webview = window
            .add_child(builder, LogicalPosition::new(at.x, at.y), LogicalSize::new(at.w, at.h))
            .context("could not open the preview")?;
        inner.pages.insert(
            key,
            Page {
                webview,
                asked: url.to_string(),
            },
        );
        inner.visible = true;
        Ok(())
    }

    /// Take the preview off the screen; it keeps its page for next time.
    pub fn hide(&self, app: &AppHandle, seq: u64) {
        let mut guard = self.inner.lock().unwrap();
        let inner = &mut *guard;
        if seq < inner.seq {
            return;
        }
        inner.seq = seq;
        let was_shown = hide_all(inner);
        // Keys typed next go to the app, not to the page that had them.
        if was_shown && let Some(main) = app.get_webview("main") {
            let _ = main.set_focus();
        }
    }
}

/// Returns whether one was on the screen.
fn hide_all(inner: &mut Inner) -> bool {
    for p in inner.pages.values() {
        let _ = p.webview.hide();
    }
    std::mem::replace(&mut inner.visible, false)
}

/// The webview's name and data folder for an account: its uuid, or
/// `default` for whatever `~/.claude` is signed in as.
fn key_of(account: &str) -> String {
    let k: String = account
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if k.is_empty() { "default".to_string() } else { k }
}

/// Only a claude.ai/code page is shown here.
fn session_link(url: &str) -> Result<Url> {
    let u = Url::parse(url).context("not a link")?;
    if u.scheme() != "https" || u.host_str() != Some("claude.ai") || !u.path().starts_with("/code/") {
        bail!("not a claude.ai/code link");
    }
    Ok(u)
}

/// A popup claude.ai opens to sign in, which needs to stay in its own
/// browser to hand the login back.
fn signs_in(url: &Url) -> bool {
    let Some(host) = url.host_str() else { return false };
    let under = |d: &str| host == d || host.ends_with(&format!(".{d}"));
    url.scheme() == "https"
        && (under("claude.ai") || under("anthropic.com") || host == "accounts.google.com" || host == "appleid.apple.com")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_claude_code_link_is_shown() {
        assert!(session_link("https://claude.ai/code/session_01Abc").is_ok());
        assert!(session_link("https://claude.ai/code/cse_01Abc?x=1").is_ok());
        assert!(session_link("http://claude.ai/code/session_01Abc").is_err());
        assert!(session_link("https://claude.ai.evil.com/code/x").is_err());
        assert!(session_link("https://claude.ai/settings").is_err());
        assert!(session_link("file:///c:/x").is_err());
    }

    #[test]
    fn an_account_names_its_own_webview() {
        assert_eq!(key_of("0f6c2a1e-9b7d-4c3a-8e21-5d4f3b2a1c0e"), "0f6c2a1e-9b7d-4c3a-8e21-5d4f3b2a1c0e");
        assert_eq!(key_of(""), "default");
        assert_eq!(key_of("../../x"), "x");
    }

    #[test]
    fn sign_in_popups_stay_and_other_links_go_to_the_browser() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(signs_in(&u("https://accounts.google.com/o/oauth2/auth?x")));
        assert!(signs_in(&u("https://claude.ai/login")));
        assert!(signs_in(&u("https://console.anthropic.com/")));
        assert!(!signs_in(&u("https://github.com/o/r/pull/1")));
        assert!(!signs_in(&u("https://notclaude.ai/")));
        assert!(!signs_in(&u("http://claude.ai/")));
    }
}
