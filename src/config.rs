//! Runtime configuration, re-read from disk while the fleet is running.
//!
//! Colours, the words on a card and the text the `u` chord types are the parts
//! one tweaks by eye, so they are the parts worth seeing change without a
//! restart. They live in one TOML file whose mtime is checked every frame; an
//! edit shows up in the next redraw, with every session left alone.
//!
//! The current config is a global rather than something threaded through every
//! draw call. Rendering reaches for a colour in a few hundred places and none
//! of those places has an opinion about where it came from; passing a handle
//! into all of them would be plumbing with no reader.

use std::{
    fs,
    path::PathBuf,
    sync::{LazyLock, RwLock},
    time::{Duration, SystemTime},
};

use ratatui::style::Color;
use serde::Deserialize;

/// Environment override for the config path, so a second fleet can be run
/// against a different file without touching the first one's.
pub const PATH_VAR: &str = "CLAUDE_FLEET_CONFIG";

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub theme: ThemeSpec,
    pub labels: Labels,
    pub understand: Understand,
    pub timings: Timings,
    pub updates: Updates,
    pub commit: CommitCfg,
    pub bigbrother: BigBrotherCfg,
}

/// Colours as written in the file: `#RRGGBB`, or a named terminal colour.
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeSpec {
    pub accent: String,
    pub accent_dim: String,
    pub text: String,
    pub muted: String,
    pub faint: String,
    pub busy: String,
    pub idle: String,
    pub ask: String,
    pub dead: String,
    pub surface: String,
    /// The filled part of a limit bar. Left out, it is the accent: one colour
    /// moves the whole chrome. Set, it is the bar's own.
    pub bar: Option<String>,
    /// The unfilled part of a limit bar, `accent_dim` unless it is set.
    pub bar_empty: Option<String>,
}

impl Default for ThemeSpec {
    fn default() -> Self {
        Self {
            accent: "#D97757".into(),
            accent_dim: "#8A4C36".into(),
            text: "#E6E1DA".into(),
            muted: "#7A746E".into(),
            faint: "#4A4642".into(),
            busy: "#E0B04C".into(),
            idle: "#6EA87A".into(),
            ask: "#5C9FD8".into(),
            dead: "#B05A5A".into(),
            surface: "#1A1817".into(),
            bar: None,
            bar_empty: None,
        }
    }
}

/// The same palette, parsed. `Copy`, so a caller takes a snapshot of the whole
/// thing and never holds the lock while it draws.
#[derive(Clone, Copy)]
pub struct Theme {
    pub accent: Color,
    pub accent_dim: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub busy: Color,
    pub idle: Color,
    pub ask: Color,
    pub dead: Color,
    pub surface: Color,
    pub bar: Color,
    pub bar_empty: Color,
}

impl ThemeSpec {
    /// Parse the palette, falling back per colour. One unreadable value costs
    /// that colour, not the whole file.
    fn resolve(&self) -> Theme {
        let d = ThemeSpec::default();
        let pick = |v: &str, fallback: &str| {
            parse_colour(v)
                .or_else(|| parse_colour(fallback))
                .unwrap_or(Color::Reset)
        };
        let accent = pick(&self.accent, &d.accent);
        let accent_dim = pick(&self.accent_dim, &d.accent_dim);
        Theme {
            accent,
            accent_dim,
            text: pick(&self.text, &d.text),
            muted: pick(&self.muted, &d.muted),
            faint: pick(&self.faint, &d.faint),
            busy: pick(&self.busy, &d.busy),
            idle: pick(&self.idle, &d.idle),
            ask: pick(&self.ask, &d.ask),
            dead: pick(&self.dead, &d.dead),
            surface: pick(&self.surface, &d.surface),
            // An unset bar colour follows the accent rather than a constant of
            // its own: changing `accent` alone has to move the bar with it, or
            // the palette comes apart the first time anyone edits it.
            bar: self.bar.as_deref().and_then(parse_colour).unwrap_or(accent),
            bar_empty: self
                .bar_empty
                .as_deref()
                .and_then(parse_colour)
                .unwrap_or(accent_dim),
        }
    }
}

/// The words on a session card, one per registry status.
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Labels {
    pub busy: String,
    pub idle: String,
    /// `status: waiting` — stopped on a dialog or a permission prompt.
    pub waiting: String,
    /// Spawned, but no registry entry written yet.
    pub starting: String,
}

impl Default for Labels {
    fn default() -> Self {
        Self {
            busy: "working".into(),
            idle: "idle".into(),
            waiting: "question".into(),
            starting: "starting".into(),
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Understand {
    /// What the chord types into the prompt box.
    pub prompt: String,
    /// How long after landing in a session a lone `u` is still the chord.
    pub window_ms: u64,
}

impl Default for Understand {
    fn default() -> Self {
        Self {
            prompt: "understand project".into(),
            window_ms: 2000,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Timings {
    /// How long a finished session's card stays on the list.
    pub finished_ttl_secs: u64,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            finished_ttl_secs: 60,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Updates {
    /// Whether GitHub is asked about a newer release.
    pub check: bool,
}

impl Default for Updates {
    fn default() -> Self {
        Self { check: true }
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommitCfg {
    /// The model that writes a commit message when the git panel asks for one.
    pub model: String,
}

impl Default for CommitCfg {
    fn default() -> Self {
        Self {
            model: "claude-haiku-4-5".into(),
        }
    }
}

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BigBrotherCfg {
    /// The model a Big Brother runs on; empty leaves it to Claude Code.
    pub model: String,
    /// Standing orders appended to its system prompt: what counts as
    /// suspicious here, which language to report in, what it may do alone.
    pub instructions: String,
}

/// Everything the loader tracks: the values, the palette they resolved to, and
/// what the file looked like when they were read.
struct Loaded {
    cfg: Config,
    theme: Theme,
    path: PathBuf,
    /// `None` when the file is missing, which is a state like any other: it
    /// means defaults, and it changes back the moment a file appears.
    stamp: Option<SystemTime>,
}

static CURRENT: LazyLock<RwLock<Loaded>> = LazyLock::new(|| {
    let path = config_path();
    let stamp = stamp_of(&path);
    let cfg = read(&path).unwrap_or_default();
    let theme = cfg.theme.resolve();
    RwLock::new(Loaded {
        cfg,
        theme,
        path,
        stamp,
    })
});

pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os(PATH_VAR) {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .map(|h| h.join(".claude").join("fleet.toml"))
        .unwrap_or_else(|| PathBuf::from("fleet.toml"))
}

fn stamp_of(path: &std::path::Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

fn read(path: &std::path::Path) -> Option<Config> {
    let raw = fs::read_to_string(path).ok()?;
    toml::from_str(&raw).ok()
}

/// Load the file now, so a broken config is reported before the UI covers the
/// screen rather than as a status line nobody was looking at.
pub fn init() -> Result<(), String> {
    let path = config_path();
    if !path.exists() {
        return Ok(());
    }
    let raw = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let cfg: Config = toml::from_str(&raw).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut cur = CURRENT
        .write()
        .map_err(|_| "config lock poisoned".to_string())?;
    cur.theme = cfg.theme.resolve();
    cur.cfg = cfg;
    cur.stamp = stamp_of(&path);
    Ok(())
}

/// Write a config with today's values in it, so there is something to edit.
///
/// Only ever writes when nothing is there: the file belongs to whoever edits
/// it, and overwriting it would be the fleet arguing with them.
pub fn write_default_if_missing() -> Option<PathBuf> {
    let path = config_path();
    if path.exists() {
        return None;
    }
    fs::create_dir_all(path.parent()?).ok()?;
    fs::write(&path, DEFAULT_FILE).ok()?;
    Some(path)
}

/// What a reload did, if it did anything.
pub enum Reload {
    /// The file changed and the new values are live.
    Applied,
    /// The file changed and could not be read; the old values still stand.
    Failed(String),
}

/// Re-read the file if it has moved since it was last read.
///
/// A failed parse keeps the previous values: a config is edited in place, and
/// half a line of TOML must not blank out the palette mid-keystroke. The stamp
/// is taken anyway, so the same broken file is not reported every frame.
pub fn reload_if_changed() -> Option<Reload> {
    let path = config_path();
    let stamp = stamp_of(&path);
    {
        let cur = CURRENT.read().ok()?;
        if cur.path == path && cur.stamp == stamp {
            return None;
        }
    }

    let parsed = if stamp.is_none() {
        // The file went away, which means defaults again.
        Ok(Config::default())
    } else {
        fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|raw| toml::from_str::<Config>(&raw).map_err(|e| e.to_string()))
    };

    let mut cur = CURRENT.write().ok()?;
    cur.path = path;
    cur.stamp = stamp;
    match parsed {
        Ok(cfg) => {
            cur.theme = cfg.theme.resolve();
            cur.cfg = cfg;
            Some(Reload::Applied)
        }
        Err(e) => Some(Reload::Failed(first_line(&e))),
    }
}

/// TOML errors carry the offending snippet over several lines; a status bar
/// has one.
fn first_line(e: &str) -> String {
    e.lines().next().unwrap_or(e).trim().to_string()
}

pub fn theme() -> Theme {
    CURRENT
        .read()
        .map(|c| c.theme)
        .unwrap_or_else(|_| ThemeSpec::default().resolve())
}

pub fn labels() -> Labels {
    CURRENT
        .read()
        .map(|c| c.cfg.labels.clone())
        .unwrap_or_default()
}

pub fn understand_prompt() -> String {
    CURRENT
        .read()
        .map(|c| c.cfg.understand.prompt.clone())
        .unwrap_or_else(|_| Understand::default().prompt)
}

pub fn understand_window() -> Duration {
    let ms = CURRENT
        .read()
        .map(|c| c.cfg.understand.window_ms)
        .unwrap_or_else(|_| Understand::default().window_ms);
    Duration::from_millis(ms)
}

pub fn check_updates() -> bool {
    CURRENT
        .read()
        .map(|c| c.cfg.updates.check)
        .unwrap_or_else(|_| Updates::default().check)
}

pub fn commit_model() -> String {
    CURRENT
        .read()
        .map(|c| c.cfg.commit.model.clone())
        .unwrap_or_else(|_| CommitCfg::default().model)
}

pub fn bigbrother() -> BigBrotherCfg {
    CURRENT
        .read()
        .map(|c| c.cfg.bigbrother.clone())
        .unwrap_or_default()
}

pub fn finished_ttl() -> Duration {
    let secs = CURRENT
        .read()
        .map(|c| c.cfg.timings.finished_ttl_secs)
        .unwrap_or_else(|_| Timings::default().finished_ttl_secs);
    Duration::from_secs(secs)
}

/// `#RRGGBB`, `#RGB`, or one of the sixteen names a terminal already knows.
fn parse_colour(s: &str) -> Option<Color> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return match hex.len() {
            6 => {
                let n = u32::from_str_radix(hex, 16).ok()?;
                Some(Color::Rgb(
                    (n >> 16) as u8,
                    ((n >> 8) & 0xff) as u8,
                    (n & 0xff) as u8,
                ))
            }
            // `#abc` is `#aabbcc`, as everywhere else that accepts it.
            3 => {
                let mut c = hex.chars();
                let mut nib = || {
                    let d = c.next()?.to_digit(16)? as u8;
                    Some(d * 16 + d)
                };
                Some(Color::Rgb(nib()?, nib()?, nib()?))
            }
            _ => None,
        };
    }
    match s.to_ascii_lowercase().as_str() {
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "gray" | "grey" => Some(Color::Gray),
        "darkgray" | "darkgrey" => Some(Color::DarkGray),
        "lightred" => Some(Color::LightRed),
        "lightgreen" => Some(Color::LightGreen),
        "lightyellow" => Some(Color::LightYellow),
        "lightblue" => Some(Color::LightBlue),
        "lightmagenta" => Some(Color::LightMagenta),
        "lightcyan" => Some(Color::LightCyan),
        "white" => Some(Color::White),
        "reset" => Some(Color::Reset),
        _ => None,
    }
}

const DEFAULT_FILE: &str = r##"# claude-fleet — read live.
# A save shows up in the panel at once: no restart, no sessions lost.

[theme]
accent     = "#D97757"
accent_dim = "#8A4C36"
text       = "#E6E1DA"
muted      = "#7A746E"
faint      = "#4A4642"
busy       = "#E0B04C"   # status: busy
idle       = "#6EA87A"   # status: idle
ask        = "#5C9FD8"   # status: waiting — the session is stopped on a question
dead       = "#B05A5A"
surface    = "#1A1817"
# The limit bar follows the accent. Uncomment to give it colours of its own.
# bar       = "#D97757"
# bar_empty = "#8A4C36"

[labels]
busy     = "working"
idle     = "idle"
waiting  = "question"
starting = "starting"

[understand]
# The text the `u` chord types into the prompt. Enter stays on your side.
prompt    = "understand project"
# For how many ms after landing in a session a lone `u` is still the chord
# rather than a letter.
window_ms = 2000

[timings]
# After how many seconds a finished session's card disappears.
finished_ttl_secs = 60

[updates]
# Ask GitHub every five minutes whether a newer release is out. Nothing is
# downloaded until `i` is pressed.
check = true

[commit]
# The model that writes a commit message when the git panel is asked for one
# (`m`, or the Generate button). It runs through `claude -p`, on your account.
model = "claude-haiku-4-5"

[bigbrother]
# The model a Big Brother (`B`) runs on. Empty = Claude Code's default.
model = ""
# Standing orders added to its system prompt, e.g. the language to report in
# or what it may do without asking.
instructions = ""
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colours_parse_in_both_lengths() {
        assert_eq!(parse_colour("#D97757"), Some(Color::Rgb(0xD9, 0x77, 0x57)));
        assert_eq!(parse_colour("#abc"), Some(Color::Rgb(0xaa, 0xbb, 0xcc)));
    }

    #[test]
    fn named_colours_parse_case_insensitively() {
        assert_eq!(parse_colour("LightBlue"), Some(Color::LightBlue));
    }

    #[test]
    fn nonsense_is_rejected_rather_than_guessed() {
        assert_eq!(parse_colour("#12345"), None);
        assert_eq!(parse_colour("burgundy"), None);
    }

    #[test]
    fn the_bar_follows_the_accent_until_it_is_told_otherwise() {
        // Changing the accent alone has to move the bar with it.
        let spec = ThemeSpec {
            accent: "#123456".into(),
            accent_dim: "#0a0a0a".into(),
            ..ThemeSpec::default()
        };
        let theme = spec.resolve();
        assert_eq!(theme.bar, Color::Rgb(0x12, 0x34, 0x56));
        assert_eq!(theme.bar_empty, Color::Rgb(0x0a, 0x0a, 0x0a));

        // And an explicit bar colour wins over the accent.
        let spec = ThemeSpec {
            bar: Some("#6EA87A".into()),
            ..spec
        };
        let theme = spec.resolve();
        assert_eq!(theme.bar, Color::Rgb(0x6E, 0xA8, 0x7A));
        assert_eq!(theme.accent, Color::Rgb(0x12, 0x34, 0x56));
    }

    #[test]
    fn one_bad_colour_costs_only_itself() {
        let spec = ThemeSpec {
            accent: "nonsense".into(),
            ..ThemeSpec::default()
        };
        let theme = spec.resolve();
        // Falls back to the default accent, and the rest is untouched.
        assert_eq!(theme.accent, Color::Rgb(0xD9, 0x77, 0x57));
        assert_eq!(theme.ask, Color::Rgb(0x5C, 0x9F, 0xD8));
    }

    #[test]
    fn the_shipped_default_file_parses_into_the_defaults() {
        let cfg: Config =
            toml::from_str(DEFAULT_FILE).expect("the shipped default file has to parse");
        assert_eq!(cfg.labels.waiting, "question");
        assert_eq!(cfg.understand.prompt, "understand project");
        assert_eq!(cfg.timings.finished_ttl_secs, 60);
        assert!(cfg.updates.check);
        assert_eq!(cfg.commit.model, "claude-haiku-4-5");
        assert_eq!(cfg.theme.resolve().ask, Color::Rgb(0x5C, 0x9F, 0xD8));
    }

    #[test]
    fn an_unknown_key_is_an_error_rather_than_silence() {
        // A typo that parsed would look like the setting simply had no effect.
        let err = toml::from_str::<Config>("[labels]\nwaitng = \"x\"\n");
        assert!(err.is_err());
    }
}
