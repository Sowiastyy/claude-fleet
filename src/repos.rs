//! The GitHub repositories a remote session can be started for.
//!
//! `claude --cloud` takes its repository from the `origin` of the directory it
//! runs in; it has no flag naming one. So a repository picked here needs a
//! checkout: a local clone when there is one, otherwise a clone fleet keeps in
//! `~/.claude/fleet-repos/<owner>/<name>`.
//!
//! The list is the account's repositories from the GitHub API, with the token
//! git already uses for github.com (`GH_TOKEN`/`GITHUB_TOKEN` first, then
//! `git credential fill`), plus the GitHub clones among the recent projects.
//! Without a token only the local clones are listed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::registry;

#[derive(Clone, Debug)]
pub struct Repo {
    /// `owner/name`.
    pub full_name: String,
    pub private: bool,
    /// A checkout of it on this machine, when one is known.
    pub local: Option<PathBuf>,
}

/// What a listing found, and why it may be short.
pub struct Listing {
    pub repos: Vec<Repo>,
    /// Set when the GitHub API could not be asked; the list is local clones only.
    pub note: Option<String>,
}

#[derive(Deserialize)]
struct ApiRepo {
    full_name: String,
    #[serde(default)]
    private: bool,
}

/// How many pages of 100 the API is asked for at most.
const MAX_PAGES: usize = 5;

pub fn list() -> Listing {
    let local = local_clones();
    let (api, note) = match token() {
        None => (Vec::new(), Some("no GitHub token — only local clones".to_string())),
        Some(token) => match api_repos(&token) {
            Ok(r) => (r, None),
            Err(e) => (Vec::new(), Some(format!("GitHub: {e} — only local clones"))),
        },
    };
    Listing {
        repos: merge(api, local),
        note,
    }
}

/// The API's list in its order (last pushed first), each with its local clone;
/// local clones the API did not return go after it.
fn merge(api: Vec<ApiRepo>, mut local: Vec<(String, PathBuf)>) -> Vec<Repo> {
    let mut out: Vec<Repo> = api
        .into_iter()
        .map(|a| {
            let key = a.full_name.to_lowercase();
            let found = local.iter().position(|(n, _)| n.to_lowercase() == key);
            Repo {
                full_name: a.full_name,
                private: a.private,
                local: found.map(|i| local.remove(i).1),
            }
        })
        .collect();
    out.extend(local.into_iter().map(|(full_name, path)| Repo {
        full_name,
        private: false,
        local: Some(path),
    }));
    out
}

/// The recent projects whose `origin` is on GitHub, one per repository.
fn local_clones() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for dir in registry::recent_cwds(60) {
        let Some(url) = git_out(&dir, &["remote", "get-url", "origin"]) else {
            continue;
        };
        let Some(name) = github_name(url.trim()) else { continue };
        let root = git_out(&dir, &["rev-parse", "--show-toplevel"])
            .map(|r| PathBuf::from(r.trim()))
            .unwrap_or(dir);
        if !out.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name)) {
            out.push((name, root));
        }
    }
    out
}

/// `owner/name` out of a GitHub remote URL, https or ssh.
fn github_name(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = rest.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    (!owner.is_empty() && !name.is_empty() && parts.next().is_none()).then(|| format!("{owner}/{name}"))
}

/// The token for github.com: the environment first, then git's credential
/// helper — never interactively, a login window has no business opening here.
fn token() -> Option<String> {
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(t) = std::env::var(var)
            && !t.trim().is_empty()
        {
            return Some(t.trim().to_string());
        }
    }
    let mut child = no_window(Command::new("git"))
        .args(["credential", "fill"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child
        .stdin
        .take()?
        .write_all(b"protocol=https\nhost=github.com\n\n")
        .ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("password="))
        .map(str::to_string)
        .filter(|t| !t.is_empty())
}

fn api_repos(token: &str) -> Result<Vec<ApiRepo>, String> {
    let mut all = Vec::new();
    for page in 1..=MAX_PAGES {
        let url = format!(
            "https://api.github.com/user/repos?per_page=100&sort=pushed\
             &affiliation=owner,collaborator,organization_member&page={page}"
        );
        // The token goes in on stdin (`-H @-`), not on the command line where
        // any process listing would show it.
        let mut child = no_window(Command::new("curl"))
            .args(["-fsS", "--max-time", "20", "-H", "@-"])
            .args(["-H", "Accept: application/vnd.github+json"])
            .args(["-H", "User-Agent: claude-fleet"])
            .arg(&url)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| "could not run curl".to_string())?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(stdin, "Authorization: Bearer {token}");
        }
        let out = child
            .wait_with_output()
            .map_err(|_| "curl failed".to_string())?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(if err.contains("401") {
                "token refused".to_string()
            } else {
                "no answer".to_string()
            });
        }
        let batch: Vec<ApiRepo> =
            serde_json::from_slice(&out.stdout).map_err(|_| "unexpected answer".to_string())?;
        let last = batch.len() < 100;
        all.extend(batch);
        if last {
            break;
        }
    }
    Ok(all)
}

/// Where fleet keeps its own clone of a repository.
pub fn cache_dir(full_name: &str) -> Option<PathBuf> {
    let mut p = dirs::home_dir()?.join(".claude").join("fleet-repos");
    for part in full_name.split('/') {
        p.push(part);
    }
    Some(p)
}

/// A checkout of `repo` to run `claude --cloud` in: the local clone, fleet's
/// own clone, or a new clone into fleet's place. Blocking; run it on a thread.
pub fn checkout(repo: &Repo) -> Result<PathBuf, String> {
    if let Some(p) = &repo.local
        && p.is_dir()
    {
        return Ok(p.clone());
    }
    let dir = cache_dir(&repo.full_name).ok_or("no home directory")?;
    if dir.join(".git").is_dir() {
        // Brought up to date so the cloud session does not start from an old
        // HEAD; a failed fetch still leaves a usable checkout.
        let _ = git_out(&dir, &["pull", "--ff-only", "--quiet"]);
        return Ok(dir);
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let url = format!("https://github.com/{}.git", repo.full_name);
    // Blobs come on demand: the history is whole, which the branch checks of
    // `--cloud` want, and the download stays small.
    let out = no_window(Command::new("git"))
        .args(["clone", "--quiet", "--filter=blob:none"])
        .arg(&url)
        .arg(&dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|_| "could not run git".to_string())?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&dir);
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("fatal:"))
            .or_else(|| err.lines().map(str::trim).find(|l| !l.is_empty()))
            .unwrap_or("git clone failed");
        return Err(line.to_string());
    }
    Ok(dir)
}

fn git_out(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = no_window(Command::new("git"))
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn no_window(mut cmd: Command) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_urls_give_owner_and_name() {
        assert_eq!(github_name("https://github.com/a/b.git").as_deref(), Some("a/b"));
        assert_eq!(github_name("https://github.com/a/b/").as_deref(), Some("a/b"));
        assert_eq!(github_name("git@github.com:a/b.git").as_deref(), Some("a/b"));
        assert_eq!(github_name("ssh://git@github.com/a/b").as_deref(), Some("a/b"));
        assert_eq!(github_name("https://gitlab.com/a/b"), None);
        assert_eq!(github_name("https://github.com/a"), None);
    }

    #[test]
    fn local_clones_join_their_api_entry_and_the_rest_go_last() {
        let api = vec![
            ApiRepo { full_name: "me/one".into(), private: true },
            ApiRepo { full_name: "me/two".into(), private: false },
        ];
        let local = vec![
            ("Me/Two".to_string(), PathBuf::from("C:/two")),
            ("other/three".to_string(), PathBuf::from("C:/three")),
        ];
        let r = merge(api, local);
        assert_eq!(r.len(), 3);
        assert!(r[0].local.is_none() && r[0].private);
        assert_eq!(r[1].local.as_deref(), Some(Path::new("C:/two")));
        assert_eq!(r[2].full_name, "other/three");
    }
}
