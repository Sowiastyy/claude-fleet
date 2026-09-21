//! Pasting an image that lives only on the clipboard.
//!
//! Windows Terminal answers Ctrl+V by pasting the clipboard's text, and an
//! image has none, so a screenshot never reaches the session at all. Dragging
//! a file in works because the terminal pastes its path, and Claude Code turns
//! an image path into an attachment. So the fleet does the same by hand: the
//! image is written to a PNG and its path is pasted, as if it had been dropped.

use std::{path::PathBuf, process::Command, time::SystemTime};

/// Save the clipboard's image, or the files copied in Explorer, and return
/// what to paste for them: one path per item, space separated. `None` when the
/// clipboard holds neither.
pub fn paste_text() -> Option<String> {
    let dir = std::env::temp_dir().join("claude-fleet");
    std::fs::create_dir_all(&dir).ok()?;
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_millis();
    let png: PathBuf = dir.join(format!("clip-{stamp}.png"));

    // One PowerShell run both asks and saves; the clipboard API needs STA.
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms;\
         $c=[Windows.Forms.Clipboard];\
         if($c::ContainsImage()){{$c::GetImage().Save('{}',[Drawing.Imaging.ImageFormat]::Png);'{}'}}\
         elseif($c::ContainsFileDropList()){{$c::GetFileDropList()}}",
        png.display(),
        png.display()
    );
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-STA", "-Command", &script]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().ok()?;
    let paths: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(quote)
        .collect();
    (!paths.is_empty()).then(|| paths.join(" "))
}

/// Quote a path the way a drop does: only when a space would split it.
fn quote(path: &str) -> String {
    if path.contains(' ') {
        format!("\"{path}\"")
    } else {
        path.to_string()
    }
}
