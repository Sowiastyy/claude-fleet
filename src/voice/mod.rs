//! VOICE: talking to the selected session, and hearing it answer.
//!
//! Everything runs on this machine. The microphone stays open; a small voice
//! detector cuts it into utterances, and whisper.cpp on the GPU turns each one
//! into text. A few short phrases steer the fleet itself — stop, repeat, yes
//! or no to a permission prompt, switch to a session — and anything else is
//! typed into the selected session and sent.
//!
//! The session's transcript is followed from the moment voice starts watching
//! it: every block of text Claude writes is cleaned up for the ear and spoken
//! by Piper, sentence by sentence, while the next one renders. Starting to
//! talk cuts the voice off mid-word. Other sessions are not read out, only
//! announced when they finish or stop on a question.

pub mod engines;
mod job;
pub mod mic;
pub mod speech;
pub mod stt;
mod transcript;
pub mod tts;

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::Ordering,
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    app::{App, Mode},
    config::{self, VoiceCfg},
    history,
};
use speech::{Intent, Phrases};

/// How often the followed transcript is read and the sessions' states are
/// compared for announcements.
const FOLLOW_POLL: Duration = Duration::from_millis(250);
/// How long after the voice falls silent something heard may still be its
/// own echo off the speakers.
const ECHO_WINDOW: Duration = Duration::from_secs(3);
/// How far back an echo is looked for in what was said.
const SAID_KEPT: Duration = Duration::from_secs(90);
/// A share of heard words also in what was just said, past which it is echo.
const ECHO_SCORE: f32 = 0.7;

/// What the list shows about voice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Off,
    Starting,
    Listening,
    Hearing,
    Transcribing,
    Speaking,
}

impl Status {
    pub fn word(self) -> &'static str {
        match self {
            Status::Off => "off",
            Status::Starting => "starting…",
            Status::Listening => "listening",
            Status::Hearing => "hearing you",
            Status::Transcribing => "recognising…",
            Status::Speaking => "speaking",
        }
    }
}

struct Engines {
    mic: mic::Mic,
    mic_rx: Receiver<mic::MicEvent>,
    stt: stt::Worker,
    tts: tts::Tts,
}

/// The session whose replies are being read out.
struct Follow {
    uid: u64,
    session_id: String,
    follower: Option<transcript::Follower>,
    /// Whether a transcript found later should be read from its start: true
    /// when it did not exist yet as following began, so all of it is new.
    from_start: bool,
    last_lookup: Option<Instant>,
}

#[derive(Default)]
pub struct Voice {
    starting: Option<Receiver<Result<Engines, String>>>,
    engines: Option<Engines>,
    hearing: bool,
    transcribing: usize,
    follow: Option<Follow>,
    last_follow_poll: Option<Instant>,
    /// Each session's state as of the last look, by uid, for announcements.
    statuses: HashMap<u64, String>,
    /// What the voice said lately, to recognise it coming back in.
    said: VecDeque<(Instant, String)>,
    last_speaking: Option<Instant>,
    last_reply: Option<String>,
    last_status: Option<Status>,
    /// Messages waiting for their session to take typed text, by uid, in
    /// the order they were said. One goes in at a time: a second typed while
    /// the first one's Enter is still pending would join it.
    outbox: VecDeque<(u64, String)>,
    /// What was heard last, and when.
    pub heard: Option<(String, Instant)>,
}

impl Voice {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_on(&self) -> bool {
        self.engines.is_some() || self.starting.is_some()
    }

    pub fn status(&self) -> Status {
        match &self.engines {
            None if self.starting.is_some() => Status::Starting,
            None => Status::Off,
            Some(e) if e.tts.is_speaking() => Status::Speaking,
            Some(_) if self.hearing => Status::Hearing,
            Some(_) if self.transcribing > 0 => Status::Transcribing,
            Some(_) => Status::Listening,
        }
    }

    fn start(&mut self) {
        let cfg = config::voice();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(start_engines(&cfg).map_err(|e| format!("{e:#}")));
        });
        self.starting = Some(rx);
    }

    fn stop(&mut self) {
        // Dropping the engines ends their threads, and the threads take the
        // whisper and piper processes down with them.
        *self = Self::default();
    }

    fn speaking(&self) -> bool {
        self.engines.as_ref().is_some_and(|e| e.tts.is_speaking())
    }

    fn say(&mut self, text: &str) {
        let Some(e) = &self.engines else {
            return;
        };
        e.tts.say(text);
        let now = Instant::now();
        self.said.push_back((now, text.to_string()));
        while self
            .said
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > SAID_KEPT)
        {
            self.said.pop_front();
        }
    }

    fn silence(&mut self) {
        if let Some(e) = &self.engines {
            e.tts.stop();
        }
    }
}

fn start_engines(cfg: &VoiceCfg) -> anyhow::Result<Engines> {
    let paths = engines::locate(cfg).map_err(anyhow::Error::msg)?;
    let scratch = std::env::temp_dir()
        .join("claude-fleet")
        .join(format!("voice-{}", std::process::id()));
    // The quick one first: a missing audio output fails before the model has
    // been loaded for nothing.
    let tts = tts::Tts::start(tts::Settings {
        piper: paths.piper.clone(),
        model: paths.voice_model.clone(),
        speed: cfg.speed,
        device: cfg.output_device.clone(),
        scratch,
    })?;
    let whisper = stt::Whisper::start(
        &paths.whisper_server,
        &paths.whisper_model,
        &cfg.language,
        engines::whisper_threads(),
        &paths.root.join("whisper-server.log"),
    )?;
    let (tx, mic_rx) = mpsc::channel();
    let mic = mic::Mic::start(&cfg.input_device, tuning(cfg), tx)?;
    Ok(Engines {
        mic,
        mic_rx,
        stt: stt::Worker::spawn(whisper),
        tts,
    })
}

pub fn tuning(cfg: &VoiceCfg) -> mic::Tuning {
    mic::Tuning {
        threshold: cfg.vad_threshold.clamp(0.05, 0.95),
        min_speech_ms: cfg.min_speech_ms,
        end_silence_ms: cfg.end_silence_ms.max(200),
    }
}

/// The prompt that nudges the recogniser towards the words this fleet uses:
/// the trade in general, the sessions by name, and whatever the config adds.
pub fn vocabulary(names: &[String], cfg: &VoiceCfg) -> String {
    let mut s = if cfg.language.starts_with("pl") {
        "Rozmowa z Claude, asystentem programisty. Sesja, repozytorium, commit, plik, funkcja, błąd, test.".to_string()
    } else {
        "A conversation with Claude, a coding assistant. Session, repository, commit, file, function, error, test.".to_string()
    };
    for n in names.iter().take(16) {
        s.push(' ');
        s.push_str(n);
    }
    if !cfg.vocabulary.trim().is_empty() {
        s.push(' ');
        s.push_str(cfg.vocabulary.trim());
    }
    s
}

/// Appended to the system prompt of sessions started while voice is on, so
/// their replies are written to be heard.
const SPOKEN_STYLE: &str = "The user is talking to you by voice through claude-fleet: their \
messages are transcribed speech, and your text replies are read aloud by a speech synthesizer. \
Transcription can mishear names and code words; when one looks wrong, go by what makes sense in \
context rather than asking. Write replies for the ear: open with the answer in one to three short, \
plain sentences, with no markdown, code, tables, lists or file paths in that opening. Anything \
that has to be read goes after it, and say that it is on screen. Keep narration between tool \
calls to a few words. Answer in the language the user speaks.";

/// Extra `claude` arguments for a session started now.
pub fn session_args(voice: &Voice) -> Vec<String> {
    if voice.is_on() && config::voice().spoken_style {
        vec!["--append-system-prompt".into(), SPOKEN_STYLE.into()]
    } else {
        Vec::new()
    }
}

/// What a wake word leaves of an utterance: the rest when it starts with it,
/// nothing when it does not. No wake word configured lets everything through.
fn after_wake_word(text: &str, wake: &str) -> Option<String> {
    let wake = speech::normalize(wake);
    if wake.is_empty() {
        return Some(text.to_string());
    }
    let norm = speech::normalize(text);
    if norm != wake && !norm.starts_with(&format!("{wake} ")) {
        return None;
    }
    let skip = wake.split_whitespace().count();
    let rest: Vec<&str> = text.split_whitespace().skip(skip).collect();
    Some(
        rest.join(" ")
            .trim_start_matches(|c: char| ",.:;!-–—".contains(c) || c.is_whitespace())
            .to_string(),
    )
}

impl App {
    /// `v` in the list, Alt+Shift+V anywhere.
    pub fn toggle_voice(&mut self) {
        if self.voice.is_on() {
            self.voice.stop();
            self.notify("voice off");
        } else {
            self.voice.start();
            self.notify("voice: starting the engines…");
        }
    }

    /// Everything voice does between frames.
    pub fn poll_voice(&mut self) {
        self.voice_started();
        self.voice_poll_engines();
        let status = self.voice.status();
        if self.voice.last_status != Some(status) {
            self.voice.last_status = Some(status);
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    fn voice_started(&mut self) {
        let Some(rx) = &self.voice.starting else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(engines)) => {
                let mic = engines.mic.name.clone();
                self.voice.starting = None;
                self.voice.engines = Some(engines);
                // Whatever the sessions are doing now is not news.
                self.voice.statuses = self
                    .voice_states()
                    .into_iter()
                    .map(|(uid, (state, _, _))| (uid, state))
                    .collect();
                let p = phrases();
                self.voice.say(p.listening);
                self.notify(format!("voice on — listening on {mic}"));
            }
            Ok(Err(e)) => {
                self.voice.starting = None;
                self.notify(format!("voice: {e}"));
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.voice.starting = None;
                self.notify("voice: the engines did not start");
            }
        }
    }

    fn voice_poll_engines(&mut self) {
        let Some(e) = &self.voice.engines else {
            return;
        };
        let failed = e.tts.failed.lock().ok().and_then(|g| g.clone());
        if let Some(err) = failed {
            self.voice.stop();
            self.notify(format!("voice off: {err}"));
            return;
        }
        if e.tts.is_speaking() {
            self.voice.last_speaking = Some(Instant::now());
        }
        let cfg = config::voice();

        let events: Vec<mic::MicEvent> = e.mic_rx.try_iter().collect();
        for ev in events {
            match ev {
                mic::MicEvent::SpeechStart => {
                    self.voice.hearing = true;
                    if cfg.barge_in && self.voice.speaking() {
                        self.voice.silence();
                    }
                }
                mic::MicEvent::Utterance(pcm) => {
                    self.voice.hearing = false;
                    self.voice.transcribing += 1;
                    let prompt = vocabulary(&self.voice_names(), &cfg);
                    if let Some(e) = &self.voice.engines {
                        e.stt.submit(pcm, prompt);
                    }
                }
                mic::MicEvent::Discarded => self.voice.hearing = false,
                mic::MicEvent::Failed(err) => {
                    self.voice.stop();
                    self.notify(format!("voice off: {err}"));
                    return;
                }
            }
        }

        let results: Vec<Result<String, String>> = match &self.voice.engines {
            Some(e) => e.stt.rx.try_iter().collect(),
            None => return,
        };
        for r in results {
            self.voice.transcribing = self.voice.transcribing.saturating_sub(1);
            match r {
                Ok(text) => self.voice_heard(&text, &cfg),
                Err(err) => self.notify(format!("voice: {err}")),
            }
            if self.voice.engines.is_none() {
                // "Wyłącz głos" was among them.
                return;
            }
        }

        self.voice_send();

        if self
            .voice
            .last_follow_poll
            .is_none_or(|t| t.elapsed() >= FOLLOW_POLL)
        {
            self.voice.last_follow_poll = Some(Instant::now());
            self.voice_follow(&cfg);
            self.voice_announce(&cfg);
        }
    }

    /// Hand the next queued message to its session once it can take one.
    fn voice_send(&mut self) {
        let Some(&(uid, _)) = self.voice.outbox.front() else {
            return;
        };
        match self.sessions.iter().position(|s| s.uid == uid) {
            Some(i) if self.sessions[i].is_alive() => {
                if !self.sessions[i].prompt_pending() {
                    let (_, text) = self.voice.outbox.pop_front().expect("peeked above");
                    self.sessions[i].queue_submit(&text);
                }
            }
            // Its session is gone; so is what was meant for it.
            _ => self.voice.outbox.retain(|(u, _)| *u != uid),
        }
    }

    /// The session spoken input goes to: the selected one, if it is a live
    /// Claude session.
    fn voice_target(&self) -> Option<usize> {
        let s = self.sessions.get(self.selected)?;
        (s.is_alive() && !s.shell).then_some(self.selected)
    }

    /// What a session is called out loud: the name Claude Code gave it, or
    /// the fleet's label.
    fn voice_label(&self, idx: usize) -> String {
        self.entry_for(idx)
            .map(|e| e.name.clone())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| self.sessions[idx].label.clone())
    }

    /// Names worth priming the recogniser with.
    fn voice_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for (i, s) in self.sessions.iter().enumerate() {
            for n in [
                s.label.clone(),
                self.voice_label(i),
                s.cwd
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ] {
                if !n.is_empty() && !names.contains(&n) {
                    names.push(n);
                }
            }
        }
        names
    }

    /// A session by what someone called it: its name, label or directory,
    /// whole before prefix before part.
    fn voice_find(&self, spoken: &str) -> Option<usize> {
        let want = speech::normalize(spoken);
        let mut best: Option<(u8, usize)> = None;
        for (i, s) in self.sessions.iter().enumerate() {
            let dir = s
                .cwd
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            for name in [s.label.clone(), self.voice_label(i), dir] {
                let n = speech::normalize(&name);
                if n.is_empty() {
                    continue;
                }
                let score = if n == want {
                    3
                } else if n.starts_with(&want) || want.starts_with(&n) {
                    2
                } else if n.contains(&want) || want.contains(&n) {
                    1
                } else {
                    0
                };
                if score > 0 && best.is_none_or(|(b, _)| score > b) {
                    best = Some((score, i));
                }
            }
        }
        best.map(|(_, i)| i)
    }

    fn voice_heard(&mut self, raw: &str, cfg: &VoiceCfg) {
        let text = raw.trim();
        if speech::is_hallucination(text) {
            return;
        }
        // Out of near-silence Whisper sometimes gives back its own prompt,
        // whole. A word or two from it ("wanderers", "test") is just speech.
        if text.split_whitespace().count() >= 4
            && speech::echo_score(text, &vocabulary(&self.voice_names(), cfg)) >= 0.9
        {
            return;
        }
        let recently_spoke = self.voice.speaking()
            || self
                .voice
                .last_speaking
                .is_some_and(|t| t.elapsed() < ECHO_WINDOW);
        if recently_spoke {
            let said: Vec<&str> = self.voice.said.iter().map(|(_, s)| s.as_str()).collect();
            if speech::echo_score(text, &said.join(" ")) >= ECHO_SCORE {
                return;
            }
        }
        let Some(text) = after_wake_word(text, &cfg.wake_word) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        self.voice.heard = Some((text.clone(), Instant::now()));
        self.notify(format!("heard: {text}"));

        let p = Phrases::for_lang(&cfg.language);
        let target = self.voice_target();
        let waiting = target
            .and_then(|i| self.entry_for(i))
            .is_some_and(|e| e.is_waiting());
        match speech::intent(&text, waiting) {
            Intent::Silence => self.voice.silence(),
            Intent::Interrupt => {
                self.voice.silence();
                if let Some(i) = target {
                    let _ = self.sessions[i].write_input(b"\x1b");
                    self.voice.say(p.interrupted);
                }
            }
            Intent::Repeat => match self.voice.last_reply.clone() {
                Some(r) => self.voice.say(&r),
                None => self.voice.say(p.nothing_to_repeat),
            },
            Intent::Allow(rest) => {
                if let Some(i) = target {
                    // The first option, "Yes", is the one a dialog opens on.
                    let _ = self.sessions[i].write_input(b"\r");
                    if let Some(r) = rest {
                        self.voice.outbox.push_back((self.sessions[i].uid, r));
                    }
                }
            }
            Intent::Deny(rest) => {
                if let Some(i) = target {
                    let _ = self.sessions[i].write_input(b"\x1b");
                    // Esc is "no, and tell Claude what to do instead"; the
                    // rest of the sentence is that.
                    if let Some(r) = rest {
                        self.voice.outbox.push_back((self.sessions[i].uid, r));
                    }
                }
            }
            Intent::Switch(name) => match self.voice_find(&name) {
                Some(i) => {
                    // Following starts over on the new session without
                    // silencing the old one's reply: the sentence below is it.
                    self.voice.follow = None;
                    self.select_index(i);
                    if self.sessions[i].is_alive() {
                        self.mode = Mode::Focus;
                    }
                    let label = self.voice_label(i);
                    self.voice.say(&format!("{} {label}.", p.switched));
                }
                None => self.voice.say(p.no_such_session),
            },
            Intent::NewSession => {
                let cwd = self.default_cwd();
                if let Err(e) = self.spawn_session(cwd) {
                    self.notify(format!("cannot start a session: {e:#}"));
                }
            }
            Intent::VoiceOff => {
                self.voice.stop();
                self.notify("voice off");
            }
            Intent::Message(m) => match target {
                Some(i) if !m.is_empty() => {
                    self.voice.outbox.push_back((self.sessions[i].uid, m));
                }
                Some(_) => {}
                None => self.voice.say(p.no_session),
            },
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Read out what the selected session wrote since the last look.
    fn voice_follow(&mut self, cfg: &VoiceCfg) {
        let Some(i) = self.voice_target() else {
            self.voice.follow = None;
            return;
        };
        let uid = self.sessions[i].uid;
        let sid = self
            .entry_for(i)
            .map(|e| e.session_id.clone())
            .unwrap_or_default();
        let same = self
            .voice
            .follow
            .as_ref()
            .is_some_and(|f| f.uid == uid && f.session_id == sid);
        if !same {
            let other_session = self.voice.follow.as_ref().is_none_or(|f| f.uid != uid);
            if other_session && self.voice.follow.is_some() {
                // The old one's reply is not what this pane shows any more.
                self.voice.silence();
            }
            self.voice.follow = Some(Follow {
                uid,
                session_id: sid,
                follower: None,
                // The same session on a new conversation (`/clear`): its new
                // transcript is news from its first line.
                from_start: !other_session,
                last_lookup: None,
            });
        }

        let texts = {
            let f = self.voice.follow.as_mut().expect("set above");
            if f.follower.is_none()
                && !f.session_id.is_empty()
                && f.last_lookup.is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
            {
                f.last_lookup = Some(Instant::now());
                match history::transcript_path(&f.session_id) {
                    Some(path) if f.from_start => {
                        f.follower = Some(transcript::Follower::from_start(path));
                    }
                    Some(path) => f.follower = Some(transcript::Follower::from_end(path)),
                    // Written with the first message; everything in it once
                    // it exists is new.
                    None => f.from_start = true,
                }
            }
            f.follower.as_mut().map(|fl| fl.poll()).unwrap_or_default()
        };
        for t in texts {
            if let Some(s) = speech::speakable(&t, cfg.max_spoken_chars, &cfg.language) {
                self.voice.last_reply = Some(s.clone());
                self.voice.say(&s);
            }
        }
    }

    /// Each session's state, label and index, by uid. A waiting session's
    /// state says what for: `waiting:permission prompt`.
    fn voice_states(&self) -> HashMap<u64, (String, String, usize)> {
        let mut out = HashMap::new();
        for (i, s) in self.sessions.iter().enumerate() {
            if s.shell {
                continue;
            }
            let state = if !s.is_alive() {
                "finished".to_string()
            } else {
                match self.entry_for(i) {
                    Some(e) if e.is_waiting() => format!("waiting:{}", e.waiting_for),
                    Some(e) => e.status.clone(),
                    None => "starting".to_string(),
                }
            };
            out.insert(s.uid, (state, self.voice_label(i), i));
        }
        out
    }

    /// Say what changed: the selected session stopping on a question, others
    /// finishing or stopping.
    fn voice_announce(&mut self, cfg: &VoiceCfg) {
        let p = Phrases::for_lang(&cfg.language);
        let target = self.voice_target();
        let now = self.voice_states();
        let mut lines = Vec::new();
        for (uid, (state, label, idx)) in &now {
            let before = self.voice.statuses.get(uid).map(String::as_str);
            if before == Some(state.as_str()) {
                continue;
            }
            let is_target = target == Some(*idx);
            if let Some(what) = state.strip_prefix("waiting:") {
                // Waiting again on a different wording of the same stop is
                // not a new question.
                if before.is_some_and(|b| b.starts_with("waiting:")) {
                    continue;
                }
                if is_target {
                    let follower = self
                        .voice
                        .follow
                        .as_ref()
                        .filter(|f| f.uid == *uid)
                        .and_then(|f| f.follower.as_ref());
                    let tool = follower.and_then(|f| f.last_tool.clone());
                    let question = follower.and_then(|f| f.last_question.clone());
                    let speak = |t: &str| speech::speakable(t, 300, &cfg.language);
                    lines.push(if what.contains("permission") {
                        match tool.as_deref().and_then(speak) {
                            Some(t) => format!("{} {t} {}", p.permission_for, p.say_yes_no),
                            None => format!("{} {}", p.permission, p.say_yes_no),
                        }
                    } else if let Some(q) = question.as_deref().and_then(speak) {
                        format!("{} {q}", p.claude_asks)
                    } else {
                        p.claude_waits.to_string()
                    });
                } else if cfg.announce {
                    lines.push(format!("{} {label} {}", p.session, p.asks));
                }
            } else if cfg.announce
                && !is_target
                && before == Some("busy")
                && matches!(state.as_str(), "idle" | "finished")
            {
                lines.push(format!("{} {label} {}", p.session, p.finished));
            }
        }
        self.voice.statuses = now.into_iter().map(|(uid, (s, _, _))| (uid, s)).collect();
        for l in lines {
            self.voice.say(&l);
        }
    }
}

fn phrases() -> &'static Phrases {
    Phrases::for_lang(&config::voice().language)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wake_word_gates_and_is_cut_off() {
        assert_eq!(after_wake_word("Zrób testy", "").as_deref(), Some("Zrób testy"));
        assert_eq!(after_wake_word("Zrób testy", "Claude"), None);
        assert_eq!(
            after_wake_word("Claude, zrób testy.", "claude").as_deref(),
            Some("zrób testy.")
        );
    }
}
