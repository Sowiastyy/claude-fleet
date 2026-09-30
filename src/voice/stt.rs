//! Speech to text: `whisper-server` from whisper.cpp, run by the fleet.
//!
//! The model is loaded once and stays on the GPU; each utterance goes to the
//! server over HTTP on the loopback interface, as a WAV in a multipart form,
//! and its text comes back as JSON. Nothing leaves the machine.

use std::{
    fs::File,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::job;

/// How long the model may take to load before the start counts as failed.
/// The first start after boot compiles nothing but does load half a
/// gigabyte from disk and set up CUDA, which is seconds, not minutes.
const START_LIMIT: Duration = Duration::from_secs(90);

pub struct Whisper {
    child: Child,
    port: u16,
}

impl Whisper {
    /// Start the server and wait until it takes connections, which is when
    /// the model is loaded.
    pub fn start(exe: &Path, model: &Path, lang: &str, threads: usize, log: &Path) -> Result<Self> {
        let port = free_port()?;
        let log_file = File::create(log).with_context(|| format!("cannot write {}", log.display()))?;
        let mut cmd = Command::new(exe);
        cmd.arg("-m")
            .arg(model)
            .args(["-l", lang, "--host", "127.0.0.1", "--port"])
            .arg(port.to_string())
            .args(["-t", &threads.to_string()])
            // No timestamps in the text, and no "[music]" style tokens.
            .args(["-nt", "-sns"])
            .current_dir(exe.parent().unwrap_or(Path::new(".")))
            .stdin(Stdio::null())
            .stdout(log_file.try_clone()?)
            .stderr(log_file);
        hide_window(&mut cmd);
        let child = cmd
            .spawn()
            .with_context(|| format!("cannot start {}", exe.display()))?;
        job::adopt(&child);
        let mut me = Self { child, port };

        let started = Instant::now();
        loop {
            if let Some(status) = me.child.try_wait()? {
                bail!(
                    "whisper-server exited ({status}) while loading — see {}",
                    log.display()
                );
            }
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
                return Ok(me);
            }
            if started.elapsed() > START_LIMIT {
                bail!("whisper-server did not come up in {}s", START_LIMIT.as_secs());
            }
            thread::sleep(Duration::from_millis(150));
        }
    }

    /// The text of 16 kHz mono audio. `prompt` is vocabulary the recogniser
    /// is nudged towards: session names, file names, words of the trade.
    pub fn transcribe(&self, pcm: &[i16], prompt: &str) -> Result<String> {
        let boundary = "----fleetvoice7MA4YWxkTrZu0gW";
        let mut body = Vec::with_capacity(pcm.len() * 2 + 1024);
        let mut field = |name: &str, value: &str| {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                )
                .as_bytes(),
            );
        };
        field("response_format", "json");
        field("temperature", "0.0");
        if !prompt.is_empty() {
            field("prompt", prompt);
        }
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"speech.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(&wav(pcm, 16_000));
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
            .context("whisper-server is not answering")?;
        stream.set_read_timeout(Some(Duration::from_secs(60)))?;
        let head = format!(
            "POST /inference HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: multipart/form-data; boundary={boundary}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.port,
            body.len()
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(&body)?;
        stream.flush()?;

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw)?;
        let body = http_body(&raw)?;
        let v: Value = serde_json::from_slice(&body).context("whisper-server sent something unreadable")?;
        if let Some(e) = v["error"].as_str() {
            bail!("whisper-server: {e}");
        }
        Ok(v["text"]
            .as_str()
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "))
    }
}

impl Drop for Whisper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A recogniser on a thread of its own: utterances go in, text comes out, and
/// the UI thread never waits on the GPU.
pub struct Worker {
    tx: Sender<(Vec<i16>, String)>,
    pub rx: Receiver<Result<String, String>>,
}

impl Worker {
    pub fn spawn(whisper: Whisper) -> Self {
        let (tx, jobs) = mpsc::channel::<(Vec<i16>, String)>();
        let (done, rx) = mpsc::channel();
        thread::spawn(move || {
            // The server lives as long as this loop: dropping the worker
            // closes the channel, ends the loop and kills the process.
            for (pcm, prompt) in jobs {
                let r = whisper.transcribe(&pcm, &prompt).map_err(|e| format!("{e:#}"));
                if done.send(r).is_err() {
                    break;
                }
            }
        });
        Self { tx, rx }
    }

    pub fn submit(&self, pcm: Vec<i16>, prompt: String) {
        let _ = self.tx.send((pcm, prompt));
    }
}

/// A port nobody is listening on right now.
fn free_port() -> Result<u16> {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    Ok(l.local_addr()?.port())
}

/// 16-bit mono PCM wrapped in a WAV header.
pub fn wav(pcm: &[i16], rate: u32) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// The body of a raw HTTP/1.1 response, with chunked encoding undone.
fn http_body(raw: &[u8]) -> Result<Vec<u8>> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("whisper-server closed the connection without an answer")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    let body = &raw[split + 4..];
    let status = head.split_whitespace().nth(1).unwrap_or("?");
    if status != "200" {
        bail!(
            "whisper-server answered {status}: {}",
            String::from_utf8_lossy(body).trim()
        );
    }
    if !head.contains("transfer-encoding: chunked") {
        return Ok(body.to_vec());
    }
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(eol) = rest.windows(2).position(|w| w == b"\r\n") {
        let size = std::str::from_utf8(&rest[..eol])
            .ok()
            .and_then(|s| usize::from_str_radix(s.split(';').next()?.trim(), 16).ok())
            .context("a chunk size that is not one")?;
        if size == 0 {
            break;
        }
        let start = eol + 2;
        let end = (start + size).min(rest.len());
        out.extend_from_slice(&rest[start..end]);
        rest = rest.get(end + 2..).unwrap_or_default();
    }
    Ok(out)
}

pub fn hide_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wav_header_describes_its_samples() {
        let w = wav(&[0, 1, -1], 16_000);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 6);
    }

    #[test]
    fn chunked_and_plain_bodies_read_the_same() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\n{\"text\":\"a\"}\n";
        assert_eq!(http_body(plain).unwrap(), b"{\"text\":\"a\"}\n");
        let chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\n{\"text\r\n6\r\n\":\"a\"}\r\n0\r\n\r\n";
        assert_eq!(http_body(chunked).unwrap(), b"{\"text\":\"a\"}");
        let bad = b"HTTP/1.1 500 Internal\r\n\r\nboom";
        assert!(http_body(bad).is_err());
    }
}
