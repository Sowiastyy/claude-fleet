//! Finding out about a newer release on GitHub and putting it in place.
//!
//! Nothing here restarts anything. Installing only swaps the file the
//! supervisor copies from; the restart that picks it up is the ordinary one,
//! with the ordinary question about the running sessions.
//!
//! HTTP goes through `curl.exe`, which Windows has shipped since 10 1803 — one
//! release older than the ConPTY the fleet needs anyway. A TLS stack compiled
//! in would be the biggest dependency in the tree, for two requests a day.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

const LATEST: &str = "https://api.github.com/repos/Sowiastyy/claude-fleet/releases/latest";
/// The asset a release carries, as `release.yml` uploads it.
const ASSET: &str = "claude-fleet.exe";

/// The version this binary was built as.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// A release newer than this binary.
#[derive(Clone, Debug)]
pub struct Release {
    /// The tag, `v` and all, as GitHub shows it.
    pub tag: String,
    pub url: String,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    /// The commit `release.yml` built from, passed as `--target`.
    #[serde(default)]
    target_commitish: String,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

/// Ask GitHub for the latest release, and say whether it is newer than us.
///
/// `repo` is the checkout a cargo build came from. Such a build calls itself
/// whatever Cargo.toml says, which is not what CI stamps into a release, so
/// there a release is also not offered once its commit is already in HEAD —
/// that build has it. A release installed over the build knows its number.
pub fn check(repo: Option<&Path>) -> Result<Option<Release>> {
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "20", "-H", "Accept: application/vnd.github+json"])
        .args(["-H", &format!("User-Agent: claude-fleet/{CURRENT}")])
        .arg(LATEST)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("could not run curl")?;
    if !out.status.success() {
        bail!("GitHub did not answer");
    }
    let rel: ApiRelease = serde_json::from_slice(&out.stdout).context("unexpected answer from GitHub")?;
    let commit = rel.target_commitish.trim().to_string();
    let found = newer(rel, CURRENT);
    Ok(match repo {
        Some(repo) => found.filter(|_| !in_head(repo, &commit)),
        None => found,
    })
}

/// Whether `commit` is already part of the checkout at `repo`. A commit git
/// does not know at all has not been pulled, so it is not.
fn in_head(repo: &Path, commit: &str) -> bool {
    !commit.is_empty()
        && Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["merge-base", "--is-ancestor", commit, "HEAD"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

/// The checkout a cargo build output belongs to: `<repo>\target\<profile>\x.exe`.
pub fn dev_repo(origin: &Path) -> Option<PathBuf> {
    is_dev_build(origin).then(|| origin.ancestors().nth(3)).flatten().map(Path::to_path_buf)
}

fn newer(rel: ApiRelease, current: &str) -> Option<Release> {
    let theirs = parse_version(&rel.tag_name)?;
    let ours = parse_version(current)?;
    if theirs <= ours {
        return None;
    }
    let asset = rel.assets.into_iter().find(|a| a.name == ASSET)?;
    Some(Release {
        tag: rel.tag_name,
        url: asset.browser_download_url,
    })
}

/// `v1.2.3` or `1.2.3`. Anything with a suffix is a pre-release, which the
/// latest-release endpoint never returns anyway, and is not offered.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.trim().trim_start_matches('v').split('.');
    let v = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(v)
}

/// Whether the file the supervisor copies from is a cargo build output. A
/// release can still be installed over one; the next `cargo build` simply
/// writes over it again.
pub fn is_dev_build(origin: &Path) -> bool {
    let mut up = origin.ancestors().skip(1);
    let profile = up.next().and_then(Path::file_name);
    let target = up.next().and_then(Path::file_name);
    matches!(profile.and_then(|p| p.to_str()), Some("release" | "debug"))
        && target.is_some_and(|t| t == "target")
}

/// Download the release and put it where `origin` is.
///
/// `origin` is usually the very file the supervisor is running from, and a
/// running executable cannot be written to on Windows. It can be renamed,
/// though, so the old one steps aside as `*.old-<ms>.exe` and the new one takes its
/// name. The old file is swept on the next start, once nothing runs it.
pub fn install(rel: &Release, origin: &Path) -> Result<()> {
    let part = sibling(origin, "new")?;
    // Unique, because the file an earlier update moved aside may still be
    // running — the supervisor keeps going on it until the fleet is quit.
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let old = sibling(origin, &format!("old-{stamp}"))?;
    let _ = fs::remove_file(&part);

    let status = Command::new("curl")
        .args(["-fsSL", "--max-time", "300", "-o"])
        .arg(&part)
        .args(["-H", &format!("User-Agent: claude-fleet/{CURRENT}")])
        .arg(&rel.url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("could not run curl")?;
    if !status.success() {
        let _ = fs::remove_file(&part);
        bail!("download of {} failed", rel.tag);
    }
    // A redirect to an error page still ends in a file; an executable starts
    // with `MZ`, and anything else must not end up where the fleet starts from.
    let head = fs::read(&part).ok().filter(|b| b.len() > 1024 && b.starts_with(b"MZ"));
    if head.is_none() {
        let _ = fs::remove_file(&part);
        bail!("the download of {} is not an executable", rel.tag);
    }

    fs::rename(origin, &old).with_context(|| format!("could not move {} aside", origin.display()))?;
    if let Err(e) = fs::rename(&part, origin) {
        // Put the old one back, so a failed update leaves a working fleet.
        let _ = fs::rename(&old, origin);
        return Err(e).with_context(|| format!("could not put the new build at {}", origin.display()));
    }
    Ok(())
}

/// Remove what earlier updates left beside the binary. An old file refuses to
/// go while a fleet still runs it, which is exactly when it should stay.
pub fn sweep(origin: &Path) {
    let (Some(dir), Some(stem)) = (origin.parent(), origin.file_stem()) else {
        return;
    };
    let old = format!("{}.old-", stem.to_string_lossy());
    let part = format!("{}.new.exe", stem.to_string_lossy());
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name == part || (name.starts_with(&old) && name.ends_with(".exe")) {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// `claude-fleet.exe` → `claude-fleet.<tag>.exe`, in the same directory, so a
/// rename never crosses a volume.
fn sibling(origin: &Path, tag: &str) -> Result<PathBuf> {
    let stem = origin
        .file_stem()
        .context("the fleet binary has no file name")?
        .to_string_lossy();
    Ok(origin.with_file_name(format!("{stem}.{tag}.exe")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str, asset: &str) -> ApiRelease {
        ApiRelease {
            tag_name: tag.into(),
            target_commitish: String::new(),
            assets: vec![ApiAsset {
                name: asset.into(),
                browser_download_url: format!("https://example.invalid/{tag}/{asset}"),
            }],
        }
    }

    #[test]
    fn only_a_higher_version_is_offered() {
        assert!(newer(rel("v0.2.0", ASSET), "0.1.0").is_some());
        assert!(newer(rel("v0.10.0", ASSET), "0.9.3").is_some());
        assert!(newer(rel("v0.1.0", ASSET), "0.1.0").is_none());
        assert!(newer(rel("v0.0.9", ASSET), "0.1.0").is_none());
    }

    #[test]
    fn a_release_without_the_binary_or_with_an_odd_tag_is_not_offered() {
        assert!(newer(rel("v9.0.0", "source.zip"), "0.1.0").is_none());
        assert!(newer(rel("v9.0.0-rc1", ASSET), "0.1.0").is_none());
        assert!(newer(rel("nightly", ASSET), "0.1.0").is_none());
    }

    #[test]
    fn a_cargo_build_output_is_left_to_cargo() {
        assert!(is_dev_build(Path::new(r"C:\src\claude-fleet\target\release\claude-fleet.exe")));
        assert!(is_dev_build(Path::new(r"C:\src\claude-fleet\target\debug\claude-fleet.exe")));
        assert!(!is_dev_build(Path::new(r"C:\tools\claude-fleet.exe")));
        assert!(!is_dev_build(Path::new(r"C:\Users\me\.cargo\bin\claude-fleet.exe")));
        assert_eq!(
            dev_repo(Path::new(r"C:\src\claude-fleet\target\debug\claude-fleet.exe")).as_deref(),
            Some(Path::new(r"C:\src\claude-fleet"))
        );
        assert_eq!(dev_repo(Path::new(r"C:\tools\claude-fleet.exe")), None);
    }

    #[test]
    fn an_install_swaps_the_binary_and_keeps_the_old_one_aside() {
        let dir = std::env::temp_dir().join(format!("fleet-update-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let origin = dir.join("claude-fleet.exe");
        fs::write(&origin, b"old").unwrap();
        let mut body = b"MZ".to_vec();
        body.resize(4096, 0);
        let served = dir.join("served.exe");
        fs::write(&served, &body).unwrap();

        let url = format!("file:///{}", served.display().to_string().replace('\\', "/"));
        install(&Release { tag: "v9.9.9".into(), url }, &origin).unwrap();
        assert_eq!(fs::read(&origin).unwrap(), body);
        let aside = |d: &Path| {
            fs::read_dir(d)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("claude-fleet.old-"))
                .count()
        };
        assert_eq!(aside(&dir), 1);
        sweep(&origin);
        assert_eq!(aside(&dir), 0);

        // Something that is not an executable never replaces the binary.
        fs::write(&served, b"<html>not found</html>").unwrap();
        let url = format!("file:///{}", served.display().to_string().replace('\\', "/"));
        assert!(install(&Release { tag: "v9.9.9".into(), url }, &origin).is_err());
        assert_eq!(fs::read(&origin).unwrap(), body);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_leftovers_sit_next_to_the_binary() {
        let p = Path::new(r"C:\tools\claude-fleet.exe");
        assert_eq!(sibling(p, "old").unwrap(), Path::new(r"C:\tools\claude-fleet.old.exe"));
    }
}
