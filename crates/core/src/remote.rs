//! Claude Code's cloud sessions (the ones claude.ai/code shows) over
//! Anthropic's API, the way Claude Code's own `--cloud` and `--teleport`
//! reach them: with the account's claude.ai OAuth token and the
//! organisation it belongs to. No claude.ai page, so no second sign-in.
//!
//! **Undocumented.** These are the endpoints Claude Code 2.1 calls, read off
//! what it sends; any release can change them.
//!
//! - `GET  /v1/code/sessions/{id}/events?limit&sort_order=desc&cursor` — the
//!   session's events, newest first, a page at a time.
//! - `GET  /v1/code/sessions/{id}/events/stream?from_sequence_num` — every
//!   event after that one as server-sent events, held open.
//! - `POST /v1/code/sessions/{id}/events` — a message, or an interrupt.
//!
//! An event's `payload` is what `claude -p --output-format stream-json`
//! prints, so a cloud session reads like a local one.
//!
//! HTTP goes through curl, as the rest of the fleet's does (see `update`).
//! The token reaches curl on stdin, as its config, never on its command line
//! where any process could read it.

use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{accounts::Accounts, repos::no_window, usage::parse_rfc3339};

const API: &str = "https://api.anthropic.com";
/// Where Claude Code refreshes its claude.ai login, as Claude Code.
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// A token this close to running out is refreshed before it is used.
const REFRESH_AHEAD_MS: u64 = 5 * 60 * 1000;
/// The most events a page holds.
const PAGE: usize = 1000;

/// One refresh at a time: two at once would spend the same refresh token
/// twice, and the second would be refused.
static REFRESHING: Mutex<()> = Mutex::new(());

/// Talks to the API as one account.
pub struct Client {
    token: String,
    org: String,
}

/// One event of a cloud session.
#[derive(Debug, Clone)]
pub struct Row {
    /// Its place in the session, counting from 1.
    pub seq: u64,
    /// When the service took it, in milliseconds since the epoch.
    pub at_ms: u64,
    pub payload: Value,
}

/// Ends a [`Client::stream`] from another thread, whenever it is asked:
/// before the stream has started, or while it waits on the service.
#[derive(Default)]
pub struct Stopper {
    stopped: AtomicBool,
    /// The curl holding the stream open.
    curl: Mutex<Option<Child>>,
}

impl Stopper {
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(mut c) = self.curl.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

/// Why a stream stopped.
#[derive(Debug, PartialEq)]
pub enum StreamEnd {
    /// The service closed it, or the connection dropped: dial again.
    Closed,
    /// The token was refused: refresh it, then dial again.
    Unauthorized,
    /// There is no such session, or not for this account.
    Gone(String),
}

impl Client {
    /// The client for `account` (empty: whatever `~/.claude` is signed in
    /// as), its token refreshed first when it is about to run out.
    pub fn for_account(accounts: &Mutex<Accounts>, account: &str) -> Result<Self> {
        Self::signed_in(accounts, account, false)
    }

    /// Like [`Self::for_account`], with the token refreshed even if it looks
    /// current: the service has just refused it.
    pub fn refreshed(accounts: &Mutex<Accounts>, account: &str) -> Result<Self> {
        Self::signed_in(accounts, account, true)
    }

    fn signed_in(accounts: &Mutex<Accounts>, account: &str, force: bool) -> Result<Self> {
        let read = || accounts.lock().unwrap().login_for(account);
        let mut login = read()?;
        let stale = |l: &crate::accounts::Login| l.expires_at() <= now_ms() + REFRESH_AHEAD_MS;
        if force || stale(&login) {
            let _one = REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
            // Someone may have refreshed it while this waited: theirs will do.
            let again = read()?;
            let done_meanwhile = again.refresh_token() != login.refresh_token() && !stale(&again);
            login = again;
            if !done_meanwhile {
                let creds = refresh(&login.creds)?;
                accounts.lock().unwrap().save_login(account, &creds)?;
                login.creds = creds;
            }
        }
        if login.access_token().is_empty() {
            bail!("the account holds no claude.ai token — log in to it again");
        }
        if login.org.is_empty() {
            bail!("Claude Code has not written down the account's organisation yet — start a session as it once");
        }
        Ok(Self {
            token: login.access_token().to_string(),
            org: login.org,
        })
    }

    fn headers(&self) -> Vec<(&'static str, String)> {
        vec![
            ("Authorization", format!("Bearer {}", self.token)),
            ("anthropic-version", "2023-06-01".to_string()),
            ("x-organization-uuid", self.org.clone()),
            ("User-Agent", format!("claude-fleet/{}", env!("CARGO_PKG_VERSION"))),
        ]
    }

    /// A page of a session's events, newest first, from just before
    /// `before` (or the newest). Also returns where the next older page
    /// starts, when there is one.
    pub fn page(&self, id: &str, before: Option<u64>) -> Result<(Vec<Row>, Option<u64>)> {
        let mut url = format!("{API}/v1/code/sessions/{}/events?limit={PAGE}&sort_order=desc", path_part(id)?);
        if let Some(c) = before {
            url.push_str(&format!("&cursor={c}"));
        }
        let v = self.get(&url)?;
        let rows = v
            .get("data")
            .and_then(Value::as_array)
            .map(|d| d.iter().filter_map(row_of).collect())
            .unwrap_or_default();
        Ok((rows, v.get("next_cursor").and_then(seq_of)))
    }

    /// The session's last `max` events or so, oldest first, and whether
    /// older ones were left out.
    pub fn history(&self, id: &str, max: usize) -> Result<(Vec<Row>, bool)> {
        let mut rows = Vec::new();
        let mut before = None;
        loop {
            let (page, next) = self.page(id, before)?;
            let empty = page.is_empty();
            rows.extend(page);
            match next {
                Some(n) if !empty && rows.len() < max => before = Some(n),
                Some(_) if !empty => {
                    rows.reverse();
                    return Ok((rows, true));
                }
                _ => break,
            }
        }
        rows.reverse();
        Ok((rows, false))
    }

    /// Post one event to a session: a message, an interrupt.
    pub fn send(&self, id: &str, event: &Value) -> Result<()> {
        let url = format!("{API}/v1/code/sessions/{}/events", path_part(id)?);
        let body = json!({ "events": [event] }).to_string();
        let (status, raw) = request(&url, &self.headers(), Some(&body), 30)?;
        if !(200..300).contains(&status) {
            bail!("{}", refusal(status, &raw));
        }
        Ok(())
    }

    /// Follow a session from just after event `from`, handing each new
    /// event to `on_row` until the stream ends or `stop` is used.
    pub fn stream(&self, id: &str, from: u64, stop: &Stopper, mut on_row: impl FnMut(Row)) -> Result<StreamEnd> {
        let url = format!("{API}/v1/code/sessions/{}/events/stream?from_sequence_num={from}", path_part(id)?);
        let mut headers = self.headers();
        headers.push(("Accept", "text/event-stream".to_string()));
        if from > 0 {
            headers.push(("Last-Event-ID", from.to_string()));
        }
        let mut child = no_window(Command::new("curl"))
            // `-f`: an HTTP error is an exit code, not a body read as events.
            // `-y`/`-Y`: a connection silent for this long (keepalives
            // included) is dead.
            .args(["-sSfN", "--connect-timeout", "20", "-y", "90", "-Y", "1", "--config", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("could not run curl")?;
        let mut stdin = child.stdin.take().context("curl has no stdin")?;
        let stdout = child.stdout.take().context("curl has no stdout")?;
        stdin.write_all(config(&url, &headers, None).as_bytes())?;
        drop(stdin);
        {
            // Checked under the lock `stop` kills under: a stop that came
            // first is seen here, one that comes later finds the curl.
            let mut slot = stop.curl.lock().unwrap();
            if stop.stopped() {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(StreamEnd::Closed);
            }
            *slot = Some(child);
        }

        let mut event = String::new();
        let mut data = String::new();
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                if (event.is_empty() || event == "client_event")
                    && let Ok(v) = serde_json::from_str::<Value>(&data)
                    && let Some(row) = row_of(&v)
                {
                    on_row(row);
                }
                event.clear();
                data.clear();
            } else if let Some(v) = line.strip_prefix("event:") {
                event = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(v.strip_prefix(' ').unwrap_or(v));
            }
        }
        let code = match stop.curl.lock().unwrap().take() {
            Some(mut c) => c.wait().ok().and_then(|s| s.code()),
            // Taken and killed by whoever wanted it stopped.
            None => return Ok(StreamEnd::Closed),
        };
        // 22: the service answered with an error. Ask once more, plainly,
        // to hear which.
        if code == Some(22) {
            let url = format!("{API}/v1/code/sessions/{}/events?limit=1&sort_order=desc", path_part(id)?);
            let (status, raw) = request(&url, &self.headers(), None, 20)?;
            return Ok(match status {
                401 => StreamEnd::Unauthorized,
                400 | 403 | 404 => StreamEnd::Gone(refusal(status, &raw)),
                _ => StreamEnd::Closed,
            });
        }
        Ok(StreamEnd::Closed)
    }

    fn get(&self, url: &str) -> Result<Value> {
        let (status, raw) = request(url, &self.headers(), None, 30)?;
        if status != 200 {
            bail!("{}", refusal(status, &raw));
        }
        serde_json::from_slice(&raw).context("unexpected answer from api.anthropic.com")
    }
}

/// A message to post to a session, in the shape Claude Code posts it.
pub fn user_message(session: &str, uuid: &str, content: Value) -> Value {
    json!({
        "uuid": uuid,
        "session_id": session,
        "type": "user",
        "parent_tool_use_id": null,
        "message": { "role": "user", "content": content },
    })
}

/// Stop the turn in progress, as Esc does.
pub fn interrupt(uuid: &str) -> Value {
    json!({
        "uuid": uuid,
        "type": "control_request",
        "request_id": uuid,
        "request": { "subtype": "interrupt" },
    })
}

/// Get fresh tokens for `creds` from the refresh token in it. Returns
/// `creds` with them in.
fn refresh(creds: &Value) -> Result<Value> {
    let token = creds
        .get("refreshToken")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .context("the account holds no refresh token — log in to it again")?;
    let scopes = creds
        .get("scopes")
        .and_then(Value::as_array)
        .map(|s| s.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    let mut body = json!({
        "grant_type": "refresh_token",
        "refresh_token": token,
        "client_id": CLIENT_ID,
    });
    if !scopes.is_empty() {
        body["scope"] = json!(scopes);
    }
    let headers = [("Content-Type", "application/json".to_string())];
    let (status, raw) = request(TOKEN_URL, &headers, Some(&body.to_string()), 30)?;
    if status != 200 {
        bail!("the claude.ai login could not be refreshed ({}) — log in to the account again", refusal(status, &raw));
    }
    let got: Value = serde_json::from_slice(&raw).context("unexpected answer refreshing the login")?;
    let access = got
        .get("access_token")
        .and_then(Value::as_str)
        .context("no token in the refresh answer")?;
    let mut out = creds.clone();
    out["accessToken"] = json!(access);
    if let Some(r) = got.get("refresh_token").and_then(Value::as_str) {
        out["refreshToken"] = json!(r);
    }
    if let Some(secs) = got.get("expires_in").and_then(Value::as_u64) {
        out["expiresAt"] = json!(now_ms() + secs * 1000);
    }
    if let Some(secs) = got.get("refresh_token_expires_in").and_then(Value::as_u64) {
        out["refreshTokenExpiresAt"] = json!(now_ms() + secs * 1000);
    }
    if let Some(scope) = got.get("scope").and_then(Value::as_str) {
        out["scopes"] = json!(scope.split_whitespace().collect::<Vec<_>>());
    }
    Ok(out)
}

/// One HTTP request through curl: the status and the body.
fn request(url: &str, headers: &[(&str, String)], body: Option<&str>, timeout_secs: u32) -> Result<(u16, Vec<u8>)> {
    let mut all: Vec<(&str, String)> = headers.to_vec();
    if body.is_some() && !all.iter().any(|(k, _)| k.eq_ignore_ascii_case("Content-Type")) {
        all.push(("Content-Type", "application/json".to_string()));
    }
    let mut child = no_window(Command::new("curl"))
        .args(["-sS", "--max-time", &timeout_secs.to_string(), "-w", "\\n%{http_code}", "--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run curl")?;
    let mut stdin = child.stdin.take().context("curl has no stdin")?;
    let cfg = config(url, &all, body);
    // Written from here while curl's output is read below: a body bigger
    // than the pipe would otherwise wait on a curl that waits on its output.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(cfg.as_bytes());
    });
    let mut out = Vec::new();
    child.stdout.take().context("curl has no stdout")?.read_to_end(&mut out)?;
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    let _ = writer.join();
    let status = child.wait()?;
    if !status.success() {
        let why = err.trim();
        bail!("could not reach {}: {}", host_of(url), if why.is_empty() { "curl failed" } else { why });
    }
    let cut = out.iter().rposition(|&b| b == b'\n').unwrap_or(0);
    let code = std::str::from_utf8(&out[cut..]).ok().and_then(|s| s.trim().parse::<u16>().ok()).unwrap_or(0);
    out.truncate(cut);
    Ok((code, out))
}

/// A curl config file for one request. Values are quoted, which in a curl
/// config takes backslash escapes; JSON never holds a raw line break.
fn config(url: &str, headers: &[(&str, String)], body: Option<&str>) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r"));
    let mut c = format!("url = {}\n", quote(url));
    for (k, v) in headers {
        c.push_str(&format!("header = {}\n", quote(&format!("{k}: {v}"))));
    }
    if let Some(b) = body {
        c.push_str(&format!("data-binary = {}\n", quote(b)));
    }
    c
}

/// What the service said when it refused, short.
fn refusal(status: u16, raw: &[u8]) -> String {
    let v: Option<Value> = serde_json::from_slice(raw).ok();
    let msg = v
        .as_ref()
        .and_then(|v| v.pointer("/error/message").or_else(|| v.get("error_description")))
        .and_then(Value::as_str)
        .map(str::to_owned);
    match (status, msg) {
        (401, _) => "the claude.ai login was refused (HTTP 401)".to_string(),
        (404, _) => "no such cloud session for this account (HTTP 404)".to_string(),
        (s, Some(m)) => format!("{m} (HTTP {s})"),
        (0, None) => "no answer".to_string(),
        (s, None) => format!("HTTP {s}"),
    }
}

fn row_of(v: &Value) -> Option<Row> {
    let payload = v.get("payload")?.clone();
    if !payload.is_object() {
        return None;
    }
    Some(Row {
        seq: v.get("sequence_num").and_then(seq_of).unwrap_or(0),
        at_ms: v
            .get("created_at")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339)
            .map(|s| s.max(0) as u64 * 1000)
            .unwrap_or(0),
        payload,
    })
}

/// A sequence number, sent as a string or a number.
fn seq_of(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}

/// A session id is spliced into the URL: it has to be plainly one.
fn path_part(id: &str) -> Result<&str> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("not a cloud session id: {id}");
    }
    Ok(id)
}

fn host_of(url: &str) -> &str {
    url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or(url)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_quotes_what_it_holds() {
        let c = config(
            "https://api.anthropic.com/x?a=1",
            &[("Authorization", r#"Bearer t"k"#.to_string())],
            Some(r#"{"text":"a \"quote\" and a \\ back\nslash"}"#),
        );
        let want = [
            r#"url = "https://api.anthropic.com/x?a=1""#,
            r#"header = "Authorization: Bearer t\"k""#,
            r#"data-binary = "{\"text\":\"a \\\"quote\\\" and a \\\\ back\\nslash\"}""#,
            "",
        ];
        assert_eq!(c, want.join("\n"));
    }

    #[test]
    fn rows_read_numbers_as_strings_or_numbers() {
        let r = row_of(&json!({
            "sequence_num": "1556",
            "created_at": "2026-11-10T11:33:38.997271Z",
            "payload": { "type": "assistant" }
        }))
        .unwrap();
        assert_eq!(r.seq, 1556);
        assert_eq!(r.at_ms, 1_794_310_418_000);
        assert_eq!(row_of(&json!({ "sequence_num": 7, "payload": {} })).unwrap().seq, 7);
        assert!(row_of(&json!({ "sequence_num": 7 })).is_none());
    }

    #[test]
    fn only_a_plain_id_goes_in_a_url() {
        assert!(path_part("cse_01Pr6NybHx7HpBNf8zrBG81j").is_ok());
        assert!(path_part("session_01-abc").is_ok());
        assert!(path_part("../x").is_err());
        assert!(path_part("a?b").is_err());
        assert!(path_part("").is_err());
    }

    #[test]
    fn a_refusal_reads_the_service_message() {
        assert_eq!(refusal(400, br#"{"error":{"message":"bad"}}"#), "bad (HTTP 400)");
        assert_eq!(refusal(401, b""), "the claude.ai login was refused (HTTP 401)");
        assert_eq!(refusal(500, b"oops"), "HTTP 500");
    }
}
