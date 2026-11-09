//! Terminal input, read by a thread of its own.
//!
//! The console's input queue is a fixed-size ring: records that land in it
//! while nobody is reading are dropped, silently and mid-word. The event loop
//! only got to read between renders, so a paste — which arrives as a flood of
//! key records, since crossterm produces `Event::Paste` on unix only — lost
//! whole runs of characters to every redraw it overlapped.
//!
//! So reading happens here instead, in a thread that does nothing else and sits
//! blocked in `read` the rest of the time. Rendering can take as long as it
//! likes; the queue is still being drained.

use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::Duration,
};

use crossterm::event::{Event, KeyCode, KeyEventKind};

pub struct Input {
    rx: Receiver<Event>,
}

impl Input {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut source = Source::open();
            let mut taken = TakenPaste::default();
            while let Ok(ev) = source.read() {
                if tx.send(taken.restore(ev)).is_err() {
                    break;
                }
            }
        });
        Self { rx }
    }

    /// The next event, or `None` if none arrived within `timeout`.
    pub fn next_within(&self, timeout: Duration) -> Option<Event> {
        match self.rx.recv_timeout(timeout) {
            Ok(ev) => Some(ev),
            Err(RecvTimeoutError::Timeout) => None,
            // The reader is gone, so no event is ever coming. Wait out the
            // timeout anyway: returning at once would spin the event loop.
            Err(RecvTimeoutError::Disconnected) => {
                thread::sleep(timeout);
                None
            }
        }
    }
}

/// Where events come from. Off Windows, crossterm and nothing else.
#[cfg(not(windows))]
struct Source;

#[cfg(not(windows))]
impl Source {
    fn open() -> Self {
        Self
    }

    fn read(&mut self) -> std::io::Result<Event> {
        crossterm::event::read()
    }
}

#[cfg(windows)]
use console::Source;

/// The characters crossterm loses on Windows.
///
/// A character past U+FFFF — every emoji — is two UTF-16 units, and a terminal
/// that reports keys as key events (Windows Terminal does) hands it over as two
/// key records, each pressed and released. crossterm pairs units in the order
/// they come without asking which record is a press: the first unit's press
/// meets its own release, the pair is invalid and both are dropped, and so is
/// the second. The emoji, typed or pasted, never arrives. Sent as both presses
/// and then both releases, it arrives twice.
///
/// So those records come off the queue before crossterm sees them, and the
/// presses are paired here. The console's own Alt+numpad form of the same
/// character, the unit riding on the release of Alt, crossterm reads correctly
/// and keeps.
#[cfg(windows)]
mod console {
    use std::{fs::File, io, os::windows::io::AsRawHandle, time::Duration};

    use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

    type Handle = *mut std::ffi::c_void;

    /// `INPUT_RECORD`, read as the `KEY_EVENT_RECORD` it holds when `kind`
    /// says so.
    #[repr(C)]
    #[derive(Default)]
    struct Record {
        kind: u16,
        key_down: i32,
        repeat: u16,
        vk: u16,
        scan: u16,
        unit: u16,
        state: u32,
    }

    const _: () = assert!(std::mem::size_of::<Record>() == 20);

    const INFINITE: u32 = u32::MAX;
    const WAIT_OBJECT_0: u32 = 0;

    unsafe extern "system" {
        fn WaitForSingleObject(handle: Handle, millis: u32) -> u32;
        fn PeekConsoleInputW(input: Handle, buf: *mut Record, len: u32, read: *mut u32) -> i32;
        fn ReadConsoleInputW(input: Handle, buf: *mut Record, len: u32, read: *mut u32) -> i32;
    }

    impl Record {
        /// Half of a character crossterm would lose.
        fn is_half(&self) -> bool {
            const KEY_EVENT: u16 = 1;
            const VK_MENU: u16 = 0x12;
            let alt_code = self.vk == VK_MENU && self.key_down == 0;
            self.kind == KEY_EVENT && (0xD800..0xE000).contains(&self.unit) && !alt_code
        }

        fn modifiers(&self) -> KeyModifiers {
            let mut mods = KeyModifiers::NONE;
            if self.state & 0x10 != 0 {
                mods |= KeyModifiers::SHIFT;
            }
            if self.state & 0x0c != 0 {
                mods |= KeyModifiers::CONTROL;
            }
            if self.state & 0x03 != 0 {
                mods |= KeyModifiers::ALT;
            }
            mods
        }
    }

    /// Puts a character back together from the presses of its two units.
    #[derive(Default)]
    struct Pair {
        high: Option<u16>,
    }

    impl Pair {
        fn feed(&mut self, unit: u16) -> Option<char> {
            if (0xD800..0xDC00).contains(&unit) {
                // A high unit with no low one after it is lost either way;
                // the newer one is the one with a chance.
                self.high = Some(unit);
                return None;
            }
            char::decode_utf16([self.high.take()?, unit]).next()?.ok()
        }
    }

    pub struct Source {
        /// The console input crossterm reads, whatever stdin is. `None` once
        /// looking at it has failed: crossterm on its own is what is left.
        conin: Option<File>,
        pair: Pair,
    }

    impl Source {
        pub fn open() -> Self {
            let conin = File::options().read(true).write(true).open("CONIN$").ok();
            Self {
                conin,
                pair: Pair::default(),
            }
        }

        pub fn read(&mut self) -> io::Result<Event> {
            let Some(conin) = &self.conin else {
                return event::read();
            };
            let handle = conin.as_raw_handle();
            loop {
                let mut rec = Record::default();
                let mut n = 0;
                // SAFETY: a console input handle that lives as long as
                // `self.conin`, and room for exactly the one record asked for.
                let peeked = unsafe {
                    WaitForSingleObject(handle, INFINITE) == WAIT_OBJECT_0
                        && PeekConsoleInputW(handle, &mut rec, 1, &mut n) != 0
                };
                if !peeked {
                    self.conin = None;
                    return event::read();
                }
                if n == 1 && rec.is_half() {
                    // SAFETY: as above. Nothing else reads the console, so
                    // this takes the record just peeked.
                    unsafe { ReadConsoleInputW(handle, &mut rec, 1, &mut n) };
                    if rec.key_down != 0
                        && let Some(c) = self.pair.feed(rec.unit)
                    {
                        let key = KeyEvent::new(KeyCode::Char(c), rec.modifiers());
                        return Ok(Event::Key(key));
                    }
                    continue;
                }
                // One record, not a read: left to block, crossterm carries on
                // past a record that makes no event, and the next one could be
                // half a character.
                if event::poll(Duration::ZERO)? {
                    return event::read();
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn key(down: bool, vk: u16, unit: u16) -> Record {
            Record {
                kind: 1,
                key_down: down.into(),
                vk,
                unit,
                ..Record::default()
            }
        }

        #[test]
        fn two_presses_make_one_emoji() {
            let mut pair = Pair::default();
            assert_eq!(pair.feed(0xD83D), None);
            assert_eq!(pair.feed(0xDE00), Some('\u{1F600}'));
        }

        #[test]
        fn a_low_unit_on_its_own_is_nothing() {
            let mut pair = Pair::default();
            assert_eq!(pair.feed(0xDE00), None);
            assert_eq!(pair.feed(0xD83D), None);
            assert_eq!(pair.feed(0xDE00), Some('\u{1F600}'));
        }

        #[test]
        fn an_orphaned_high_unit_gives_way() {
            let mut pair = Pair::default();
            pair.feed(0xD83C);
            pair.feed(0xD83D);
            assert_eq!(pair.feed(0xDE00), Some('\u{1F600}'));
        }

        #[test]
        fn both_halves_are_taken_pressed_or_released() {
            assert!(key(true, 0, 0xD83D).is_half());
            assert!(key(false, 0, 0xDE00).is_half());
        }

        #[test]
        fn the_alt_numpad_form_is_left_to_crossterm() {
            assert!(!key(false, 0x12, 0xD83D).is_half());
        }

        #[test]
        fn a_plain_letter_is_left_to_crossterm() {
            assert!(!key(true, 0x41, u16::from(b'a')).is_half());
        }
    }
}

/// Notices a paste the terminal kept to itself.
///
/// Windows Terminal binds Ctrl+V to its own paste: the press never becomes a
/// key record, only the pasted text does. A clipboard holding nothing but an
/// image has no text, so the terminal sends an empty bracketed paste — its way
/// of saying the user pasted — and conhost drops the brackets on the way to an
/// application that reads key records. All that still arrives is the release
/// of the `v`, which the binding leaves alone.
///
/// So a `v` released without having been pressed is that paste, and goes on as
/// the empty `Event::Paste` a terminal that delivers one would have sent. It
/// follows a paste of text just the same; whoever reads it tells the two apart
/// by what the clipboard holds.
#[derive(Default)]
struct TakenPaste {
    /// A press of `v` has been read and its release has not.
    down: bool,
}

impl TakenPaste {
    fn restore(&mut self, ev: Event) -> Event {
        let Event::Key(key) = &ev else {
            return ev;
        };
        // Ctrl may be let go first, and Shift belongs to the other paste
        // chord, so the modifiers say nothing; the letter does.
        if !matches!(key.code, KeyCode::Char('v' | 'V')) {
            return ev;
        }
        if key.kind != KeyEventKind::Release {
            self.down = true;
            return ev;
        }
        if std::mem::take(&mut self.down) {
            return ev;
        }
        Event::Paste(String::new())
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent, KeyModifiers};

    use super::*;

    fn key(c: char, mods: KeyModifiers, kind: KeyEventKind) -> Event {
        Event::Key(KeyEvent::new_with_kind(KeyCode::Char(c), mods, kind))
    }

    fn taken_paste() -> Event {
        Event::Paste(String::new())
    }

    #[test]
    fn a_typed_v_stays_a_key() {
        let mut taken = TakenPaste::default();
        let press = key('v', KeyModifiers::NONE, KeyEventKind::Press);
        let release = key('v', KeyModifiers::NONE, KeyEventKind::Release);
        assert_eq!(taken.restore(press.clone()), press);
        assert_eq!(taken.restore(release.clone()), release);
    }

    #[test]
    fn a_ctrl_v_the_terminal_let_through_stays_a_key() {
        let mut taken = TakenPaste::default();
        let press = key('v', KeyModifiers::CONTROL, KeyEventKind::Press);
        let release = key('v', KeyModifiers::CONTROL, KeyEventKind::Release);
        assert_eq!(taken.restore(press.clone()), press);
        assert_eq!(taken.restore(release.clone()), release);
    }

    #[test]
    fn a_v_released_without_its_press_is_a_paste() {
        let mut taken = TakenPaste::default();
        let release = key('v', KeyModifiers::CONTROL, KeyEventKind::Release);
        assert_eq!(taken.restore(release), taken_paste());
    }

    #[test]
    fn so_is_one_whose_ctrl_was_let_go_first() {
        let mut taken = TakenPaste::default();
        let release = key('v', KeyModifiers::NONE, KeyEventKind::Release);
        assert_eq!(taken.restore(release), taken_paste());
    }

    #[test]
    fn a_v_in_the_pasted_text_does_not_stand_in_for_the_press() {
        // What a paste of "v" looks like: the letter typed out by the console,
        // then the release of the chord that asked for it.
        let mut taken = TakenPaste::default();
        taken.restore(key('v', KeyModifiers::NONE, KeyEventKind::Press));
        taken.restore(key('v', KeyModifiers::NONE, KeyEventKind::Release));
        let release = key('v', KeyModifiers::CONTROL, KeyEventKind::Release);
        assert_eq!(taken.restore(release), taken_paste());
    }

    #[test]
    fn a_held_v_is_released_once() {
        let mut taken = TakenPaste::default();
        let press = key('v', KeyModifiers::NONE, KeyEventKind::Press);
        let release = key('v', KeyModifiers::NONE, KeyEventKind::Release);
        taken.restore(press.clone());
        taken.restore(press);
        assert_eq!(taken.restore(release.clone()), release);
    }

    #[test]
    fn other_releases_are_left_alone() {
        let mut taken = TakenPaste::default();
        let release = key('c', KeyModifiers::CONTROL, KeyEventKind::Release);
        assert_eq!(taken.restore(release.clone()), release);
    }
}
