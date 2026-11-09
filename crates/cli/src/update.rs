//! Finding out about a newer release of the terminal app and putting it in
//! place. The work is `fleet_core::update`'s, shared with the GUI; this side
//! names the asset and builds from source where a release carries none.
//!
//! Nothing here restarts anything. Installing only swaps the file the
//! supervisor copies from; the restart that picks it up is the ordinary one,
//! with the ordinary question about the running sessions.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
pub use fleet_core::update::{Release, dev_repo, sweep};
use fleet_core::update::{self as core, sibling, source_repo};

/// The asset a release carries, as `release.yml` uploads it.
const ASSET: &str = if cfg!(target_os = "macos") {
    "claude-fleet-macos-arm64"
} else {
    "claude-fleet.exe"
};

/// The version this binary was built as.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Ask GitHub for the latest release, and say whether it is newer than us.
pub fn check(repo: Option<&Path>) -> Result<Option<Release>> {
    core::check(ASSET, CURRENT, repo)
}

/// Download the release and put it where `origin` is, or build it from the
/// checkout when the release has no binary for this platform.
pub fn install(rel: &Release, origin: &Path) -> Result<()> {
    if rel.url.is_empty() {
        return build_from_source(origin);
    }
    core::install(rel, origin, CURRENT)
}

fn build_from_source(origin: &Path) -> Result<()> {
    let repo = source_repo().context("the checkout this fleet was built from is gone")?;
    run(Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["pull", "--rebase", "--autostash"]))
    .context("git pull failed")?;
    let cargo = dirs::home_dir()
        .map(|h| h.join(".cargo").join("bin").join("cargo"))
        .filter(|c| c.is_file())
        .unwrap_or_else(|| PathBuf::from("cargo"));
    run(Command::new(cargo)
        .args(["build", "--release", "--manifest-path"])
        .arg(repo.join("Cargo.toml")))
    .context("cargo build failed")?;
    let built = repo.join("target").join("release").join("claude-fleet");
    if built == origin {
        return Ok(());
    }
    let part = sibling(origin, "new")?;
    fs::copy(&built, &part).with_context(|| format!("could not copy {}", built.display()))?;
    fs::rename(&part, origin)
        .with_context(|| format!("could not put the new build at {}", origin.display()))
}

fn run(cmd: &mut Command) -> Result<()> {
    let out = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let last = err
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    bail!("{last}")
}
