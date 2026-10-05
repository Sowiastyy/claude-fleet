use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

const BUNDLE: &str = "Claude Fleet.app";
const ICON: &[u8] = include_bytes!("../assets/macos/icon.icns");

const RUN: &str = r#"#!/bin/sh
cd "$HOME" || exit 1
exec "${SHELL:-/bin/zsh}" -lc 'exec "$0"' "$(dirname "$0")/claude-fleet"
"#;

pub fn install(args: &[String]) -> Result<()> {
    let terminal = match args.iter().position(|a| a == "--terminal") {
        Some(i) => Some(
            args.get(i + 1)
                .context("--terminal needs a name: Ghostty, iTerm or Terminal")?,
        ),
        None => None,
    };
    if let Some(t) = terminal
        && !matches!(t.as_str(), "Ghostty" | "iTerm" | "Terminal")
    {
        bail!("--terminal takes Ghostty, iTerm or Terminal, not \"{t}\"");
    }

    let app = target_dir()?.join(BUNDLE);
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    fs::create_dir_all(&macos)?;
    fs::create_dir_all(&resources)?;

    let own = env::current_exe()?.canonicalize()?;
    let bin = resources.join("claude-fleet");
    if bin.canonicalize().ok().as_deref() != Some(own.as_path()) {
        let part = resources.join("claude-fleet.part");
        fs::copy(&own, &part).with_context(|| format!("cannot copy {}", own.display()))?;
        fs::rename(&part, &bin)?;
    }
    executable(&bin)?;

    fs::write(contents.join("Info.plist"), info_plist())?;
    fs::write(resources.join("icon.icns"), ICON)?;
    let run = resources.join("run.command");
    fs::write(&run, RUN)?;
    executable(&run)?;
    if terminal.map_or(Path::new("/Applications/iTerm.app").is_dir(), |t| {
        t == "iTerm"
    }) {
        iterm_profile(&run)?;
    }
    let launch = macos.join("launch");
    fs::write(&launch, launcher(terminal.map(String::as_str)))?;
    executable(&launch)?;

    let _ = Command::new(
        "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister",
    )
    .arg("-f")
    .arg(&app)
    .status();
    let _ = Command::new("touch").arg(&app).status();

    println!("installed {}", app.display());
    println!("open it from Launchpad or Spotlight (\"Claude Fleet\"), or drag it to the Dock.");
    println!(
        "it opens in {}.",
        terminal.map_or(
            "Ghostty, iTerm or Terminal — the first one installed",
            String::as_str
        )
    );
    Ok(())
}

fn target_dir() -> Result<PathBuf> {
    let system = PathBuf::from("/Applications");
    let probe = system.join(".claude-fleet-probe");
    if fs::write(&probe, b"").is_ok() {
        let _ = fs::remove_file(&probe);
        return Ok(system);
    }
    let home = dirs::home_dir()
        .context("no home directory")?
        .join("Applications");
    fs::create_dir_all(&home)?;
    Ok(home)
}

fn executable(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("cannot make {} executable", path.display()))
}

const CMD_KEYS: &[(char, bool, &str)] = &[
    ('1', false, "OP"),
    ('2', false, "OQ"),
    ('3', false, "OR"),
    ('4', false, "OS"),
    ('5', false, "[15~"),
    ('6', false, "[17~"),
    ('7', false, "[18~"),
    ('8', false, "[19~"),
    ('9', false, "[20~"),
    ('\u{1b}', false, "[21~"),
    ('n', false, "[23~"),
    ('/', false, "[24~"),
    ('g', false, "g"),
    ('g', true, "G"),
    ('e', false, "e"),
    ('v', true, "V"),
];

const ITERM: &str = r#"on run argv
  set fresh to not (application "iTerm" is running)
  tell application "iTerm"
    activate
    if fresh then delay 0.5
    set w to (create window with profile "Claude Fleet")
    if fresh and (count of windows) is 2 then
      repeat with x in windows
        if id of x is not id of w then close x
      end repeat
    end if
    set zoomed of w to true
  end tell
end run"#;

const TERMINAL: &str = r#"on run argv
  set fresh to not (application "Terminal" is running)
  set cmd to "exec " & quoted form of (item 1 of argv)
  tell application "Terminal"
    activate
    if fresh then
      delay 0.5
      do script cmd in front window
    else
      do script cmd
    end if
    delay 0.3
    set zoomed of front window to true
  end tell
end run"#;

fn launcher(terminal: Option<&str>) -> String {
    let open = |app: &str| match app {
        "Ghostty" => format!(
            r#"exec open -na Ghostty --args --maximize=true --macos-option-as-alt=left --quit-after-last-window-closed=true --title="Claude Fleet" {} -e "$RUN""#,
            ghostty_keybinds()
        ),
        "iTerm" => format!("exec osascript - \"$RUN\" <<'EOF'\n{ITERM}\nEOF"),
        _ => format!("exec osascript - \"$RUN\" <<'EOF'\n{TERMINAL}\nEOF"),
    };
    let body = match terminal {
        Some(t) => open(t),
        None => format!(
            "if [ -d /Applications/Ghostty.app ]; then\n{}\nelif [ -d /Applications/iTerm.app ]; then\n{}\nelse\n{}\nfi",
            open("Ghostty"),
            open("iTerm"),
            open("Terminal")
        ),
    };
    format!(
        "#!/bin/sh\nRUN=\"$(cd \"$(dirname \"$0\")/../Resources\" && pwd)/run.command\"\n{body}\n"
    )
}

fn ghostty_keybinds() -> String {
    CMD_KEYS
        .iter()
        .map(|&(key, shift, seq)| {
            let name = match key {
                '\u{1b}' => "escape".to_string(),
                '/' => "slash".to_string(),
                c => c.to_string(),
            };
            let mods = if shift { "cmd+shift" } else { "cmd" };
            format!("'--keybind={mods}+{name}=text:\\x1b{seq}'")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn iterm_keyboard_map() -> serde_json::Value {
    const CMD: u32 = 0x10_0000;
    const SHIFT: u32 = 0x2_0000;
    CMD_KEYS
        .iter()
        .map(|&(key, shift, seq)| {
            let (key, mods) = if shift {
                (key.to_ascii_uppercase(), CMD | SHIFT)
            } else {
                (key, CMD)
            };
            (
                format!("0x{:x}-0x{mods:x}", key as u32),
                serde_json::json!({ "Action": 10, "Text": seq }),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

fn iterm_profile(run: &Path) -> Result<()> {
    let dir = dirs::home_dir()
        .context("no home directory")?
        .join("Library/Application Support/iTerm2/DynamicProfiles");
    fs::create_dir_all(&dir)?;
    let command = run.display().to_string().replace(' ', "\\ ");
    let profile = serde_json::json!({
        "Profiles": [{
            "Name": "Claude Fleet",
            "Guid": "claude-fleet-app",
            "Custom Command": "Yes",
            "Command": command,
            "Option Key Sends": 2,
            "Right Option Key Sends": 0,
            "Close Sessions On End": true,
            "Custom Window Title": "Claude Fleet",
            "Keyboard Map": iterm_keyboard_map(),
        }]
    });
    fs::write(
        dir.join("claude-fleet.json"),
        serde_json::to_string_pretty(&profile)?,
    )?;
    Ok(())
}

fn info_plist() -> String {
    let v = crate::update::CURRENT;
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Claude Fleet</string>
  <key>CFBundleDisplayName</key><string>Claude Fleet</string>
  <key>CFBundleIdentifier</key><string>com.github.sowiastyy.claude-fleet</string>
  <key>CFBundleExecutable</key><string>launch</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>{v}</string>
  <key>CFBundleShortVersionString</key><string>{v}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
</dict>
</plist>
"#
    )
}
