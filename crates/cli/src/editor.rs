//! One file open in the editor: its lines, a cursor with an optional
//! selection, and the undo history.
//!
//! Positions are `(line, col)` with the column counted in characters, never
//! bytes, so a cursor can not land inside a multi-byte character. How wide a
//! character is on screen (a tab, a CJK ideograph) is a separate question,
//! answered by `display_col` and `col_at_display`.
//!
//! Every change goes through `edit`, which replaces a range with text and
//! records the inverse. Undo is those records played backwards; typing is
//! folded into one record per word, so ctrl+z does not take back a letter at
//! a time.

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use unicode_width::UnicodeWidthChar;

use crate::syntax::{self, Lang};

/// Past this a file is not opened: the whole of it lives in memory, and a
/// log or a dump that size is not something to edit by hand anyway.
pub const MAX_FILE: u64 = 8 * 1024 * 1024;
/// How many columns a tab advances to.
pub const TAB: usize = 4;
/// Undo steps kept per file.
const UNDO_LIMIT: usize = 1000;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// One replacement: at `at`, `removed` was taken out and `inserted` put in.
#[derive(Clone, Debug)]
struct Edit {
    at: Pos,
    removed: String,
    inserted: String,
}

/// What one undo takes back.
#[derive(Clone, Debug)]
struct Group {
    edits: Vec<Edit>,
    /// Cursor and anchor before the first edit, restored by undo.
    before: (Pos, Option<Pos>),
    /// Cursor after the last edit, restored by redo.
    after: Pos,
    id: u64,
}

/// What the last change was, for folding the next one into it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Type,
    Delete,
    Other,
}

/// What a look at the file on disk found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Disk {
    Same,
    /// It changed and nothing here was unsaved, so it was read again.
    Reloaded,
    /// It changed under unsaved edits. Said once, until saved or reloaded.
    Conflict,
    /// It is not there any more.
    Gone,
}

pub struct Buffer {
    pub path: PathBuf,
    /// Never empty: an empty file is one empty line.
    pub lines: Vec<String>,
    /// The file used `\r\n`; it is written back the same way.
    pub crlf: bool,
    bom: bool,
    pub cursor: Pos,
    /// The other end of the selection, when there is one.
    pub anchor: Option<Pos>,
    /// The screen column up and down aim for, kept across short lines.
    want_col: Option<usize>,
    /// First line and first screen column on show.
    pub scroll: usize,
    pub hscroll: usize,
    /// The view should keep the cursor in sight. The wheel turns this off,
    /// so scrolling away from the cursor is not undone on the next frame;
    /// anything that moves the cursor turns it back on.
    pub follow: bool,
    undo: Vec<Group>,
    redo: Vec<Group>,
    next_id: u64,
    /// The undo group the file on disk matches; 0 is the file as opened.
    saved_id: u64,
    last_kind: Kind,
    last_ws: bool,
    /// Modification time and length as last read or written.
    stamp: Option<(Option<SystemTime>, u64)>,
    /// The file changed on disk while there were unsaved edits here.
    pub changed_on_disk: bool,
    pub gone: bool,
    pub lang: &'static Lang,
    /// What one level of indentation is in this file.
    pub indent: String,
}

impl Buffer {
    /// Read a file for editing. Refuses what would not survive the round
    /// trip: binary files, text that is not UTF-8, and the very large.
    pub fn open(path: &Path) -> Result<Self, String> {
        let meta = fs::metadata(path).map_err(|e| e.to_string())?;
        if meta.is_dir() {
            return Err("that is a directory".into());
        }
        if meta.len() > MAX_FILE {
            return Err(format!(
                "too large to edit here ({} MB)",
                meta.len() / (1024 * 1024)
            ));
        }
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        let (text, bom, crlf) = decode(&bytes)?;
        let lines = split_lines(&text);
        let indent = detect_indent(&lines);
        Ok(Self {
            path: path.to_path_buf(),
            lines,
            crlf,
            bom,
            cursor: Pos::default(),
            anchor: None,
            want_col: None,
            scroll: 0,
            hscroll: 0,
            follow: true,
            undo: Vec::new(),
            redo: Vec::new(),
            next_id: 1,
            saved_id: 0,
            last_kind: Kind::Other,
            last_ws: false,
            stamp: Some(stamp_of(&meta)),
            changed_on_disk: false,
            gone: false,
            lang: syntax::detect(path),
            indent,
        })
    }

    /// A buffer that is not backed by anything yet, for tests.
    #[cfg(test)]
    pub fn scratch(text: &str) -> Self {
        let lines = split_lines(text);
        let indent = detect_indent(&lines);
        Self {
            path: PathBuf::from("scratch.txt"),
            lines,
            crlf: false,
            bom: false,
            cursor: Pos::default(),
            anchor: None,
            want_col: None,
            scroll: 0,
            hscroll: 0,
            follow: true,
            undo: Vec::new(),
            redo: Vec::new(),
            next_id: 1,
            saved_id: 0,
            last_kind: Kind::Other,
            last_ws: false,
            stamp: None,
            changed_on_disk: false,
            gone: false,
            lang: syntax::detect(Path::new("scratch.txt")),
            indent,
        }
    }

    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    fn current_id(&self) -> u64 {
        self.undo.last().map_or(0, |g| g.id)
    }

    /// Whether there is something here the file on disk does not have.
    pub fn dirty(&self) -> bool {
        self.current_id() != self.saved_id
    }

    pub fn save(&mut self) -> Result<(), String> {
        let mut body = self.text();
        if self.crlf {
            body = body.replace('\n', "\r\n");
        }
        let mut bytes = Vec::with_capacity(body.len() + 3);
        if self.bom {
            bytes.extend_from_slice(b"\xef\xbb\xbf");
        }
        bytes.extend_from_slice(body.as_bytes());
        fs::write(&self.path, bytes).map_err(|e| e.to_string())?;
        self.stamp = fs::metadata(&self.path).ok().map(|m| stamp_of(&m));
        self.saved_id = self.current_id();
        self.changed_on_disk = false;
        self.gone = false;
        // Typing after a save starts a new undo step, or the step holding the
        // saved state would be stretched past it.
        self.last_kind = Kind::Other;
        Ok(())
    }

    /// Compare the file on disk with what was last read or written, and read
    /// it again when it moved and nothing here would be lost.
    ///
    /// Sessions edit the same files this editor has open, so this is the
    /// ordinary case, not the exotic one.
    pub fn check_disk(&mut self) -> Disk {
        let Ok(meta) = fs::metadata(&self.path) else {
            if self.stamp.is_some() && !self.gone {
                self.gone = true;
                return Disk::Gone;
            }
            return Disk::Same;
        };
        let now = stamp_of(&meta);
        if self.stamp == Some(now) {
            return Disk::Same;
        }
        if self.dirty() {
            if self.changed_on_disk {
                return Disk::Same;
            }
            self.changed_on_disk = true;
            return Disk::Conflict;
        }
        match self.reload() {
            Ok(true) => Disk::Reloaded,
            _ => Disk::Same,
        }
    }

    /// Take what is on disk, as an undoable step. Returns whether the text
    /// changed at all.
    pub fn reload(&mut self) -> Result<bool, String> {
        let meta = fs::metadata(&self.path).map_err(|e| e.to_string())?;
        if meta.len() > MAX_FILE {
            return Err("the file grew too large to edit here".into());
        }
        let bytes = fs::read(&self.path).map_err(|e| e.to_string())?;
        let (text, bom, crlf) = decode(&bytes)?;
        self.stamp = Some(stamp_of(&meta));
        self.changed_on_disk = false;
        self.gone = false;
        self.bom = bom;
        self.crlf = crlf;
        let text = split_lines(&text).join("\n");
        if text == self.text() {
            self.saved_id = self.current_id();
            return Ok(false);
        }
        let (cursor, scroll) = (self.cursor, self.scroll);
        let end = self.end();
        self.edit(Pos::default(), end, &text, Kind::Other);
        self.last_kind = Kind::Other;
        self.saved_id = self.current_id();
        self.cursor = self.clamp(cursor);
        self.anchor = None;
        self.scroll = scroll.min(self.lines.len() - 1);
        Ok(true)
    }

    // ---- positions -------------------------------------------------------

    pub fn line_len(&self, line: usize) -> usize {
        self.lines.get(line).map_or(0, |l| l.chars().count())
    }

    pub fn end(&self) -> Pos {
        let last = self.lines.len() - 1;
        Pos::new(last, self.line_len(last))
    }

    pub fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        Pos::new(line, p.col.min(self.line_len(line)))
    }

    /// The selection in reading order, when there is a non-empty one.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        match a.cmp(&self.cursor) {
            std::cmp::Ordering::Less => Some((a, self.cursor)),
            std::cmp::Ordering::Greater => Some((self.cursor, a)),
            std::cmp::Ordering::Equal => None,
        }
    }

    pub fn text_range(&self, start: Pos, end: Pos) -> String {
        if start.line == end.line {
            let l = &self.lines[start.line];
            return l[byte_at(l, start.col)..byte_at(l, end.col)].to_string();
        }
        let first = &self.lines[start.line];
        let mut out = first[byte_at(first, start.col)..].to_string();
        for l in &self.lines[start.line + 1..end.line] {
            out.push('\n');
            out.push_str(l);
        }
        out.push('\n');
        let last = &self.lines[end.line];
        out.push_str(&last[..byte_at(last, end.col)]);
        out
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection().map(|(s, e)| self.text_range(s, e))
    }

    // ---- the one way text changes ---------------------------------------

    /// Replace `start..end` with `text`, returning where the new text ends.
    fn raw_replace(&mut self, start: Pos, end: Pos, text: &str) -> Pos {
        let sb = byte_at(&self.lines[start.line], start.col);
        let eb = byte_at(&self.lines[end.line], end.col);
        let prefix = self.lines[start.line][..sb].to_string();
        let suffix = self.lines[end.line][eb..].to_string();
        let mut new: Vec<String> = text.split('\n').map(String::from).collect();
        let n = new.len();
        let last_len = new[n - 1].chars().count();
        new[0].insert_str(0, &prefix);
        new[n - 1].push_str(&suffix);
        self.lines.splice(start.line..=end.line, new);
        if n == 1 {
            Pos::new(start.line, start.col + last_len)
        } else {
            Pos::new(start.line + n - 1, last_len)
        }
    }

    fn edit(&mut self, start: Pos, end: Pos, text: &str, kind: Kind) -> Pos {
        let (start, end) = (self.clamp(start), self.clamp(end));
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let before = (self.cursor, self.anchor);
        let removed = self.text_range(start, end);
        if removed.is_empty() && text.is_empty() {
            return start;
        }
        let after = self.raw_replace(start, end, text);
        self.redo.clear();
        let ws = text.chars().all(char::is_whitespace);
        // A word and the space before it are one step; the space after a
        // word starts the next one.
        let fold = kind != Kind::Other
            && kind == self.last_kind
            && !(kind == Kind::Type && ws && !self.last_ws)
            && !self.undo.is_empty();
        let e = Edit {
            at: start,
            removed,
            inserted: text.to_string(),
        };
        if fold && let Some(g) = self.undo.last_mut() {
            g.edits.push(e);
            g.after = after;
        } else {
            self.undo.push(Group {
                edits: vec![e],
                before,
                after,
                id: self.next_id,
            });
            self.next_id += 1;
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.last_kind = kind;
        self.last_ws = ws;
        self.cursor = after;
        self.anchor = None;
        self.want_col = None;
        self.follow = true;
        after
    }

    pub fn undo(&mut self) -> bool {
        let Some(g) = self.undo.pop() else {
            return false;
        };
        for e in g.edits.iter().rev() {
            let end = end_of(e.at, &e.inserted);
            self.raw_replace(e.at, end, &e.removed);
        }
        self.cursor = self.clamp(g.before.0);
        self.anchor = g.before.1.map(|a| self.clamp(a));
        self.redo.push(g);
        self.after_history();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(g) = self.redo.pop() else {
            return false;
        };
        for e in &g.edits {
            let end = end_of(e.at, &e.removed);
            self.raw_replace(e.at, end, &e.inserted);
        }
        self.cursor = self.clamp(g.after);
        self.anchor = None;
        self.undo.push(g);
        self.after_history();
        true
    }

    fn after_history(&mut self) {
        self.last_kind = Kind::Other;
        self.want_col = None;
        self.follow = true;
    }

    // ---- typing ----------------------------------------------------------

    /// Put `text` where the cursor is, over the selection if there is one.
    pub fn insert(&mut self, text: &str) {
        let (start, end) = self.selection().unwrap_or((self.cursor, self.cursor));
        let one_char = text.chars().count() == 1 && text != "\n";
        let kind = if one_char && start == end {
            Kind::Type
        } else {
            Kind::Other
        };
        self.edit(start, end, text, kind);
    }

    /// Enter: a new line indented like this one, one level deeper after an
    /// opening bracket, and with the closing bracket moved down a line of its
    /// own when the cursor sat between the two.
    pub fn newline(&mut self) {
        let (start, end) = self.selection().unwrap_or((self.cursor, self.cursor));
        let line = &self.lines[start.line];
        let indent: String = line
            .chars()
            .take(start.col)
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let before = line.chars().take(start.col).collect::<String>();
        let opener = before.trim_end().chars().last();
        let after_char = self.lines[end.line].chars().nth(end.col);
        let deeper = matches!(opener, Some('{' | '[' | '('))
            || (opener == Some(':') && self.lang.name == "Python");
        let closes = matches!(
            (opener, after_char),
            (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('('), Some(')'))
        );
        let mut text = format!("\n{indent}");
        if deeper {
            text.push_str(&self.indent);
        }
        let at = self.edit(start, end, &text, Kind::Other);
        if closes {
            self.edit(at, at, &format!("\n{indent}"), Kind::Other);
            self.cursor = at;
            // The two steps are one Enter.
            self.merge_last_two();
        }
    }

    fn merge_last_two(&mut self) {
        if self.undo.len() < 2 {
            return;
        }
        let last = self.undo.pop().expect("checked above");
        let prev = self.undo.last_mut().expect("checked above");
        prev.edits.extend(last.edits);
        prev.after = self.cursor;
    }

    /// Tab: indent the selected lines, or put one level of indentation in.
    pub fn tab(&mut self) {
        match self.selection() {
            Some((s, e)) if s.line != e.line => self.shift_lines(false),
            _ => {
                let unit = self.indent.clone();
                self.insert(&unit);
            }
        }
    }

    /// Shift+Tab: take one level of indentation off the lines.
    pub fn backtab(&mut self) {
        self.shift_lines(true);
    }

    pub fn backspace(&mut self) {
        if let Some((s, e)) = self.selection() {
            self.edit(s, e, "", Kind::Other);
            return;
        }
        let c = self.cursor;
        if c.col == 0 {
            if c.line > 0 {
                let prev = Pos::new(c.line - 1, self.line_len(c.line - 1));
                self.edit(prev, c, "", Kind::Delete);
            }
            return;
        }
        // In the indentation of a line indented with spaces, one press takes
        // back a whole level, the way it went in.
        let head: String = self.lines[c.line].chars().take(c.col).collect();
        let unit = self.indent.chars().count();
        let from = if !self.indent.starts_with('\t') && head.chars().all(|ch| ch == ' ') {
            let w = head.chars().count();
            let back = match w % unit {
                0 => unit,
                r => r,
            };
            c.col - back.min(w)
        } else {
            c.col - 1
        };
        self.edit(Pos::new(c.line, from), c, "", Kind::Delete);
    }

    pub fn delete(&mut self) {
        if let Some((s, e)) = self.selection() {
            self.edit(s, e, "", Kind::Other);
            return;
        }
        let c = self.cursor;
        let next = if c.col < self.line_len(c.line) {
            Pos::new(c.line, c.col + 1)
        } else if c.line + 1 < self.lines.len() {
            Pos::new(c.line + 1, 0)
        } else {
            return;
        };
        self.edit(c, next, "", Kind::Delete);
    }

    pub fn delete_word_back(&mut self) {
        if self.selection().is_some() {
            return self.backspace();
        }
        let to = self.word_left_of(self.cursor);
        self.edit(to, self.cursor, "", Kind::Other);
    }

    pub fn delete_word_forward(&mut self) {
        if self.selection().is_some() {
            return self.delete();
        }
        let to = self.word_right_of(self.cursor);
        self.edit(self.cursor, to, "", Kind::Other);
    }

    /// The line range the selection touches, or the cursor's line. A
    /// selection ending at the very start of a line leaves that line out.
    fn touched_lines(&self) -> (usize, usize) {
        match self.selection() {
            Some((s, e)) if e.col == 0 && e.line > s.line => (s.line, e.line - 1),
            Some((s, e)) => (s.line, e.line),
            None => (self.cursor.line, self.cursor.line),
        }
    }

    /// Replace whole lines `a..=b` with `new`, as one step, and select them.
    fn replace_lines(&mut self, a: usize, b: usize, new: Vec<String>, select: bool) {
        let end = Pos::new(b, self.line_len(b));
        let count = new.len();
        let last_len = new.last().map_or(0, |l| l.chars().count());
        self.edit(Pos::new(a, 0), end, &new.join("\n"), Kind::Other);
        if select {
            self.anchor = Some(Pos::new(a, 0));
            self.cursor = Pos::new(a + count - 1, last_len);
        }
    }

    fn shift_lines(&mut self, out: bool) {
        let (a, b) = self.touched_lines();
        let had_selection = self.selection().is_some();
        let cursor = self.cursor;
        let unit = self.indent.clone();
        let width = unit.chars().count();
        let mut moved = 0isize;
        let new: Vec<String> = self.lines[a..=b]
            .iter()
            .enumerate()
            .map(|(i, l)| {
                if out {
                    let strip = if l.starts_with('\t') {
                        1
                    } else {
                        l.chars().take(width).take_while(|c| *c == ' ').count()
                    };
                    if a + i == cursor.line {
                        moved = -(strip as isize);
                    }
                    l.chars().skip(strip).collect()
                } else if l.is_empty() {
                    String::new()
                } else {
                    if a + i == cursor.line {
                        moved = width as isize;
                    }
                    format!("{unit}{l}")
                }
            })
            .collect();
        if new == self.lines[a..=b] {
            return;
        }
        self.replace_lines(a, b, new, had_selection);
        if !had_selection {
            self.cursor = self.clamp(Pos::new(
                cursor.line,
                cursor.col.saturating_add_signed(moved),
            ));
        }
    }

    /// Alt+Up / Alt+Down: move the touched lines past their neighbour.
    pub fn move_lines(&mut self, up: bool) {
        let (a, b) = self.touched_lines();
        let (cursor, anchor) = (self.cursor, self.anchor);
        if up {
            if a == 0 {
                return;
            }
            let mut new: Vec<String> = self.lines[a..=b].to_vec();
            new.push(self.lines[a - 1].clone());
            self.replace_lines(a - 1, b, new, false);
        } else {
            if b + 1 >= self.lines.len() {
                return;
            }
            let mut new = vec![self.lines[b + 1].clone()];
            new.extend_from_slice(&self.lines[a..=b]);
            self.replace_lines(a, b + 1, new, false);
        }
        let shift = |p: Pos| Pos::new(if up { p.line - 1 } else { p.line + 1 }, p.col);
        self.cursor = self.clamp(shift(cursor));
        self.anchor = anchor.map(|p| self.clamp(shift(p)));
    }

    /// Ctrl+D: the selection again after itself, or the line again below it.
    pub fn duplicate(&mut self) {
        if let Some((s, e)) = self.selection() {
            let text = self.text_range(s, e);
            self.edit(e, e, &text, Kind::Other);
            self.anchor = Some(e);
            return;
        }
        let c = self.cursor;
        let line = self.lines[c.line].clone();
        let end = Pos::new(c.line, self.line_len(c.line));
        self.edit(end, end, &format!("\n{line}"), Kind::Other);
        self.cursor = Pos::new(c.line + 1, c.col);
    }

    /// Ctrl+K: delete the touched lines.
    pub fn delete_lines(&mut self) {
        let (a, b) = self.touched_lines();
        let col = self.cursor.col;
        let (start, end) = if b + 1 < self.lines.len() {
            (Pos::new(a, 0), Pos::new(b + 1, 0))
        } else if a > 0 {
            (
                Pos::new(a - 1, self.line_len(a - 1)),
                Pos::new(b, self.line_len(b)),
            )
        } else {
            (Pos::new(0, 0), Pos::new(b, self.line_len(b)))
        };
        self.edit(start, end, "", Kind::Other);
        let line = a.min(self.lines.len() - 1);
        self.cursor = self.clamp(Pos::new(line, col));
    }

    /// Ctrl+/: comment the touched lines out, or back in when all of them
    /// already are.
    pub fn toggle_comment(&mut self) -> bool {
        let Some(token) = self.lang.line_comment else {
            return false;
        };
        let (a, b) = self.touched_lines();
        let had_selection = self.selection().is_some();
        let cursor = self.cursor;
        let lines = &self.lines[a..=b];
        let blank = |l: &String| l.trim().is_empty();
        let all = lines
            .iter()
            .filter(|l| !blank(l))
            .all(|l| l.trim_start().starts_with(token));
        let depth = lines
            .iter()
            .filter(|l| !blank(l))
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        let new: Vec<String> = lines
            .iter()
            .map(|l| {
                if blank(l) {
                    return l.clone();
                }
                if all {
                    let lead = l.len() - l.trim_start().len();
                    let rest = &l[lead + token.len()..];
                    let rest = rest.strip_prefix(' ').unwrap_or(rest);
                    format!("{}{rest}", &l[..lead])
                } else {
                    format!("{}{token} {}", &l[..depth], &l[depth..])
                }
            })
            .collect();
        self.replace_lines(a, b, new, had_selection);
        if !had_selection {
            self.cursor = self.clamp(cursor);
        }
        true
    }

    /// Replace every match of `query` with `with`, as one step. Returns how
    /// many there were.
    pub fn replace_all(&mut self, query: &str, with: &str) -> usize {
        if query.is_empty() || query.contains('\n') {
            return 0;
        }
        let mut count = 0;
        let new: Vec<String> = (0..self.lines.len())
            .map(|i| {
                let hits = self.matches_in_line(i, query);
                if hits.is_empty() {
                    return self.lines[i].clone();
                }
                count += hits.len();
                let chars: Vec<char> = self.lines[i].chars().collect();
                let qlen = query.chars().count();
                let mut out = String::new();
                let mut at = 0;
                for h in hits {
                    out.extend(&chars[at..h]);
                    out.push_str(with);
                    at = h + qlen;
                }
                out.extend(&chars[at..]);
                out
            })
            .collect();
        if count > 0 {
            let cursor = self.cursor;
            let last = self.lines.len() - 1;
            self.replace_lines(0, last, new, false);
            self.cursor = self.clamp(cursor);
        }
        count
    }

    // ---- moving ----------------------------------------------------------

    /// Before a move: start a selection, or drop the one there is.
    fn begin_move(&mut self, select: bool) {
        if select {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.last_kind = Kind::Other;
        self.follow = true;
    }

    pub fn left(&mut self, select: bool) {
        if !select && let Some((s, _)) = self.selection() {
            self.anchor = None;
            self.follow = true;
            self.cursor = s;
            self.want_col = None;
            return;
        }
        self.begin_move(select);
        let c = self.cursor;
        self.cursor = if c.col > 0 {
            Pos::new(c.line, c.col - 1)
        } else if c.line > 0 {
            Pos::new(c.line - 1, self.line_len(c.line - 1))
        } else {
            c
        };
        self.want_col = None;
    }

    pub fn right(&mut self, select: bool) {
        if !select && let Some((_, e)) = self.selection() {
            self.anchor = None;
            self.follow = true;
            self.cursor = e;
            self.want_col = None;
            return;
        }
        self.begin_move(select);
        let c = self.cursor;
        self.cursor = if c.col < self.line_len(c.line) {
            Pos::new(c.line, c.col + 1)
        } else if c.line + 1 < self.lines.len() {
            Pos::new(c.line + 1, 0)
        } else {
            c
        };
        self.want_col = None;
    }

    /// Up or down by `delta` lines, aiming for the same screen column.
    pub fn vertical(&mut self, delta: isize, select: bool) {
        self.begin_move(select);
        let c = self.cursor;
        let want = self
            .want_col
            .unwrap_or_else(|| display_col(&self.lines[c.line], c.col));
        let last = self.lines.len() - 1;
        let target = c.line.saturating_add_signed(delta).min(last);
        if target == c.line {
            // Nowhere further to go: the arrow goes to the end of the line.
            self.cursor = if delta < 0 {
                Pos::new(c.line, 0)
            } else {
                Pos::new(c.line, self.line_len(c.line))
            };
            self.want_col = None;
            return;
        }
        self.cursor = Pos::new(target, col_at_display(&self.lines[target], want));
        self.want_col = Some(want);
    }

    /// Home: to the first character that is not indentation, and from there
    /// to the very start.
    pub fn home(&mut self, select: bool) {
        self.begin_move(select);
        let c = self.cursor;
        let indent = self.lines[c.line]
            .chars()
            .take_while(|ch| ch.is_whitespace())
            .count();
        self.cursor.col = if c.col == indent { 0 } else { indent };
        self.want_col = None;
    }

    pub fn end_of_line(&mut self, select: bool) {
        self.begin_move(select);
        self.cursor.col = self.line_len(self.cursor.line);
        self.want_col = None;
    }

    pub fn doc_start(&mut self, select: bool) {
        self.begin_move(select);
        self.cursor = Pos::default();
        self.want_col = None;
    }

    pub fn doc_end(&mut self, select: bool) {
        self.begin_move(select);
        self.cursor = self.end();
        self.want_col = None;
    }

    pub fn word_left(&mut self, select: bool) {
        self.begin_move(select);
        self.cursor = self.word_left_of(self.cursor);
        self.want_col = None;
    }

    pub fn word_right(&mut self, select: bool) {
        self.begin_move(select);
        self.cursor = self.word_right_of(self.cursor);
        self.want_col = None;
    }

    fn word_left_of(&self, p: Pos) -> Pos {
        if p.col == 0 {
            return if p.line > 0 {
                Pos::new(p.line - 1, self.line_len(p.line - 1))
            } else {
                p
            };
        }
        let chars: Vec<char> = self.lines[p.line].chars().collect();
        let mut i = p.col;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        if i > 0 {
            let class = char_class(chars[i - 1]);
            while i > 0 && char_class(chars[i - 1]) == class {
                i -= 1;
            }
        }
        Pos::new(p.line, i)
    }

    fn word_right_of(&self, p: Pos) -> Pos {
        let len = self.line_len(p.line);
        if p.col >= len {
            return if p.line + 1 < self.lines.len() {
                Pos::new(p.line + 1, 0)
            } else {
                p
            };
        }
        let chars: Vec<char> = self.lines[p.line].chars().collect();
        let mut i = p.col;
        while i < len && chars[i].is_whitespace() {
            i += 1;
        }
        if i < len {
            let class = char_class(chars[i]);
            while i < len && char_class(chars[i]) == class {
                i += 1;
            }
        }
        Pos::new(p.line, i)
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(Pos::default());
        self.cursor = self.end();
        self.last_kind = Kind::Other;
        self.follow = true;
    }

    /// Put the cursor somewhere without selecting, as a click or a jump does.
    pub fn set_cursor(&mut self, p: Pos, select: bool) {
        self.begin_move(select);
        self.cursor = self.clamp(p);
        self.want_col = None;
    }

    pub fn goto_line(&mut self, line: usize) {
        self.set_cursor(Pos::new(line.saturating_sub(1), 0), false);
    }

    // ---- searching -------------------------------------------------------

    /// Where `query` starts in a line, by character. Lower-case queries match
    /// either case; one with a capital in it matches exactly.
    pub fn matches_in_line(&self, line: usize, query: &str) -> Vec<usize> {
        let Some(l) = self.lines.get(line) else {
            return Vec::new();
        };
        if query.is_empty() {
            return Vec::new();
        }
        let exact = query.chars().any(char::is_uppercase);
        let fold = |c: char| {
            if exact {
                c
            } else {
                c.to_lowercase().next().unwrap_or(c)
            }
        };
        let hay: Vec<char> = l.chars().map(fold).collect();
        let needle: Vec<char> = query.chars().map(fold).collect();
        if needle.len() > hay.len() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i + needle.len() <= hay.len() {
            if hay[i..i + needle.len()] == needle[..] {
                out.push(i);
                i += needle.len();
            } else {
                i += 1;
            }
        }
        out
    }

    /// Select the next match of `query` after the cursor (or the previous one
    /// before it), going round the end of the file. Returns whether there was
    /// one.
    pub fn find(&mut self, query: &str, forward: bool) -> bool {
        if query.is_empty() || query.contains('\n') {
            return false;
        }
        let qlen = query.chars().count();
        let (sel_start, sel_end) = self.selection().unwrap_or((self.cursor, self.cursor));
        let n = self.lines.len();
        let hit = if forward {
            let from = sel_end;
            (0..=n).find_map(|k| {
                let line = (from.line + k) % n;
                let hits = self.matches_in_line(line, query);
                let pick = if k == 0 {
                    hits.into_iter().find(|&h| h >= from.col)
                } else if k == n {
                    hits.into_iter().find(|&h| h < from.col)
                } else {
                    hits.into_iter().next()
                };
                pick.map(|h| Pos::new(line, h))
            })
        } else {
            let from = sel_start;
            (0..=n).find_map(|k| {
                let line = (from.line + n * 2 - k) % n;
                let hits = self.matches_in_line(line, query);
                let pick = if k == 0 {
                    hits.into_iter().rev().find(|&h| h + qlen <= from.col)
                } else if k == n {
                    hits.into_iter().rev().find(|&h| h + qlen > from.col)
                } else {
                    hits.into_iter().next_back()
                };
                pick.map(|h| Pos::new(line, h))
            })
        };
        let Some(at) = hit else {
            return false;
        };
        self.anchor = Some(at);
        self.cursor = Pos::new(at.line, at.col + qlen);
        self.want_col = None;
        self.last_kind = Kind::Other;
        self.follow = true;
        true
    }

    // ---- the view --------------------------------------------------------

    /// Scroll just enough to have the cursor on screen.
    pub fn scroll_to_cursor(&mut self, height: usize, width: usize) {
        let height = height.max(1);
        let c = self.cursor;
        // A little context above and below, the way editors keep it.
        let margin = if height > 10 { 2 } else { 0 };
        if c.line < self.scroll + margin {
            self.scroll = c.line.saturating_sub(margin);
        } else if c.line + margin >= self.scroll + height {
            self.scroll = c.line + margin + 1 - height;
        }
        self.scroll = self.scroll.min(self.lines.len().saturating_sub(1));
        let dc = display_col(&self.lines[c.line], c.col);
        let width = width.max(1);
        if dc < self.hscroll {
            self.hscroll = dc.saturating_sub(width / 4);
        } else if dc >= self.hscroll + width {
            self.hscroll = dc + 1 - width + width / 4;
        }
    }

    /// The wheel: move the view, not the cursor.
    pub fn scroll_by(&mut self, delta: isize) {
        self.follow = false;
        self.scroll = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.lines.len().saturating_sub(1));
    }
}

/// The text of a file, whether it began with a byte order mark, and whether
/// its lines end in `\r\n`.
fn decode(bytes: &[u8]) -> Result<(String, bool, bool), String> {
    let (bom, body) = match bytes.strip_prefix(b"\xef\xbb\xbf") {
        Some(rest) => (true, rest),
        None => (false, bytes),
    };
    if body[..body.len().min(8192)].contains(&0) {
        return Err("a binary file".into());
    }
    let text = std::str::from_utf8(body)
        .map_err(|_| "not UTF-8 text".to_string())?
        .to_string();
    let crlf = text.contains("\r\n");
    Ok((text, bom, crlf))
}

fn split_lines(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
        .split('\n')
        .map(String::from)
        .collect()
}

/// A tab when most indented lines start with one; otherwise the smallest
/// run of spaces a line is indented by, two or four.
fn detect_indent(lines: &[String]) -> String {
    let (mut tabs, mut spaces, mut two) = (0, 0, 0);
    for l in lines.iter().take(2000) {
        if l.starts_with('\t') {
            tabs += 1;
        } else if l.starts_with(' ') && !l.trim().is_empty() {
            spaces += 1;
            let n = l.chars().take_while(|c| *c == ' ').count();
            if n == 2 {
                two += 1;
            }
        }
    }
    if tabs > spaces {
        "\t".to_string()
    } else if two * 3 > spaces && two > 0 {
        "  ".to_string()
    } else {
        " ".repeat(TAB)
    }
}

fn stamp_of(meta: &fs::Metadata) -> (Option<SystemTime>, u64) {
    (meta.modified().ok(), meta.len())
}

/// Where text inserted at `at` ends.
fn end_of(at: Pos, text: &str) -> Pos {
    match text.rfind('\n') {
        None => Pos::new(at.line, at.col + text.chars().count()),
        Some(i) => Pos::new(
            at.line + text.matches('\n').count(),
            text[i + 1..].chars().count(),
        ),
    }
}

fn byte_at(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

fn char_class(c: char) -> u8 {
    if c.is_alphanumeric() || c == '_' {
        2
    } else if c.is_whitespace() {
        0
    } else {
        1
    }
}

/// How many screen cells a character takes at screen column `at`.
pub fn char_width(c: char, at: usize) -> usize {
    if c == '\t' {
        TAB - at % TAB
    } else if c.is_control() {
        1
    } else {
        c.width().unwrap_or(1).max(1)
    }
}

/// The screen column the character at `col` starts at.
pub fn display_col(line: &str, col: usize) -> usize {
    let mut w = 0;
    for c in line.chars().take(col) {
        w += char_width(c, w);
    }
    w
}

/// The character under screen column `target`, or the line's end.
pub fn col_at_display(line: &str, target: usize) -> usize {
    let mut w = 0;
    for (i, c) in line.chars().enumerate() {
        let cw = char_width(c, w);
        if w + cw > target {
            // Past the middle of a wide cell counts as after it.
            return if target - w > cw / 2 && cw > 1 {
                i + 1
            } else {
                i
            };
        }
        w += cw;
    }
    line.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(b: &Buffer) -> (usize, usize) {
        (b.cursor.line, b.cursor.col)
    }

    #[test]
    fn typing_and_undo_go_a_word_at_a_time() {
        let mut b = Buffer::scratch("");
        for c in "fix the bug".chars() {
            b.insert(&c.to_string());
        }
        assert_eq!(b.text(), "fix the bug");
        assert!(b.dirty());
        b.undo();
        assert_eq!(b.text(), "fix the");
        b.undo();
        assert_eq!(b.text(), "fix");
        b.redo();
        assert_eq!(b.text(), "fix the");
        b.undo();
        b.undo();
        assert_eq!(b.text(), "");
        assert!(!b.dirty());
    }

    #[test]
    fn a_selection_is_replaced_by_what_is_typed() {
        let mut b = Buffer::scratch("hello world");
        b.set_cursor(Pos::new(0, 6), false);
        b.end_of_line(true);
        b.insert("there");
        assert_eq!(b.text(), "hello there");
        b.undo();
        assert_eq!(b.text(), "hello world");
    }

    #[test]
    fn enter_keeps_the_indent_and_opens_a_block() {
        let mut b = Buffer::scratch("    fn x() {}");
        b.set_cursor(Pos::new(0, 12), false);
        b.newline();
        assert_eq!(b.text(), "    fn x() {\n        \n    }");
        assert_eq!(at(&b), (1, 8));
        // One enter, one undo.
        b.undo();
        assert_eq!(b.text(), "    fn x() {}");
    }

    #[test]
    fn backspace_in_indentation_takes_a_level() {
        let mut b = Buffer::scratch("        x");
        b.set_cursor(Pos::new(0, 8), false);
        b.backspace();
        assert_eq!(b.text(), "    x");
        b.backspace();
        assert_eq!(b.text(), "x");
    }

    #[test]
    fn backspace_at_a_line_start_joins_it_up() {
        let mut b = Buffer::scratch("ab\ncd");
        b.set_cursor(Pos::new(1, 0), false);
        b.backspace();
        assert_eq!(b.text(), "abcd");
        assert_eq!(at(&b), (0, 2));
    }

    #[test]
    fn lines_move_indent_and_duplicate() {
        let mut b = Buffer::scratch("a\nb\nc");
        b.set_cursor(Pos::new(2, 0), false);
        b.move_lines(true);
        assert_eq!(b.text(), "a\nc\nb");
        assert_eq!(at(&b), (1, 0));
        b.duplicate();
        assert_eq!(b.text(), "a\nc\nc\nb");
        b.select_all();
        b.tab();
        assert_eq!(b.text(), "    a\n    c\n    c\n    b");
        b.backtab();
        assert_eq!(b.text(), "a\nc\nc\nb");
        b.set_cursor(Pos::new(0, 0), false);
        b.delete_lines();
        assert_eq!(b.text(), "c\nc\nb");
    }

    #[test]
    fn comments_toggle_both_ways() {
        let mut b = Buffer::scratch("  a\n  b");
        b.lang = syntax::detect(Path::new("x.rs"));
        b.select_all();
        b.toggle_comment();
        assert_eq!(b.text(), "  // a\n  // b");
        b.toggle_comment();
        assert_eq!(b.text(), "  a\n  b");
    }

    #[test]
    fn find_goes_round_the_end() {
        let mut b = Buffer::scratch("foo bar\nbar foo\nFOO");
        assert!(b.find("foo", true));
        assert_eq!(b.selection(), Some((Pos::new(0, 0), Pos::new(0, 3))));
        assert!(b.find("foo", true));
        assert_eq!(b.cursor, Pos::new(1, 7));
        assert!(b.find("foo", true));
        assert_eq!(b.cursor, Pos::new(2, 3));
        assert!(b.find("foo", true));
        assert_eq!(b.cursor, Pos::new(0, 3));
        assert!(b.find("foo", false));
        assert_eq!(b.cursor, Pos::new(2, 3));
        // A capital makes it exact.
        assert_eq!(b.matches_in_line(2, "Foo"), Vec::<usize>::new());
        assert_eq!(b.replace_all("bar", "baz"), 2);
        assert_eq!(b.text(), "foo baz\nbaz foo\nFOO");
        b.undo();
        assert_eq!(b.text(), "foo bar\nbar foo\nFOO");
    }

    #[test]
    fn up_and_down_keep_the_screen_column_over_tabs() {
        let mut b = Buffer::scratch("\tx\nabcdefgh\nab");
        b.set_cursor(Pos::new(0, 1), false);
        b.vertical(1, false);
        assert_eq!(at(&b), (1, 4));
        b.vertical(1, false);
        assert_eq!(at(&b), (2, 2));
        b.vertical(-1, false);
        assert_eq!(at(&b), (1, 4));
    }

    #[test]
    fn words_are_stepped_over_by_kind() {
        let mut b = Buffer::scratch("let x = foo.bar();");
        b.doc_end(false);
        b.word_left(false);
        assert_eq!(b.cursor.col, 15);
        b.word_left(false);
        assert_eq!(b.cursor.col, 12);
        b.delete_word_back();
        assert_eq!(b.text(), "let x = foobar();");
    }

    #[test]
    fn wide_characters_take_two_columns() {
        assert_eq!(display_col("a漢b", 2), 3);
        assert_eq!(col_at_display("a漢b", 3), 2);
        assert_eq!(display_col("\tx", 1), TAB);
    }

    #[test]
    fn a_saved_file_round_trips_its_line_endings() {
        let dir = std::env::temp_dir().join(format!("fleet-editor-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("crlf.txt");
        fs::write(&p, "a\r\nb\r\n").unwrap();
        let mut b = Buffer::open(&p).unwrap();
        assert!(b.crlf);
        assert_eq!(b.lines, vec!["a", "b", ""]);
        b.set_cursor(Pos::new(0, 1), false);
        b.insert("!");
        b.save().unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "a!\r\nb\r\n");
        assert!(!b.dirty());
        // Someone else writes it: nothing unsaved here, so it is read again.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&p, "changed\r\n").unwrap();
        assert_eq!(b.check_disk(), Disk::Reloaded);
        assert_eq!(b.text(), "changed\n");
        let _ = fs::remove_dir_all(&dir);
    }
}
