//! Where the voice engines live, how they get there, and a way to check them.
//!
//! Both engines are downloaded, not built: whisper.cpp's CUDA build of
//! `whisper-server` with a quantised large-v3-turbo model for listening, and
//! Piper with one voice for speaking. They sit in one directory the fleet
//! owns, `%LOCALAPPDATA%\claude-fleet\voice` unless the config says otherwise.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use super::{mic, stt, tts};
use crate::config::{self, VoiceCfg};

/// The whisper.cpp build the setup downloads. Pinned, so a setup made today
/// and one made next month install the same thing.
const WHISPER_TAG: &str = "b5130";
const WHISPER_CUDA_ZIP: &str = "whisper-cublas-12.4.0-bin-x64.zip";
const WHISPER_CPU_ZIP: &str = "whisper-bin-x64.zip";
const PIPER_URL: &str =
    "https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_windows_amd64.zip";
const MODELS_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";
const VOICES_URL: &str = "https://huggingface.co/rhasspy/piper-voices/resolve/main";

/// Recognition models in the order `whisper_model = ""` picks them: the best
/// one present wins.
const MODEL_PREFERENCE: &[&str] = &[
    "ggml-large-v3-turbo-q5_0.bin",
    "ggml-large-v3-turbo-q8_0.bin",
    "ggml-large-v3-turbo.bin",
    "ggml-medium-q5_0.bin",
    "ggml-medium.bin",
    "ggml-small-q8_0.bin",
    "ggml-small.bin",
];

pub struct Paths {
    pub root: PathBuf,
    pub whisper_server: PathBuf,
    pub whisper_model: PathBuf,
    pub piper: PathBuf,
    pub voice_model: PathBuf,
}

pub fn root(cfg: &VoiceCfg) -> PathBuf {
    if !cfg.dir.trim().is_empty() {
        return PathBuf::from(cfg.dir.trim());
    }
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("claude-fleet")
        .join("voice")
}

fn first_existing(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|p| p.is_file())
}

/// Every engine file, or what is missing and how to get it.
pub fn locate(cfg: &VoiceCfg) -> Result<Paths, String> {
    let root = root(cfg);
    let exe = |name: &str| format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let whisper_server = first_existing([
        root.join("whisper").join("Release").join(exe("whisper-server")),
        root.join("whisper").join(exe("whisper-server")),
    ]);
    let whisper_model = if cfg.whisper_model.trim().is_empty() {
        first_existing(MODEL_PREFERENCE.iter().map(|m| root.join("models").join(m)))
    } else {
        first_existing([
            root.join("models").join(cfg.whisper_model.trim()),
            PathBuf::from(cfg.whisper_model.trim()),
        ])
    };
    let piper = first_existing([
        root.join("piper").join("piper").join(exe("piper")),
        root.join("piper").join(exe("piper")),
    ]);
    let voice = cfg.voice.trim().trim_end_matches(".onnx");
    let voice_model = first_existing([
        root.join("voices").join(format!("{voice}.onnx")),
        PathBuf::from(format!("{voice}.onnx")),
    ]);

    let mut missing = Vec::new();
    if whisper_server.is_none() {
        missing.push("whisper-server");
    }
    if whisper_model.is_none() {
        missing.push("a whisper model");
    }
    if piper.is_none() {
        missing.push("piper");
    }
    if voice_model.is_none() {
        missing.push("the piper voice");
    }
    if !missing.is_empty() {
        return Err(format!(
            "voice engines missing in {}: {} — run `claude-fleet voice setup`",
            root.display(),
            missing.join(", ")
        ));
    }
    Ok(Paths {
        root,
        whisper_server: whisper_server.expect("checked"),
        whisper_model: whisper_model.expect("checked"),
        piper: piper.expect("checked"),
        voice_model: voice_model.expect("checked"),
    })
}

/// Threads for the recogniser's CPU side. With a GPU it barely matters; on
/// the CPU alone half the cores keeps the rest of the machine usable.
pub fn whisper_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| (n.get() / 2).clamp(2, 8))
        .unwrap_or(4)
}

fn has_nvidia() -> bool {
    Command::new("nvidia-smi")
        .arg("-L")
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
}

fn download(url: &str, to: &Path) -> Result<()> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = to.with_extension("part");
    println!("  downloading {url}");
    let ok = Command::new("curl")
        .args(["-L", "--fail", "-#", "-o"])
        .arg(&part)
        .arg(url)
        .status()
        .context("curl is needed to download the engines")?
        .success();
    if !ok {
        let _ = std::fs::remove_file(&part);
        bail!("download failed: {url}");
    }
    std::fs::rename(&part, to)?;
    Ok(())
}

fn unzip(zip: &Path, into: &Path) -> Result<()> {
    std::fs::create_dir_all(into)?;
    // Windows' own tar is bsdtar and reads zips; a Git Bash tar on PATH is
    // GNU tar and does not, so the system one is named outright.
    let tar = if cfg!(windows) {
        std::env::var("SystemRoot")
            .map(|r| PathBuf::from(r).join("System32").join("tar.exe"))
            .unwrap_or_else(|_| PathBuf::from("tar"))
    } else {
        PathBuf::from("tar")
    };
    let ok = Command::new(tar)
        .arg("-xf")
        .arg(zip)
        .arg("-C")
        .arg(into)
        .status()
        .context("cannot run tar to unpack")?
        .success();
    if !ok {
        bail!("cannot unpack {}", zip.display());
    }
    let _ = std::fs::remove_file(zip);
    Ok(())
}

/// `claude-fleet voice setup [--cpu]`: fetch whatever is not there yet.
pub fn setup(args: &[String]) -> Result<()> {
    let cfg = config::voice();
    let root = root(&cfg);
    let cpu = args.iter().any(|a| a == "--cpu") || !has_nvidia();
    println!("voice engines go to {}", root.display());
    std::fs::create_dir_all(&root)?;

    let server = root.join("whisper").join("Release").join("whisper-server.exe");
    if server.is_file() {
        println!("- whisper-server: already there");
    } else {
        let zip_name = if cpu { WHISPER_CPU_ZIP } else { WHISPER_CUDA_ZIP };
        println!(
            "- whisper-server ({}):",
            if cpu { "CPU build" } else { "CUDA build, for the NVIDIA GPU" }
        );
        let zip = root.join(zip_name);
        download(
            &format!("https://github.com/ggml-org/whisper.cpp/releases/download/{WHISPER_TAG}/{zip_name}"),
            &zip,
        )?;
        unzip(&zip, &root.join("whisper"))?;
    }

    let model = if !cfg.whisper_model.trim().is_empty() {
        cfg.whisper_model.trim().to_string()
    } else if cpu {
        // Turbo on a CPU takes seconds per sentence; small keeps up.
        "ggml-small-q8_0.bin".to_string()
    } else {
        MODEL_PREFERENCE[0].to_string()
    };
    let model_path = root.join("models").join(&model);
    if model_path.is_file() {
        println!("- whisper model {model}: already there");
    } else {
        println!("- whisper model {model}:");
        download(&format!("{MODELS_URL}/{model}"), &model_path)?;
    }

    let piper = root.join("piper").join("piper").join("piper.exe");
    if piper.is_file() {
        println!("- piper: already there");
    } else {
        println!("- piper:");
        let zip = root.join("piper.zip");
        download(PIPER_URL, &zip)?;
        unzip(&zip, &root.join("piper"))?;
    }

    let voice = cfg.voice.trim().trim_end_matches(".onnx").to_string();
    let onnx = root.join("voices").join(format!("{voice}.onnx"));
    if onnx.is_file() {
        println!("- voice {voice}: already there");
    } else {
        // pl_PL-gosia-medium lives at pl/pl_PL/gosia/medium/.
        let parts: Vec<&str> = voice.split('-').collect();
        let [lang_region, name, quality] = parts[..] else {
            bail!("a piper voice is named like pl_PL-gosia-medium, not \"{voice}\"");
        };
        let lang = lang_region.split('_').next().unwrap_or(lang_region);
        let base = format!("{VOICES_URL}/{lang}/{lang_region}/{name}/{quality}/{voice}");
        println!("- voice {voice}:");
        download(&format!("{base}.onnx"), &onnx)?;
        download(
            &format!("{base}.onnx.json"),
            &root.join("voices").join(format!("{voice}.onnx.json")),
        )?;
    }

    match locate(&cfg) {
        Ok(_) => println!("\nready. In the fleet, `v` (or Alt+Shift+V anywhere) turns voice on."),
        Err(e) => println!("\n{e}"),
    }
    Ok(())
}

/// `claude-fleet voice devices`: what can be named in `input_device` and
/// `output_device`.
pub fn devices() -> Result<()> {
    println!("microphones (input_device):");
    for n in mic::input_names() {
        println!("  {n}");
    }
    println!("outputs (output_device):");
    for n in tts::output_names() {
        println!("  {n}");
    }
    println!("\nBluetooth headphones switch to low-quality \"hands-free\" audio while their");
    println!("microphone is open. The laptop's own microphone avoids that.");
    Ok(())
}

/// `claude-fleet voice test [--mic]`: every engine, one after another, with
/// timings — first without the microphone (a sentence spoken by piper is
/// recognised back), then, with `--mic`, twenty seconds of listening.
pub fn test(args: &[String]) -> Result<()> {
    let cfg = config::voice();
    let paths = locate(&cfg).map_err(anyhow::Error::msg)?;
    println!("whisper-server  {}", paths.whisper_server.display());
    println!("model           {}", paths.whisper_model.display());
    println!("piper           {}", paths.piper.display());
    println!("voice           {}", paths.voice_model.display());

    let scratch = std::env::temp_dir()
        .join("claude-fleet")
        .join(format!("voice-test-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let sentence = if cfg.language.starts_with("pl") {
        "Sprawdź, czy funkcja spawn session w pliku app.rs dobrze obsługuje błędy."
    } else {
        "Check whether the spawn session function in app.rs handles errors well."
    };

    print!("\n1. piper renders a sentence ... ");
    let t = Instant::now();
    let wav_path = scratch.join("test.wav");
    let mut cmd = Command::new(&paths.piper);
    cmd.arg("--model")
        .arg(&paths.voice_model)
        .arg("--output_file")
        .arg(&wav_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = cmd.spawn().context("cannot start piper")?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().context("piper has no stdin")?;
        writeln!(stdin, "{sentence}")?;
    }
    child.wait()?;
    let (rate, samples) = read_wav_mono(&wav_path)?;
    println!(
        "{:.1}s of audio in {} ms",
        samples.len() as f32 / rate as f32,
        t.elapsed().as_millis()
    );

    print!("2. whisper-server loads the model ... ");
    let t = Instant::now();
    let whisper = stt::Whisper::start(
        &paths.whisper_server,
        &paths.whisper_model,
        &cfg.language,
        whisper_threads(),
        &paths.root.join("whisper-server.log"),
    )?;
    println!("{} ms", t.elapsed().as_millis());

    print!("3. the voice detector cuts it out of silence ... ");
    // A second of silence either side, as a pause in a room would give.
    let mut pcm = vec![0i16; mic::RATE as usize];
    pcm.extend(mic::to_16k(&samples, rate));
    pcm.extend(vec![0i16; mic::RATE as usize * 2]);
    let pieces = mic::segment(&pcm, super::tuning(&cfg));
    let lens: Vec<String> = pieces
        .iter()
        .map(|p| format!("{:.1}s", p.len() as f32 / mic::RATE as f32))
        .collect();
    println!("{} utterance(s): {}", pieces.len(), lens.join(", "));

    print!("4. and whisper hears it back ... ");
    let t = Instant::now();
    let heard: Vec<String> = pieces
        .iter()
        .map(|p| whisper.transcribe(p, &super::vocabulary(&[], &cfg)))
        .collect::<Result<_>>()?;
    println!(
        "{} ms\n   said:  {sentence}\n   heard: {}",
        t.elapsed().as_millis(),
        heard.join(" / ")
    );

    print!("5. the voice, out loud ... ");
    let t = Instant::now();
    let speaker = tts::Tts::start(tts::Settings {
        piper: paths.piper.clone(),
        model: paths.voice_model.clone(),
        speed: cfg.speed,
        device: cfg.output_device.clone(),
        scratch: scratch.join("tts"),
    })?;
    speaker.say(sentence);
    std::thread::sleep(Duration::from_millis(300));
    let until = Instant::now() + Duration::from_secs(20);
    while speaker.is_speaking() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    match speaker.failed.lock().ok().and_then(|g| g.clone()) {
        Some(e) => println!("failed: {e}"),
        None => println!("played for {:.1}s", t.elapsed().as_secs_f32()),
    }

    if cfg.sounds {
        println!("6. the pips: you are heard, it is sent, Claude is working");
        for cue in [tts::Cue::Hearing, tts::Cue::Sent, tts::Cue::Working] {
            speaker.cue(cue, cfg.sound_volume.clamp(0.0, 1.0));
            std::thread::sleep(Duration::from_millis(700));
        }
    } else {
        println!("6. the pips are off (sounds = false)");
    }

    print!("7. the microphone opens ... ");
    match mic::probe(&cfg.input_device, 1.5) {
        Ok((name, rate, db)) => println!("{name}: {rate} samples/s, level {db:.0} dBFS"),
        Err(e) => println!("failed: {e:#}"),
    }

    if args.iter().any(|a| a == "--mic") {
        let (tx, rx) = mpsc::channel();
        let m = mic::Mic::start(&cfg.input_device, super::tuning(&cfg), tx)?;
        println!("8. listening on \"{}\" for 20 s — say something", m.name);
        let until = Instant::now() + Duration::from_secs(20);
        while Instant::now() < until {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(mic::MicEvent::SpeechStart) => println!("   (speech)"),
                Ok(mic::MicEvent::SoFar(_)) => {}
                Ok(mic::MicEvent::Discarded) => println!("   (too short, dropped)"),
                Ok(mic::MicEvent::Failed(e)) => bail!("{e}"),
                Ok(mic::MicEvent::Utterance(pcm)) => {
                    let t = Instant::now();
                    let text = whisper.transcribe(&pcm, &super::vocabulary(&[], &cfg))?;
                    println!(
                        "   {:.1}s heard in {} ms: {text}",
                        pcm.len() as f32 / mic::RATE as f32,
                        t.elapsed().as_millis()
                    );
                }
                Err(_) => {}
            }
        }
    } else {
        println!("\n(`claude-fleet voice test --mic` also listens to the microphone for 20 s)");
    }
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(())
}

/// A mono 16-bit WAV as floats, with its rate.
fn read_wav_mono(path: &Path) -> Result<(u32, Vec<f32>)> {
    let bytes = std::fs::read(path)?;
    let data_at = bytes
        .windows(4)
        .position(|w| w == b"data")
        .context("not a WAV file")?;
    let rate = u32::from_le_bytes(bytes[24..28].try_into()?);
    let data = &bytes[data_at + 8..];
    Ok((
        rate,
        data.chunks_exact(2)
            .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
            .collect(),
    ))
}
