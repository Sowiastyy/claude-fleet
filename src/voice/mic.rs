//! The microphone, always on, cut into utterances.
//!
//! Audio arrives from WASAPI at whatever rate and width the device runs at.
//! It is folded to mono, brought down to the 16 kHz both the voice detector
//! and the recogniser work at, and judged 16 ms at a time by Earshot, a small
//! neural voice detector. A run of voiced frames opens an utterance — which
//! is also the moment the fleet stops talking, if it was — and enough silence
//! closes it and sends it off to be recognised.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use cpal::{
    SampleFormat,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

pub const RATE: u32 = 16_000;
/// Earshot's frame: 256 samples, 16 ms.
const FRAME: usize = 256;
const FRAME_MS: u64 = 16;
/// Kept from before speech starts, so the first syllable is not clipped by
/// the time the detector is sure.
const PRE_ROLL_FRAMES: usize = 40;
/// The window a start is judged over, and how much of it must be voiced.
const START_WINDOW: usize = 16;
/// An utterance longer than this is cut and sent as it stands. Whisper hears
/// thirty seconds at a time; a monologue continues in the next piece.
const MAX_UTTERANCE_MS: u64 = 28_000;
/// How often an utterance still being said is handed over as far as it has
/// got: about a second, in frames.
const SO_FAR_FRAMES: u64 = 60;

pub enum MicEvent {
    /// Someone started talking.
    SpeechStart,
    /// They are still talking: what they have said up to now, to be shown
    /// while the rest is on its way.
    SoFar(Vec<i16>),
    /// They stopped: 16 kHz mono samples of what they said.
    Utterance(Vec<i16>),
    /// Speech that ended too short to be worth recognising.
    Discarded,
    Failed(String),
}

#[derive(Clone)]
pub struct Tuning {
    /// Earshot score a frame needs to count as voice, 0-1.
    pub threshold: f32,
    /// Voiced audio an utterance needs in total to be sent.
    pub min_speech_ms: u64,
    /// Silence that ends an utterance.
    pub end_silence_ms: u64,
}

pub struct Mic {
    stop: Arc<AtomicBool>,
    pub name: String,
}

impl Drop for Mic {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Mic {
    /// Open the microphone and start listening. `device` is part of its name;
    /// empty is the system default.
    pub fn start(device: &str, tuning: Tuning, tx: Sender<MicEvent>) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        // The stream is built on the thread that keeps it: a WASAPI stream
        // belongs where it was made. The name comes back once it is open.
        let (ready_tx, ready_rx) = mpsc::channel::<Result<String, String>>();
        let (stop_in, device) = (stop.clone(), device.to_string());
        thread::spawn(move || {
            if let Err(e) = listen(&device, tuning, &tx, &stop_in, &ready_tx) {
                let msg = format!("{e:#}");
                let _ = ready_tx.send(Err(msg.clone()));
                let _ = tx.send(MicEvent::Failed(msg));
            }
        });
        let name = ready_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| anyhow::anyhow!("the microphone did not open"))?
            .map_err(anyhow::Error::msg)?;
        Ok(Self { stop, name })
    }
}

/// A device as Windows names it, "Mikrofon (H858 Wireless headset)": the
/// short name alone is the same for every headset.
pub fn device_name(d: &cpal::Device) -> Option<String> {
    let desc = d.description().ok()?;
    Some(match (desc.extended().first(), desc.driver()) {
        (Some(full), _) => full.clone(),
        (None, Some(driver)) => format!("{} ({driver})", desc.name()),
        (None, None) => desc.name().to_string(),
    })
}

/// The device whose name contains `part`, ignoring case.
pub fn matching(devices: impl Iterator<Item = cpal::Device>, part: &str) -> Option<cpal::Device> {
    let want = part.trim().to_lowercase();
    devices
        .into_iter()
        .find(|d| device_name(d).is_some_and(|n| n.to_lowercase().contains(&want)))
}

fn find_input(host: &cpal::Host, part: &str) -> Option<cpal::Device> {
    if part.trim().is_empty() {
        return host.default_input_device();
    }
    matching(host.input_devices().ok()?, part)
}

/// The input devices, by name, for `claude-fleet voice devices`.
pub fn input_names() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|it| it.filter_map(|d| device_name(&d)).collect())
        .unwrap_or_default()
}

fn listen(
    device: &str,
    tuning: Tuning,
    tx: &Sender<MicEvent>,
    stop: &AtomicBool,
    ready: &Sender<Result<String, String>>,
) -> Result<()> {
    let host = cpal::default_host();
    let dev = find_input(&host, device).with_context(|| {
        if device.is_empty() {
            "no microphone".to_string()
        } else {
            format!("no microphone named like \"{device}\"")
        }
    })?;
    let name = device_name(&dev).unwrap_or_else(|| "microphone".into());
    let supported = dev
        .default_input_config()
        .context("the microphone has no usable format")?;
    let config = supported.config();
    let channels = usize::from(config.channels.max(1));
    let rate = config.sample_rate;

    // The callback only copies; everything else happens on this thread.
    let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(rate as usize)));
    let err_flag = Arc::new(Mutex::new(None::<String>));
    let on_err = {
        let e = err_flag.clone();
        move |err: cpal::StreamError| {
            *e.lock().unwrap_or_else(|p| p.into_inner()) = Some(err.to_string());
        }
    };
    let stream = match supported.sample_format() {
        SampleFormat::F32 => {
            let b = buf.clone();
            dev.build_input_stream(
                &config,
                move |data: &[f32], _: &_| push_mono(&b, data.iter().copied(), channels),
                on_err,
                None,
            )
        }
        SampleFormat::I16 => {
            let b = buf.clone();
            dev.build_input_stream(
                &config,
                move |data: &[i16], _: &_| {
                    push_mono(&b, data.iter().map(|s| f32::from(*s) / 32768.0), channels)
                },
                on_err,
                None,
            )
        }
        SampleFormat::I32 => {
            let b = buf.clone();
            dev.build_input_stream(
                &config,
                move |data: &[i32], _: &_| {
                    push_mono(&b, data.iter().map(|s| *s as f32 / 2_147_483_648.0), channels)
                },
                on_err,
                None,
            )
        }
        other => anyhow::bail!("the microphone speaks {other:?}, which is not handled"),
    }
    .context("cannot open the microphone")?;
    stream.play().context("cannot start the microphone")?;
    let _ = ready.send(Ok(name));

    let mut resampler = Resampler::new(rate, RATE);
    let mut seg = Segmenter::new(tuning);
    let mut pending: Vec<i16> = Vec::with_capacity(FRAME * 4);
    let mut detector = earshot::Detector::default();

    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(10));
        if let Some(e) = err_flag.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = tx.send(MicEvent::Failed(format!("microphone: {e}")));
        }
        let chunk = std::mem::take(&mut *buf.lock().unwrap_or_else(|p| p.into_inner()));
        resampler.push(&chunk, &mut pending);
        let whole = pending.len() / FRAME * FRAME;
        for frame in pending[..whole].chunks_exact(FRAME) {
            let score = detector.predict_i16(frame);
            for ev in seg.feed(frame, score) {
                if tx.send(ev).is_err() {
                    return Ok(());
                }
            }
        }
        pending.drain(..whole);
    }
    drop(stream);
    Ok(())
}

fn push_mono(buf: &Mutex<Vec<f32>>, samples: impl Iterator<Item = f32>, channels: usize) {
    let mut b = buf.lock().unwrap_or_else(|p| p.into_inner());
    // Bounded, in case the processing thread stalls: a few seconds of audio.
    if b.len() > 48_000 * 8 {
        b.clear();
    }
    let mut acc = 0.0;
    let mut n = 0;
    for s in samples {
        acc += s;
        n += 1;
        if n == channels {
            b.push(acc / channels as f32);
            acc = 0.0;
            n = 0;
        }
    }
}

/// A 16 kHz recording cut the way the live microphone would cut it: the same
/// detector, the same segmenter. For the self-test.
pub fn segment(pcm: &[i16], tuning: Tuning) -> Vec<Vec<i16>> {
    let mut detector = earshot::Detector::default();
    let mut seg = Segmenter::new(tuning);
    let mut out = Vec::new();
    for frame in pcm.chunks_exact(FRAME) {
        let score = detector.predict_i16(frame);
        for ev in seg.feed(frame, score) {
            if let MicEvent::Utterance(u) = ev {
                out.push(u);
            }
        }
    }
    out
}

/// Open a microphone for a moment: how many samples a second it delivers and
/// how loud it is, in dBFS. For the self-test.
pub fn probe(device: &str, secs: f32) -> Result<(String, u32, f32)> {
    let host = cpal::default_host();
    let dev = find_input(&host, device).context("no such microphone")?;
    let name = device_name(&dev).unwrap_or_default();
    let supported = dev.default_input_config()?;
    let config = supported.config();
    let channels = usize::from(config.channels.max(1));
    let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let b = buf.clone();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => dev.build_input_stream(
            &config,
            move |d: &[f32], _: &_| push_mono(&b, d.iter().copied(), channels),
            |_| {},
            None,
        ),
        SampleFormat::I16 => dev.build_input_stream(
            &config,
            move |d: &[i16], _: &_| push_mono(&b, d.iter().map(|s| f32::from(*s) / 32768.0), channels),
            |_| {},
            None,
        ),
        other => anyhow::bail!("the microphone speaks {other:?}"),
    }?;
    stream.play()?;
    thread::sleep(Duration::from_secs_f32(secs));
    drop(stream);
    let got = std::mem::take(&mut *buf.lock().unwrap_or_else(|p| p.into_inner()));
    let rms = (got.iter().map(|s| s * s).sum::<f32>() / got.len().max(1) as f32).sqrt();
    let db = 20.0 * rms.max(1e-6).log10();
    Ok((name, (got.len() as f32 / secs) as u32, db))
}

/// A whole recording brought to 16 kHz, for the self-test.
pub fn to_16k(samples: &[f32], rate: u32) -> Vec<i16> {
    let mut r = Resampler::new(rate, RATE);
    let mut out = Vec::with_capacity(samples.len() * RATE as usize / rate.max(1) as usize + 1);
    r.push(samples, &mut out);
    out
}

/// Any rate down (or up) to 16 kHz: two one-pole low-passes against aliasing,
/// then linear interpolation. Speech carries little above 7 kHz, and the
/// recogniser is trained on worse.
struct Resampler {
    step: f64,
    pos: f64,
    prev: f32,
    lp: [f32; 2],
    alpha: f32,
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        let from = from.max(1) as f32;
        let cutoff = 7_000f32.min(from / 2.0 * 0.9);
        let alpha = 1.0 - (-2.0 * std::f32::consts::PI * cutoff / from).exp();
        Self {
            step: f64::from(from) / f64::from(to),
            pos: 0.0,
            prev: 0.0,
            lp: [0.0; 2],
            alpha: if from as u32 > to { alpha } else { 1.0 },
        }
    }

    fn push(&mut self, input: &[f32], out: &mut Vec<i16>) {
        for &x in input {
            self.lp[0] += self.alpha * (x - self.lp[0]);
            self.lp[1] += self.alpha * (self.lp[0] - self.lp[1]);
            let cur = self.lp[1];
            // Emit every output sample that falls between prev and cur.
            while self.pos < 1.0 {
                let t = self.pos as f32;
                let v = self.prev + (cur - self.prev) * t;
                out.push((v.clamp(-1.0, 1.0) * 32767.0) as i16);
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.prev = cur;
        }
    }
}

/// Voiced frames in, utterances out.
struct Segmenter {
    tuning: Tuning,
    pre_roll: VecDeque<Vec<i16>>,
    window: VecDeque<bool>,
    speaking: bool,
    utterance: Vec<i16>,
    voiced_frames: u64,
    silent_run: u64,
    /// Frames since the utterance was last handed over unfinished.
    since_so_far: u64,
}

impl Segmenter {
    fn new(tuning: Tuning) -> Self {
        Self {
            tuning,
            pre_roll: VecDeque::with_capacity(PRE_ROLL_FRAMES + 1),
            window: VecDeque::with_capacity(START_WINDOW + 1),
            speaking: false,
            utterance: Vec::new(),
            voiced_frames: 0,
            silent_run: 0,
            since_so_far: 0,
        }
    }

    fn feed(&mut self, frame: &[i16], score: f32) -> Vec<MicEvent> {
        let voiced = score >= self.tuning.threshold;
        let mut out = Vec::new();
        if !self.speaking {
            self.pre_roll.push_back(frame.to_vec());
            if self.pre_roll.len() > PRE_ROLL_FRAMES {
                self.pre_roll.pop_front();
            }
            self.window.push_back(voiced);
            if self.window.len() > START_WINDOW {
                self.window.pop_front();
            }
            // Most of a quarter second voiced: a person, not a click.
            let voiced_in_window = self.window.iter().filter(|v| **v).count();
            if voiced_in_window * 10 >= START_WINDOW * 6 {
                self.speaking = true;
                self.utterance = self.pre_roll.drain(..).flatten().collect();
                self.voiced_frames = voiced_in_window as u64;
                self.silent_run = 0;
                self.since_so_far = 0;
                self.window.clear();
                out.push(MicEvent::SpeechStart);
            }
            return out;
        }

        self.utterance.extend_from_slice(frame);
        if voiced {
            self.voiced_frames += 1;
            self.silent_run = 0;
        } else {
            self.silent_run += 1;
        }
        let len_ms = self.utterance.len() as u64 * 1000 / u64::from(RATE);
        let ended = self.silent_run * FRAME_MS >= self.tuning.end_silence_ms;
        if ended || len_ms >= MAX_UTTERANCE_MS {
            // Most of the trailing silence goes; a little stays so the last
            // word is not cut against the edge.
            let keep_tail = (self.silent_run as usize).saturating_sub(12) * FRAME;
            let cut = self.utterance.len().saturating_sub(keep_tail);
            self.utterance.truncate(cut);
            let pcm = std::mem::take(&mut self.utterance);
            let enough = self.voiced_frames * FRAME_MS >= self.tuning.min_speech_ms;
            out.push(if enough {
                MicEvent::Utterance(pcm)
            } else {
                MicEvent::Discarded
            });
            self.speaking = false;
            self.voiced_frames = 0;
            self.silent_run = 0;
            return out;
        }
        self.since_so_far += 1;
        // Only on a voiced frame: in a pause there is nothing new to show.
        if voiced && self.since_so_far >= SO_FAR_FRAMES {
            self.since_so_far = 0;
            out.push(MicEvent::SoFar(self.utterance.clone()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tuning() -> Tuning {
        Tuning {
            threshold: 0.5,
            min_speech_ms: 250,
            end_silence_ms: 480,
        }
    }

    fn run(seg: &mut Segmenter, voiced: bool, frames: usize) -> Vec<&'static str> {
        let frame = vec![0i16; FRAME];
        let mut seen = Vec::new();
        for _ in 0..frames {
            for ev in seg.feed(&frame, if voiced { 0.9 } else { 0.1 }) {
                seen.push(match ev {
                    MicEvent::SpeechStart => "start",
                    MicEvent::SoFar(_) => "so far",
                    MicEvent::Utterance(_) => "utterance",
                    MicEvent::Discarded => "discarded",
                    MicEvent::Failed(_) => "failed",
                });
            }
        }
        seen
    }

    #[test]
    fn speech_then_silence_is_one_utterance() {
        let mut seg = Segmenter::new(tuning());
        assert!(run(&mut seg, false, 50).is_empty());
        assert_eq!(run(&mut seg, true, 60), vec!["start"]);
        // 480 ms of silence is 30 frames.
        assert!(run(&mut seg, false, 29).is_empty());
        assert_eq!(run(&mut seg, false, 1), vec!["utterance"]);
    }

    #[test]
    fn a_long_utterance_is_handed_over_as_it_goes() {
        let mut seg = Segmenter::new(tuning());
        assert_eq!(run(&mut seg, true, 10), vec!["start"]);
        assert!(run(&mut seg, true, 59).is_empty());
        assert_eq!(run(&mut seg, true, 1), vec!["so far"]);
        // A pause adds nothing worth showing; the next word does.
        assert!(run(&mut seg, false, 20).is_empty());
        assert!(run(&mut seg, true, 39).is_empty());
        assert_eq!(run(&mut seg, true, 1), vec!["so far"]);
        assert_eq!(run(&mut seg, false, 30), vec!["utterance"]);
    }

    #[test]
    fn a_click_does_not_open_an_utterance() {
        let mut seg = Segmenter::new(tuning());
        assert!(run(&mut seg, true, 3).is_empty());
        assert!(run(&mut seg, false, 40).is_empty());
    }

    #[test]
    fn a_short_grunt_is_heard_but_not_sent() {
        let mut seg = Segmenter::new(Tuning {
            min_speech_ms: 1000,
            ..tuning()
        });
        assert_eq!(run(&mut seg, true, 12), vec!["start"]);
        assert_eq!(run(&mut seg, false, 30), vec!["discarded"]);
    }

    #[test]
    fn resampling_48k_gives_a_third_of_the_samples() {
        let mut r = Resampler::new(48_000, 16_000);
        let mut out = Vec::new();
        r.push(&vec![0.0; 4800], &mut out);
        assert!((1599..=1601).contains(&out.len()), "{}", out.len());
    }
}
