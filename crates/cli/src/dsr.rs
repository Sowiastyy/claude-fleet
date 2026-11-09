//! Replies to the terminal queries a child sends upstream.
//!
//! ConPTY opens by asking for the cursor position (`ESC [ 6 n`) and waits for
//! the answer before letting the child produce any output. A pane that never
//! answers looks like a hung process, so the reader thread scans the child's
//! output for these queries and writes the replies back into the PTY.

/// A query a child terminal expects an answer to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query {
    /// `ESC [ 6 n` — report the cursor position.
    CursorPosition,
    /// `ESC [ 5 n` — report device status.
    DeviceStatus,
    /// `ESC [ c` / `ESC [ 0 c` — primary device attributes.
    PrimaryAttributes,
    /// `ESC [ > c` — secondary device attributes.
    SecondaryAttributes,
}

impl Query {
    /// The bytes to write back. `cursor` is the zero-based position the screen
    /// reached after the query was processed.
    pub fn reply(self, cursor: (u16, u16)) -> Vec<u8> {
        match self {
            // Reports are one-based.
            Self::CursorPosition => format!("\x1b[{};{}R", cursor.0 + 1, cursor.1 + 1).into_bytes(),
            Self::DeviceStatus => b"\x1b[0n".to_vec(),
            // "VT100 with advanced video", which is what most emulators claim.
            Self::PrimaryAttributes => b"\x1b[?1;2c".to_vec(),
            Self::SecondaryAttributes => b"\x1b[>0;276;0c".to_vec(),
        }
    }
}

/// How many trailing bytes to carry over between reads so a control sequence
/// split across two chunks is still recognised.
pub const TAIL: usize = 16;

/// Scan a byte run for terminal queries.
///
/// Returns the queries found and the index one past the last complete sequence,
/// so the caller can retain only the bytes that might still be a partial match.
pub fn scan(buf: &[u8]) -> (Vec<Query>, usize) {
    let mut found = Vec::new();
    let mut consumed = 0;
    let mut i = 0;

    while i < buf.len() {
        if buf[i] != 0x1b {
            i += 1;
            continue;
        }
        match parse_csi(&buf[i..]) {
            // An incomplete sequence at the end of the buffer: stop here so the
            // caller keeps it for next time.
            CsiScan::Incomplete => break,
            CsiScan::NotCsi => {
                i += 1;
            }
            CsiScan::Complete {
                params,
                final_byte,
                len,
            } => {
                if let Some(q) = classify(params, final_byte) {
                    found.push(q);
                }
                i += len;
                consumed = i;
            }
        }
    }

    if found.is_empty() {
        consumed = buf.len();
    }
    (found, consumed)
}

enum CsiScan<'a> {
    Complete {
        params: &'a [u8],
        final_byte: u8,
        len: usize,
    },
    Incomplete,
    NotCsi,
}

/// Parse `ESC [ <params> <intermediates> <final>` starting at `buf[0] == ESC`.
fn parse_csi(buf: &[u8]) -> CsiScan<'_> {
    if buf.len() < 2 {
        return CsiScan::Incomplete;
    }
    if buf[1] != b'[' {
        return CsiScan::NotCsi;
    }

    let mut i = 2;
    let start = i;
    // Parameter bytes: 0x30-0x3F.
    while i < buf.len() && (0x30..=0x3f).contains(&buf[i]) {
        i += 1;
    }
    let params = &buf[start..i];
    // Intermediate bytes: 0x20-0x2F.
    while i < buf.len() && (0x20..=0x2f).contains(&buf[i]) {
        i += 1;
    }
    if i >= buf.len() {
        return CsiScan::Incomplete;
    }
    // Final byte: 0x40-0x7E.
    if !(0x40..=0x7e).contains(&buf[i]) {
        return CsiScan::NotCsi;
    }
    CsiScan::Complete {
        params,
        final_byte: buf[i],
        len: i + 1,
    }
}

fn classify(params: &[u8], final_byte: u8) -> Option<Query> {
    match final_byte {
        b'n' => match params {
            b"6" => Some(Query::CursorPosition),
            b"5" => Some(Query::DeviceStatus),
            _ => None,
        },
        b'c' => match params {
            b"" | b"0" => Some(Query::PrimaryAttributes),
            p if p.starts_with(b">") => Some(Query::SecondaryAttributes),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_conpty_startup_query() {
        let (q, consumed) = scan(b"\x1b[6n");
        assert_eq!(q, vec![Query::CursorPosition]);
        assert_eq!(consumed, 4);
    }

    #[test]
    fn cursor_report_is_one_based() {
        assert_eq!(Query::CursorPosition.reply((0, 0)), b"\x1b[1;1R");
        assert_eq!(Query::CursorPosition.reply((11, 4)), b"\x1b[12;5R");
    }

    #[test]
    fn ignores_ordinary_output_and_styling() {
        let (q, _) = scan(b"hello \x1b[1;32mworld\x1b[0m\r\n");
        assert!(q.is_empty());
    }

    #[test]
    fn does_not_confuse_a_cursor_move_with_a_query() {
        // `ESC [ 6 A` moves up six lines; only `n` is a status report.
        let (q, _) = scan(b"\x1b[6A");
        assert!(q.is_empty());
    }

    #[test]
    fn detects_a_query_embedded_in_output() {
        let (q, consumed) = scan(b"abc\x1b[6ndef");
        assert_eq!(q, vec![Query::CursorPosition]);
        // Everything up to the end of the sequence is accounted for.
        assert_eq!(consumed, 7);
    }

    #[test]
    fn keeps_a_split_sequence_for_the_next_chunk() {
        let (q, consumed) = scan(b"out\x1b[");
        assert!(q.is_empty());
        // The dangling ESC [ must be retained, not consumed.
        assert_eq!(consumed, 5);
    }

    #[test]
    fn recognises_both_device_attribute_forms() {
        assert_eq!(scan(b"\x1b[c").0, vec![Query::PrimaryAttributes]);
        assert_eq!(scan(b"\x1b[0c").0, vec![Query::PrimaryAttributes]);
        assert_eq!(scan(b"\x1b[>c").0, vec![Query::SecondaryAttributes]);
        assert_eq!(scan(b"\x1b[>0c").0, vec![Query::SecondaryAttributes]);
    }

    #[test]
    fn device_status_reports_ok() {
        assert_eq!(scan(b"\x1b[5n").0, vec![Query::DeviceStatus]);
        assert_eq!(Query::DeviceStatus.reply((0, 0)), b"\x1b[0n");
    }

    #[test]
    fn finds_several_queries_in_one_chunk() {
        let (q, _) = scan(b"\x1b[6n\x1b[c");
        assert_eq!(q, vec![Query::CursorPosition, Query::PrimaryAttributes]);
    }
}
