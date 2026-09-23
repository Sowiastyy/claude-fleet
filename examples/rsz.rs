use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize { rows: 20, cols: 100, pixel_width: 0, pixel_height: 0 }).unwrap();
    let mut cmd = CommandBuilder::new(&args[0]);
    for a in &args[1..] { cmd.arg(a); }
    cmd.cwd(std::env::current_dir().unwrap());
    let _child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
    let log: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let parser = Arc::new(Mutex::new(vt100::Parser::new(20, 100, 1000)));
    { let log = log.clone(); let parser = parser.clone(); let w = writer.clone();
      std::thread::spawn(move || { let mut b = [0u8; 8192]; loop { let n = match reader.read(&mut b) { Ok(0)|Err(_) => break, Ok(n) => n };
        let mut p = parser.lock().unwrap(); p.process(&b[..n]);
        if b[..n].windows(4).any(|x| x == b"\x1b[6n") { let (r,c) = p.screen().cursor_position(); let _ = w.lock().unwrap().write_all(format!("\x1b[{};{}R", r+1, c+1).as_bytes()); }
        log.lock().unwrap().extend_from_slice(&b[..n]); } }); }
    let dump = |tag: &str| {
        std::thread::sleep(Duration::from_secs(8));
        let bytes = std::mem::take(&mut *log.lock().unwrap());
        println!("===== {tag}: {} bytes raw:\n{:?}", bytes.len(), String::from_utf8_lossy(&bytes[..bytes.len().min(1500)]));
        println!("----- screen:\n{}", parser.lock().unwrap().screen().contents());
    };
    dump("start");
    pair.master.resize(PtySize { rows: 20, cols: 40, pixel_width: 0, pixel_height: 0 }).unwrap();
    parser.lock().unwrap().screen_mut().set_size(20, 40);
    dump("narrow 40");
    for c in (45..=100).step_by(5) {
        pair.master.resize(PtySize { rows: 20, cols: c, pixel_width: 0, pixel_height: 0 }).unwrap();
        parser.lock().unwrap().screen_mut().set_size(20, c);
        std::thread::sleep(Duration::from_millis(30));
    }
    dump("wide 100");
    std::process::exit(0);
}
