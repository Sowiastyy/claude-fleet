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

use crossterm::event::{self, Event, KeyCode, KeyEventKind};

pub struct Input {
    rx: Receiver<Event>,
}

impl Input {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut taken = TakenPaste::default();
            while let Ok(ev) = event::read() {
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
