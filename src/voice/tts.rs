//! Text to speech: Piper, a neural voice that runs on the CPU in real time.
//!
//! One `piper` process stays up with the voice loaded. Sentences go in on its
//! stdin as JSON lines, each naming the WAV file to write; piper prints the
//! name back when the file is done, which is how its output is told apart.
//! Only a sentence or two is handed over ahead of the speaker, so stopping
//! mid-reply wastes almost nothing and a new reply starts at once.

use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader, Write},
    num::NonZero,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use cpal::traits::HostTrait;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player, buffer::SamplesBuffer};

use super::{job, mic, speech, stt::hide_window};

/// Sentences rendered ahead of the one playing.
const AHEAD: usize = 2;

enum Cmd {
    Say(String),
    Stop,
}

/// The voice, on a thread of its own.
pub struct Tts {
    tx: Sender<Cmd>,
    speaking: Arc<AtomicBool>,
    /// Set by the thread when something went wrong that ended it.
    pub failed: Arc<std::sync::Mutex<Option<String>>>,
}

pub struct Settings {
    pub piper: PathBuf,
    pub model: PathBuf,
    /// 1.0 is the voice's own pace; 1.2 is a fifth faster.
    pub speed: f32,
    /// Part of the name of the output device; empty is the system default.
    pub device: String,
    /// Where the rendered sentences are written, briefly.
    pub scratch: PathBuf,
}

impl Tts {
    pub fn start(s: Settings) -> Result<Self> {
        fs::create_dir_all(&s.scratch)?;
        let sink = open_output(&s.device)?;
        let mut piper = spawn_piper(&s)?;
        let stdin = piper.stdin.take().context("piper has no stdin")?;
        let stdout = piper.stdout.take().context("piper has no stdout")?;
        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if done_tx.send(PathBuf::from(line.trim())).is_err() {
                    break;
                }
            }
        });

        let (tx, rx) = mpsc::channel();
        let speaking = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(std::sync::Mutex::new(None));
        let mut worker = Worker {
            sink,
            player: None,
            piper,
            stdin,
            done: done_rx,
            scratch: s.scratch,
            pending: VecDeque::new(),
            in_flight: VecDeque::new(),
            generation: 0,
            serial: 0,
            speaking: speaking.clone(),
        };
        let failed_in = failed.clone();
        thread::spawn(move || {
            if let Err(e) = worker.run(rx) {
                *failed_in.lock().unwrap_or_else(|p| p.into_inner()) = Some(format!("{e:#}"));
            }
            worker.speaking.store(false, Ordering::Relaxed);
        });
        Ok(Self {
            tx,
            speaking,
            failed,
        })
    }

    pub fn say(&self, text: &str) {
        if !text.trim().is_empty() {
            let _ = self.tx.send(Cmd::Say(text.to_string()));
        }
    }

    /// Stop now: what is playing and everything queued behind it.
    pub fn stop(&self) {
        self.speaking.store(false, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Stop);
    }

    pub fn is_speaking(&self) -> bool {
        self.speaking.load(Ordering::Relaxed)
    }
}

struct Worker {
    sink: MixerDeviceSink,
    /// Replaced rather than reused after a stop: a stopped player stays so.
    player: Option<Player>,
    piper: Child,
    stdin: ChildStdin,
    done: Receiver<PathBuf>,
    scratch: PathBuf,
    /// Sentences not yet handed to piper.
    pending: VecDeque<String>,
    /// Files piper is writing, with the generation they belong to.
    in_flight: VecDeque<(u64, PathBuf)>,
    /// Bumped by every stop; audio of an older generation is thrown away.
    generation: u64,
    serial: u64,
    speaking: Arc<AtomicBool>,
}

impl Worker {
    fn run(&mut self, rx: Receiver<Cmd>) -> Result<()> {
        loop {
            match rx.recv_timeout(Duration::from_millis(15)) {
                Ok(Cmd::Say(text)) => {
                    self.pending.extend(speech::sentences(&text));
                }
                Ok(Cmd::Stop) => {
                    self.pending.clear();
                    self.generation += 1;
                    if let Some(p) = self.player.take() {
                        p.stop();
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
            if let Some(status) = self.piper.try_wait()? {
                bail!("piper exited ({status})");
            }

            while let Ok(path) = self.done.try_recv() {
                let Some(pos) = self.in_flight.iter().position(|(_, p)| same_file(p, &path)) else {
                    let _ = fs::remove_file(&path);
                    continue;
                };
                let (generation, file) = self.in_flight.remove(pos).expect("found above");
                if generation == self.generation
                    && let Some(buf) = load_wav(&file)
                {
                    let mixer = self.sink.mixer();
                    self.player
                        .get_or_insert_with(|| Player::connect_new(mixer))
                        .append(buf);
                }
                let _ = fs::remove_file(&file);
            }

            let queued = self.player.as_ref().map_or(0, Player::len);
            while self.in_flight.len() + queued < AHEAD
                && let Some(sentence) = self.pending.pop_front()
            {
                self.serial += 1;
                let file = self.scratch.join(format!("s{}.wav", self.serial));
                let line = serde_json::json!({
                    "text": sentence,
                    "output_file": file.display().to_string(),
                });
                writeln!(self.stdin, "{line}")?;
                self.stdin.flush()?;
                self.in_flight.push_back((self.generation, file));
            }

            let playing = self.player.as_ref().is_some_and(|p| !p.empty());
            let busy = playing
                || !self.pending.is_empty()
                || self.in_flight.iter().any(|(g, _)| *g == self.generation);
            self.speaking.store(busy, Ordering::Relaxed);
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.piper.kill();
        let _ = self.piper.wait();
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

fn spawn_piper(s: &Settings) -> Result<Child> {
    let speed = if s.speed > 0.1 { s.speed } else { 1.0 };
    let mut cmd = Command::new(&s.piper);
    cmd.arg("--model")
        .arg(&s.model)
        .arg("--json-input")
        .args(["--length_scale", &format!("{:.3}", 1.0 / speed)])
        .args(["--sentence_silence", "0.15"])
        .current_dir(s.piper.parent().unwrap_or(Path::new(".")))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_window(&mut cmd);
    let child = cmd
        .spawn()
        .with_context(|| format!("cannot start {}", s.piper.display()))?;
    job::adopt(&child);
    Ok(child)
}

/// The output named in the settings, or the system default. Its errors go
/// nowhere: rodio's own handler prints to stderr, which is the fleet's screen.
fn open_output(device: &str) -> Result<MixerDeviceSink> {
    let builder = if device.trim().is_empty() {
        DeviceSinkBuilder::from_default_device().context("no audio output")?
    } else {
        let dev = find_output(device)
            .with_context(|| format!("no audio output named like \"{device}\""))?;
        DeviceSinkBuilder::from_device(dev).context("cannot use that audio output")?
    };
    let mut sink = builder
        .with_error_callback(|_| {})
        .open_stream()
        .context("cannot open the audio output")?;
    sink.log_on_drop(false);
    Ok(sink)
}

fn find_output(part: &str) -> Option<cpal::Device> {
    mic::matching(cpal::default_host().output_devices().ok()?, part)
}

/// A 16-bit PCM WAV as a playable buffer.
fn load_wav(path: &Path) -> Option<SamplesBuffer> {
    let bytes = fs::read(path).ok()?;
    let (channels, rate, data) = parse_wav(&bytes)?;
    let samples: Vec<f32> = data
        .chunks_exact(2)
        .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
        .collect();
    Some(SamplesBuffer::new(
        NonZero::new(channels)?,
        NonZero::new(rate)?,
        samples,
    ))
}

/// Channels, sample rate and the PCM bytes of a WAV file.
fn parse_wav(bytes: &[u8]) -> Option<(u16, u32, &[u8])> {
    if bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut at = 12;
    let mut fmt = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        let body = bytes.get(at + 8..(at + 8 + len).min(bytes.len()))?;
        if id == b"fmt " && body.len() >= 16 {
            let bits = u16::from_le_bytes([body[14], body[15]]);
            if bits != 16 {
                return None;
            }
            let channels = u16::from_le_bytes([body[2], body[3]]);
            let rate = u32::from_le_bytes(body[4..8].try_into().ok()?);
            fmt = Some((channels, rate));
        } else if id == b"data" {
            let (c, r) = fmt?;
            return Some((c, r, body));
        }
        at += 8 + len + (len & 1);
    }
    None
}

fn same_file(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    norm(a) == norm(b)
}

/// The output devices, by name, for `claude-fleet voice devices`.
pub fn output_names() -> Vec<String> {
    cpal::default_host()
        .output_devices()
        .map(|it| it.filter_map(|d| mic::device_name(&d)).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wav_written_for_the_recogniser_reads_back() {
        let bytes = super::super::stt::wav(&[1, 2, 3], 22_050);
        let (c, r, data) = parse_wav(&bytes).unwrap();
        assert_eq!((c, r, data.len()), (1, 22_050, 6));
    }
}
