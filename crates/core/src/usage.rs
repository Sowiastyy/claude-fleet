//! Reads the usage limits Claude Code caches in `~/.claude.json`.
//!
//! A session refreshes `cachedUsageUtilization` for the account it is signed in
//! as, which holds the same numbers `/usage` prints: how much of the five-hour
//! and seven-day windows is spent, and when each one resets. Fleet is a reader
//! here exactly as it is for the session registry — nothing is fetched over the
//! network and nothing is written back.
//!
//! The file is a couple of hundred kilobytes and almost never changes, so it is
//! re-read only when its mtime moves.
//!
//! Reading alone leaves the numbers as old as the last session that bothered to
//! refresh them, which on a quiet machine is hours. So when they go stale, fleet
//! asks Claude Code for new ones the only way that is documented: it drives a
//! hidden session the way a person would — spawn `claude`, type `/usage`, wait
//! for the file to move (`refresh_via_claude` in the terminal app).

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

/// One rate-limit window.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    /// Percent of the window already spent.
    pub pct: u8,
    /// Time left until it resets, or `None` when the cache carries no reset
    /// time — some windows genuinely have none.
    pub resets_in: Option<Duration>,
    /// The window reset while the cache sat unrefreshed, so its numbers
    /// describe a window that is already over.
    pub expired: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    pub session: Option<Window>,
    pub weekly: Option<Window>,
    /// How long ago Claude Code last refreshed these numbers.
    pub fetched_ago: Duration,
}

/// What the file says, before it is turned into countdowns.
#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    session: Option<(u8, Option<i64>)>,
    weekly: Option<(u8, Option<i64>)>,
    fetched_at_ms: Option<u64>,
}

/// Re-reads `~/.claude.json`, but only when it has actually changed.
///
/// The countdowns still have to move every second, so parsing and deriving are
/// separate: the file is parsed on an mtime change, the windows are derived
/// from that snapshot on every refresh.
pub struct Watch {
    path: Option<PathBuf>,
    seen: Option<SystemTime>,
    snapshot: Option<Snapshot>,
    pub current: Option<Usage>,
}

/// The file Claude Code caches the limits in.
pub fn cache_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude.json"))
}

impl Default for Watch {
    fn default() -> Self {
        Self::new()
    }
}

impl Watch {
    pub fn new() -> Self {
        let mut w = Self {
            path: cache_path(),
            seen: None,
            snapshot: None,
            current: None,
        };
        w.refresh();
        w
    }

    /// Returns true when the displayed numbers changed, so the caller knows
    /// whether a redraw is owed.
    pub fn refresh(&mut self) -> bool {
        let Some(path) = self.path.clone() else {
            return false;
        };
        let mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
        if mtime != self.seen {
            self.seen = mtime;
            self.snapshot = read_snapshot(&path);
        }
        let next = self.snapshot.as_ref().map(derive);
        let changed = next != self.current;
        self.current = next;
        changed
    }
}

/// The stamp the fetch itself writes, which is the only part of the file that
/// says the numbers are new.
pub fn fetched_at(path: &Path) -> Option<u64> {
    read_snapshot(&path.to_path_buf())?.fetched_at_ms
}

fn read_snapshot(path: &PathBuf) -> Option<Snapshot> {
    let raw = fs::read_to_string(path).ok()?;
    let root: Value = serde_json::from_str(&raw).ok()?;
    let cached = root.get("cachedUsageUtilization")?;
    // Right after an account switch the cache still holds the numbers of the
    // account switched away from. Showing them under the new one would be
    // wrong in the worst way — plausible — so they are not shown at all until
    // the refresh that a switch asks for writes the new account's.
    let owner = root
        .get("oauthAccount")
        .and_then(|a| a.get("accountUuid"))
        .and_then(Value::as_str);
    let cached_for = cached.get("accountUuid").and_then(Value::as_str);
    if let (Some(owner), Some(cached_for)) = (owner, cached_for)
        && owner != cached_for
    {
        return None;
    }
    snapshot_of(cached)
}

fn snapshot_of(cached: &Value) -> Option<Snapshot> {
    let util = cached.get("utilization")?;
    Some(Snapshot {
        session: window(util.get("five_hour")),
        weekly: window(util.get("seven_day")),
        fetched_at_ms: cached.get("fetchedAtMs").and_then(Value::as_u64),
    })
}

/// The windows in a `cachedUsageUtilization` object kept somewhere else —
/// the copy an account profile holds of the last numbers seen for it.
pub fn from_cache(cached: &Value) -> Option<Usage> {
    snapshot_of(cached).as_ref().map(derive)
}

fn derive(snap: &Snapshot) -> Usage {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    Usage {
        session: snap.session.map(|w| countdown(w, now_ms / 1000)),
        weekly: snap.weekly.map(|w| countdown(w, now_ms / 1000)),
        fetched_ago: snap
            .fetched_at_ms
            .map(|ms| Duration::from_millis(now_ms.saturating_sub(ms)))
            .unwrap_or_default(),
    }
}

fn countdown((pct, resets_at): (u8, Option<i64>), now: u64) -> Window {
    let now = now as i64;
    let (resets_in, expired) = match resets_at {
        Some(at) if at > now => (Some(Duration::from_secs((at - now) as u64)), false),
        Some(_) => (None, true),
        None => (None, false),
    };
    Window {
        pct,
        resets_in,
        expired,
    }
}

fn window(v: Option<&Value>) -> Option<(u8, Option<i64>)> {
    let v = v?;
    if v.is_null() {
        return None;
    }
    let pct = v.get("utilization").and_then(Value::as_f64)?.round();
    let resets_at = v
        .get("resets_at")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339);
    Some((pct.clamp(0.0, 255.0) as u8, resets_at))
}

/// Seconds since the epoch for an RFC 3339 timestamp, e.g.
/// `2026-09-17T16:10:00.888709+00:00`.
///
/// Only what this one field can hold: a fixed numeric offset or `Z`. Pulling in
/// a date crate to read one field of one cache would be the larger cost.
fn parse_rfc3339(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() < 19 || bytes[10] != b'T' {
        return None;
    }
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);

    // The offset is whatever follows the seconds and any fraction.
    let rest = &s[19..];
    let tz = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match tz.as_bytes().first() {
        None | Some(b'Z' | b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let body = &tz[1..];
            let (oh, om) = match body.split_once(':') {
                Some((a, b)) => (a.parse::<i64>().ok()?, b.parse::<i64>().ok()?),
                None if body.len() == 4 => (
                    body[..2].parse::<i64>().ok()?,
                    body[2..].parse::<i64>().ok()?,
                ),
                None => (body.parse::<i64>().ok()?, 0),
            };
            let mag = oh * 3600 + om * 60;
            if *sign == b'-' { -mag } else { mag }
        }
        _ => return None,
    };

    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - offset)
}

/// Days between 1970-01-01 and a civil date, by Howard Hinnant's algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_is_where_it_should_be() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn a_cached_reset_time_parses_offset_and_fraction() {
        // The shape the cache actually stores.
        let utc = parse_rfc3339("2026-09-17T16:10:00.888709+00:00").unwrap();
        let plain = parse_rfc3339("2026-09-17T16:10:00Z").unwrap();
        assert_eq!(utc, plain);
        // An offset moves the instant the other way.
        let plus2 = parse_rfc3339("2026-09-17T16:10:00+02:00").unwrap();
        assert_eq!(plain - plus2, 2 * 3600);
        let minus5 = parse_rfc3339("2026-09-17T16:10:00-05:00").unwrap();
        assert_eq!(minus5 - plain, 5 * 3600);
    }

    #[test]
    fn a_window_that_already_reset_says_so_instead_of_counting_down() {
        let past = serde_json::json!({
            "utilization": 21,
            "resets_at": "2000-01-01T00:00:00Z",
        });
        let parsed = window(Some(&past)).unwrap();
        assert_eq!(parsed.0, 21);

        let now = 1_700_000_000;
        let w = countdown(parsed, now);
        assert!(w.expired);
        assert!(w.resets_in.is_none());

        // The same window, read before it resets, counts down instead.
        let ahead = countdown((21, Some(now as i64 + 90 * 60)), now);
        assert!(!ahead.expired);
        assert_eq!(ahead.resets_in, Some(Duration::from_secs(90 * 60)));
    }

    #[test]
    fn a_null_window_is_simply_absent() {
        assert!(window(Some(&Value::Null)).is_none());
        assert!(window(None).is_none());
    }
}
