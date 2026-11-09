//! Translation of crossterm key events into the byte sequences a PTY expects.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Strip the modifiers Windows attaches to a character typed with AltGr.
///
/// On layouts like Polish programmer's, `ą` is AltGr+a, and the console reports
/// AltGr as right-Alt plus left-Ctrl held together. Pasting is no different:
/// conhost re-types the clipboard as key records, and every letter that needs
/// AltGr on the active layout arrives with the same two modifiers. Taken at face
/// value that is a Ctrl+Alt chord, which the encoder turns into `ESC ą`, the
/// paste coalescer refuses to treat as text, and the path form drops outright.
/// The character already reflects the layout, so the modifiers carry nothing.
///
/// Only non-ASCII characters qualify. A real Ctrl+Alt+letter chord reports the
/// plain letter (crossterm looks it up from the layout when the console gives
/// no character), and that has to stay a chord.
pub fn normalize(key: KeyEvent) -> KeyEvent {
    let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
    match key.code {
        KeyCode::Char(c) if key.modifiers.contains(altgr) && !c.is_ascii() => KeyEvent {
            modifiers: key.modifiers.difference(altgr),
            ..key
        },
        _ => key,
    }
}

/// Encode a key press for the child terminal.
///
/// `app_cursor` reflects DECCKM: in application-cursor mode the arrow and Home/
/// End keys use SS3 (`ESC O`) instead of CSI (`ESC [`).
pub fn encode(key: KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    let bytes = match key.code {
        KeyCode::Char(c) => {
            let mut out = Vec::new();
            if alt {
                out.push(0x1b);
            }
            if ctrl {
                // C0 control codes: ^A..^Z plus the handful above them.
                let b = match c.to_ascii_lowercase() {
                    c @ 'a'..='z' => c as u8 - b'a' + 1,
                    ' ' | '@' => 0,
                    '[' => 0x1b,
                    '\\' => 0x1c,
                    ']' => 0x1d,
                    '^' => 0x1e,
                    '_' | '?' => 0x1f,
                    // Anything else — a digit, punctuation, a non-ASCII
                    // letter — has no control code, so it goes through as
                    // itself.
                    _ => {
                        out.extend_from_slice(c.to_string().as_bytes());
                        return Some(out);
                    }
                };
                out.push(b);
            } else {
                out.extend_from_slice(c.to_string().as_bytes());
            }
            out
        }

        // Claude Code treats meta+enter as "newline, don't submit"; keep that
        // reachable, and map shift+enter to it since terminals that report
        // shift+enter at all mean the same thing by it.
        KeyCode::Enter if alt || shift => vec![0x1b, b'\r'],
        KeyCode::Enter => vec![b'\r'],

        KeyCode::Backspace if alt => vec![0x1b, 0x7f],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],

        KeyCode::Up => cursor_key(b'A', app_cursor, key.modifiers),
        KeyCode::Down => cursor_key(b'B', app_cursor, key.modifiers),
        KeyCode::Right => cursor_key(b'C', app_cursor, key.modifiers),
        KeyCode::Left => cursor_key(b'D', app_cursor, key.modifiers),
        KeyCode::Home => cursor_key(b'H', app_cursor, key.modifiers),
        KeyCode::End => cursor_key(b'F', app_cursor, key.modifiers),

        KeyCode::Insert => tilde_key(2, key.modifiers),
        KeyCode::Delete => tilde_key(3, key.modifiers),
        KeyCode::PageUp => tilde_key(5, key.modifiers),
        KeyCode::PageDown => tilde_key(6, key.modifiers),

        KeyCode::F(n) => function_key(n, key.modifiers)?,

        _ => return None,
    };

    Some(bytes)
}

/// CSI modifier parameter: 1 + bitmask of shift/alt/ctrl.
fn modifier_param(mods: KeyModifiers) -> Option<u8> {
    let mut m = 1;
    if mods.contains(KeyModifiers::SHIFT) {
        m += 1;
    }
    if mods.contains(KeyModifiers::ALT) {
        m += 2;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        m += 4;
    }
    (m > 1).then_some(m)
}

fn cursor_key(final_byte: u8, app_cursor: bool, mods: KeyModifiers) -> Vec<u8> {
    match modifier_param(mods) {
        // Modified cursor keys are always CSI, even in application mode.
        Some(m) => format!("\x1b[1;{m}{}", final_byte as char).into_bytes(),
        None if app_cursor => vec![0x1b, b'O', final_byte],
        None => vec![0x1b, b'[', final_byte],
    }
}

fn tilde_key(n: u8, mods: KeyModifiers) -> Vec<u8> {
    match modifier_param(mods) {
        Some(m) => format!("\x1b[{n};{m}~").into_bytes(),
        None => format!("\x1b[{n}~").into_bytes(),
    }
}

fn function_key(n: u8, mods: KeyModifiers) -> Option<Vec<u8>> {
    // F1-F4 are SS3 when unmodified; the rest use the CSI ~ form.
    let plain: Vec<u8> = match n {
        1..=4 if modifier_param(mods).is_none() => return Some(vec![0x1b, b'O', b'P' + (n - 1)]),
        1 => b"\x1b[11".to_vec(),
        2 => b"\x1b[12".to_vec(),
        3 => b"\x1b[13".to_vec(),
        4 => b"\x1b[14".to_vec(),
        5 => b"\x1b[15".to_vec(),
        6..=10 => format!("\x1b[{}", n + 11).into_bytes(),
        11 | 12 => format!("\x1b[{}", n + 12).into_bytes(),
        _ => return None,
    };

    let mut out = plain;
    if let Some(m) = modifier_param(mods) {
        out.extend_from_slice(format!(";{m}").as_bytes());
    }
    out.push(b'~');
    Some(out)
}

/// Wrap pasted text in bracketed-paste markers when the child asked for them.
///
/// One paste too large to hand over in a single piece is sent as several calls,
/// and the markers belong to the paste, not to the piece: `open` on the first,
/// `close` on the last. Bracketing every piece separately would hand the child
/// a row of complete pastes instead of the one it is actually receiving.
pub fn encode_paste_chunk(text: &str, bracketed: bool, open: bool, close: bool) -> Vec<u8> {
    if !bracketed {
        return text.as_bytes().to_vec();
    }
    let mut out = Vec::with_capacity(text.len() + PASTE_START.len() + PASTE_END.len());
    if open {
        out.extend_from_slice(PASTE_START);
    }
    out.extend_from_slice(text.as_bytes());
    if close {
        out.extend_from_slice(PASTE_END);
    }
    out
}

pub const PASTE_START: &[u8] = b"[200~";
pub const PASTE_END: &[u8] = b"[201~";

/// Encode a mouse wheel notch for a child that asked for mouse reporting.
///
/// Claude Code runs on the alternate screen (`ESC[?1049h`) and turns on its own
/// tracking (`ESC[?1000h` … `ESC[?1006h`): its history lives inside the child,
/// not in the emulator's scrollback, which the alternate grid does not even
/// keep. So a wheel notch has to be handed to the child the way a real terminal
/// hands it over, or nothing scrolls at all.
///
/// `col` and `row` are 1-based and pane-relative.
pub fn encode_wheel(
    up: bool,
    col: u16,
    row: u16,
    encoding: vt100::MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    // Wheel buttons are the low two bits with bit 6 (64) set.
    let button: u16 = if up { 64 } else { 65 };
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            Some(format!("\x1b[<{button};{col};{row}M").into_bytes())
        }
        vt100::MouseProtocolEncoding::Utf8 => {
            let mut out = b"\x1b[M".to_vec();
            for v in [button + 32, col + 32, row + 32] {
                let mut buf = [0u8; 4];
                out.extend_from_slice(
                    char::from_u32(u32::from(v))?
                        .encode_utf8(&mut buf)
                        .as_bytes(),
                );
            }
            Some(out)
        }
        vt100::MouseProtocolEncoding::Default => {
            // The classic encoding is one byte per field, so it simply cannot
            // address anything past column 223.
            let (b, c, r) = (button + 32, col + 32, row + 32);
            if b > 255 || c > 255 || r > 255 {
                return None;
            }
            Some(vec![0x1b, b'[', b'M', b as u8, c as u8, r as u8])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn altgr_letter_loses_its_modifiers() {
        let k = normalize(key(
            KeyCode::Char('ą'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert_eq!(k.code, KeyCode::Char('ą'));
        assert!(k.modifiers.is_empty());
    }

    #[test]
    fn altgr_with_shift_keeps_only_the_shift() {
        let mods = KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT;
        let k = normalize(key(KeyCode::Char('Ą'), mods));
        assert_eq!(k.modifiers, KeyModifiers::SHIFT);
    }

    #[test]
    fn a_real_ctrl_alt_chord_is_left_alone() {
        let k = normalize(key(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert_eq!(k.modifiers, KeyModifiers::CONTROL | KeyModifiers::ALT);
    }

    #[test]
    fn a_polish_letter_encodes_as_its_utf8() {
        let k = normalize(key(
            KeyCode::Char('ł'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert_eq!(encode(k, false).unwrap(), "ł".as_bytes());
    }

    #[test]
    fn ctrl_c_is_etx() {
        let out = encode(key(KeyCode::Char('c'), KeyModifiers::CONTROL), false).unwrap();
        assert_eq!(out, vec![0x03]);
    }

    #[test]
    fn arrows_follow_application_cursor_mode() {
        let normal = encode(key(KeyCode::Up, KeyModifiers::NONE), false).unwrap();
        assert_eq!(normal, b"\x1b[A");
        let app = encode(key(KeyCode::Up, KeyModifiers::NONE), true).unwrap();
        assert_eq!(app, b"\x1bOA");
    }

    #[test]
    fn modified_arrows_stay_csi_in_application_mode() {
        let out = encode(key(KeyCode::Up, KeyModifiers::CONTROL), true).unwrap();
        assert_eq!(out, b"\x1b[1;5A");
    }

    #[test]
    fn shift_enter_is_meta_enter() {
        let out = encode(key(KeyCode::Enter, KeyModifiers::SHIFT), false).unwrap();
        assert_eq!(out, vec![0x1b, b'\r']);
    }

    #[test]
    fn plain_f1_is_ss3() {
        let out = encode(key(KeyCode::F(1), KeyModifiers::NONE), false).unwrap();
        assert_eq!(out, b"\x1bOP");
    }

    #[test]
    fn f5_is_csi_tilde() {
        let out = encode(key(KeyCode::F(5), KeyModifiers::NONE), false).unwrap();
        assert_eq!(out, b"\x1b[15~");
    }

    #[test]
    fn a_whole_paste_carries_both_markers() {
        let out = encode_paste_chunk("hi", true, true, true);
        assert_eq!(out, b"[200~hi[201~");
    }

    #[test]
    fn a_split_paste_brackets_only_its_ends() {
        assert_eq!(encode_paste_chunk("ab", true, true, false), b"[200~ab");
        assert_eq!(encode_paste_chunk("cd", true, false, false), b"cd");
        assert_eq!(encode_paste_chunk("ef", true, false, true), b"ef[201~");
    }

    #[test]
    fn paste_stays_raw_when_the_child_did_not_ask_for_brackets() {
        assert_eq!(encode_paste_chunk("hi", false, true, true), b"hi");
    }

    #[test]
    fn alt_char_is_escape_prefixed() {
        let out = encode(key(KeyCode::Char('b'), KeyModifiers::ALT), false).unwrap();
        assert_eq!(out, vec![0x1b, b'b']);
    }
}
