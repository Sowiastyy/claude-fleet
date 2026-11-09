//! Finding the `claude` executable.

use std::path::PathBuf;

/// Resolve the `claude` executable, falling back to the standard install path
/// when it is not on PATH (common when launched from a GUI shell).
pub fn claude_binary() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        let local = home.join(".local").join("bin").join(if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        });
        if local.exists() {
            return local;
        }
    }
    PathBuf::from("claude")
}
