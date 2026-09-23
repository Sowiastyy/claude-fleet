//! The editor view: a file tree on the left, open files as tabs on the right,
//! and a one-line prompt for the things that need typing — find, replace,
//! go to line, quick open, new file, rename, delete.
//!
//! It takes the pane's place while it has the keyboard (`e` in the list,
//! `Alt+E` anywhere), and gives it back to the session when it is left. Open
//! files stay open in between, unsaved edits included.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::{
    clipimg,
    editor::{self, Buffer, Disk, Pos},
    files::{self, Tree},
};

/// How often the tree is listed again while on screen.
const TREE_REFRESH: Duration = Duration::from_secs(2);
/// How often open files are compared with the disk.
const DISK_CHECK: Duration = Duration::from_millis(800);
/// How many matches quick open lists.
pub const PICK_ROWS: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptKind {
    Find,
    /// Replace asks twice: what, then with what.
    Replace(Option<String>),
    Goto,
    /// Quick open, over every file under the root.
    Open,
    NewFile(PathBuf),
    Rename(PathBuf),
    Delete(PathBuf),
    /// A tab with unsaved edits is being closed.
    CloseDirty(usize),
    /// Saving over a file that changed on disk since it was read.
    Overwrite,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub input: String,
    /// Quick open: the files under the root, and the row picked.
    pub files: Vec<String>,
    pub pick: usize,
}

impl Prompt {
    fn new(kind: PromptKind, input: String) -> Self {
        Self {
            kind,
            input,
            files: Vec::new(),
            pick: 0,
        }
    }

    /// What the prompt says before the typed text.
    pub fn label(&self) -> String {
        match &self.kind {
            PromptKind::Find => "find (enter next, shift+enter previous):".into(),
            PromptKind::Replace(None) => "replace:".into(),
            PromptKind::Replace(Some(q)) => format!("replace \"{q}\" with:"),
            PromptKind::Goto => "go to line:".into(),
            PromptKind::Open => "open file:".into(),
            PromptKind::NewFile(_) => "new file (end with / for a folder):".into(),
            PromptKind::Rename(_) => "rename to:".into(),
            PromptKind::Delete(p) => format!(
                "delete {}? y deletes, anything else keeps it",
                p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            ),
            PromptKind::CloseDirty(_) => {
                "unsaved changes — s saves and closes, d discards them, esc keeps the tab".into()
            }
            PromptKind::Overwrite => {
                "changed on disk since it was opened — o overwrites it, r takes the disk's version, esc cancels".into()
            }
        }
    }

    /// Whether the prompt is a question answered by one key, not a line to
    /// type.
    pub fn is_question(&self) -> bool {
        matches!(
            self.kind,
            PromptKind::Delete(_) | PromptKind::CloseDirty(_) | PromptKind::Overwrite
        )
    }

    /// Quick open's matches, best first.
    pub fn matches(&self) -> Vec<&str> {
        let mut scored: Vec<(i64, &str)> = self
            .files
            .iter()
            .filter_map(|f| files::fuzzy_score(f, &self.input).map(|s| (s, f.as_str())))
            .collect();
        scored.sort();
        scored.into_iter().take(PICK_ROWS).map(|(_, f)| f).collect()
    }
}

/// Where the parts the mouse can hit were drawn, as of the last frame.
#[derive(Default)]
pub struct Areas {
    /// The whole view, less its outermost columns, which are the borders the
    /// mouse drags to resize the columns.
    pub all: Rect,
    pub tree: Rect,
    /// The text, gutter excluded.
    pub text: Rect,
    pub gutter: Rect,
    pub tabs: Vec<(Rect, usize)>,
}

/// What the app should do after a key or a click here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Stay,
    /// Give the pane back and the keyboard to the list.
    Leave,
}

pub struct Ide {
    pub tree: Tree,
    pub buffers: Vec<Buffer>,
    pub active: usize,
    /// The keyboard is in the text rather than the tree.
    pub editing: bool,
    pub prompt: Option<Prompt>,
    /// The last thing searched for, highlighted wherever it shows.
    pub query: String,
    /// What was copied last, for when the system clipboard cannot be read.
    clipboard: String,
    /// What should go on the status line.
    pub message: Option<String>,
    pub areas: Areas,
    /// The left button went down in the text and is still down.
    dragging: bool,
    last_tree_refresh: Instant,
    last_disk_check: Instant,
}

impl Ide {
    pub fn new(root: PathBuf) -> Self {
        Self {
            tree: Tree::new(root),
            buffers: Vec::new(),
            active: 0,
            editing: false,
            prompt: None,
            query: String::new(),
            clipboard: String::new(),
            message: None,
            areas: Areas::default(),
            dragging: false,
            last_tree_refresh: Instant::now(),
            last_disk_check: Instant::now(),
        }
    }

    pub fn buffer(&self) -> Option<&Buffer> {
        self.buffers.get(self.active)
    }

    fn say(&mut self, msg: impl Into<String>) {
        self.message = Some(msg.into());
    }

    /// Files with edits not on disk.
    pub fn unsaved(&self) -> Vec<String> {
        self.buffers
            .iter()
            .filter(|b| b.dirty())
            .map(Buffer::name)
            .collect()
    }

    /// Typing goes in as text rather than as commands: into a file or a
    /// prompt. The paste coalescer asks, since a letter in the tree is a key.
    pub fn takes_text(&self) -> bool {
        match &self.prompt {
            Some(p) => !p.is_question(),
            None => self.editing && !self.buffers.is_empty(),
        }
    }

    /// Open `path` in a tab, or go to its tab when it is open already.
    pub fn open(&mut self, path: &Path) -> bool {
        if let Some(i) = self.buffers.iter().position(|b| b.path == path) {
            self.active = i;
            self.editing = true;
            return true;
        }
        match Buffer::open(path) {
            Ok(b) => {
                self.buffers.push(b);
                self.active = self.buffers.len() - 1;
                self.editing = true;
                true
            }
            Err(e) => {
                let name = path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                self.say(format!("{name}: {e}"));
                false
            }
        }
    }

    fn close_tab(&mut self, i: usize) {
        if i >= self.buffers.len() {
            return;
        }
        self.buffers.remove(i);
        if self.active > i || self.active >= self.buffers.len() {
            self.active = self.active.saturating_sub(1);
        }
        if self.buffers.is_empty() {
            self.editing = false;
        }
    }

    /// Ask before closing a tab that holds unsaved edits.
    fn request_close(&mut self, i: usize) {
        match self.buffers.get(i) {
            Some(b) if b.dirty() => {
                self.active = i;
                self.prompt = Some(Prompt::new(PromptKind::CloseDirty(i), String::new()));
            }
            Some(_) => self.close_tab(i),
            None => {}
        }
    }

    fn save(&mut self, force: bool) {
        let Some(b) = self.buffers.get_mut(self.active) else {
            return;
        };
        if b.changed_on_disk && !force {
            self.prompt = Some(Prompt::new(PromptKind::Overwrite, String::new()));
            return;
        }
        let name = b.name();
        match b.save() {
            Ok(()) => self.say(format!("saved {name}")),
            Err(e) => self.say(format!("could not save {name}: {e}")),
        }
    }

    /// Per-frame upkeep: list the tree again now and then, and read files
    /// again that changed on disk. Returns whether anything on screen moved.
    pub fn tick(&mut self, visible: bool) -> bool {
        let mut changed = false;
        if visible && self.last_tree_refresh.elapsed() >= TREE_REFRESH {
            self.last_tree_refresh = Instant::now();
            self.tree.refresh();
            changed = true;
        }
        if self.last_disk_check.elapsed() >= DISK_CHECK {
            self.last_disk_check = Instant::now();
            let mut notes = Vec::new();
            for b in &mut self.buffers {
                match b.check_disk() {
                    Disk::Same => {}
                    Disk::Reloaded => {
                        changed = true;
                        if visible {
                            notes.push(format!("{} changed on disk — reloaded", b.name()));
                        }
                    }
                    Disk::Conflict => {
                        changed = true;
                        notes.push(format!(
                            "{} changed on disk under your unsaved edits — ctrl+s asks what to keep",
                            b.name()
                        ));
                    }
                    Disk::Gone => {
                        changed = true;
                        notes.push(format!("{} was deleted on disk", b.name()));
                    }
                }
            }
            if let Some(n) = notes.pop() {
                self.say(n);
            }
        }
        changed
    }

    // ---- keys ------------------------------------------------------------

    pub fn key(&mut self, key: KeyEvent, page: usize) -> Outcome {
        if self.prompt.is_some() {
            self.prompt_key(key);
            return Outcome::Stay;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Wherever the keyboard is, these mean the same.
        match key.code {
            KeyCode::Char('p') if ctrl => {
                self.start_open();
                return Outcome::Stay;
            }
            KeyCode::Char('s') if ctrl => {
                self.save(false);
                return Outcome::Stay;
            }
            KeyCode::Char('w') if ctrl => {
                self.request_close(self.active);
                return Outcome::Stay;
            }
            KeyCode::PageUp if ctrl => {
                self.cycle_tab(-1);
                return Outcome::Stay;
            }
            KeyCode::PageDown if ctrl => {
                self.cycle_tab(1);
                return Outcome::Stay;
            }
            _ => {}
        }
        if self.editing && !self.buffers.is_empty() {
            self.edit_key(key, page);
            Outcome::Stay
        } else {
            self.tree_key(key, page)
        }
    }

    fn cycle_tab(&mut self, delta: isize) {
        let n = self.buffers.len();
        if n == 0 {
            return;
        }
        self.active = (self.active as isize + delta).rem_euclid(n as isize) as usize;
        self.editing = true;
    }

    fn start_open(&mut self) {
        let mut p = Prompt::new(PromptKind::Open, String::new());
        p.files = files::all_files(&self.tree.root);
        self.prompt = Some(p);
    }

    fn tree_key(&mut self, key: KeyEvent, page: usize) -> Outcome {
        let page = page.max(1) as isize;
        self.tree.follow = true;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('e') => return Outcome::Leave,
            KeyCode::Char('j') | KeyCode::Down => self.tree.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.tree.move_cursor(-1),
            KeyCode::PageDown => self.tree.move_cursor(page),
            KeyCode::PageUp => self.tree.move_cursor(-page),
            KeyCode::Home => self.tree.move_cursor(isize::MIN / 2),
            KeyCode::End => self.tree.move_cursor(isize::MAX / 2),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(path) = self.tree.activate() {
                    self.open(&path);
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                if let Some(path) = self.tree.open() {
                    self.open(&path);
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.tree.close();
            }
            KeyCode::Tab => {
                if self.buffers.is_empty() {
                    self.say("no file open — enter opens the one under the cursor");
                } else {
                    self.editing = true;
                }
            }
            KeyCode::Char('a') | KeyCode::Char('n') => {
                let dir = self.tree.target_dir();
                self.prompt = Some(Prompt::new(PromptKind::NewFile(dir), String::new()));
            }
            KeyCode::Char('r') => {
                if let Some(row) = self.tree.selected() {
                    self.prompt = Some(Prompt::new(PromptKind::Rename(row.path), row.name));
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(row) = self.tree.selected() {
                    self.prompt = Some(Prompt::new(PromptKind::Delete(row.path), String::new()));
                }
            }
            KeyCode::Char('R') => {
                self.tree.refresh();
                self.say("tree read again");
            }
            KeyCode::Char('y') => {
                if let Some(row) = self.tree.selected() {
                    let shown = row.path.display().to_string();
                    if clipimg::copy_text(&shown) {
                        self.say(format!("copied {shown}"));
                    }
                }
            }
            KeyCode::Char('/') | KeyCode::Char('f') => self.start_open(),
            _ => {}
        }
        Outcome::Stay
    }

    fn edit_key(&mut self, key: KeyEvent, page: usize) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page = page.max(1) as isize;

        // Keys that open a prompt or reach outside the buffer.
        match key.code {
            KeyCode::Esc => {
                let b = &mut self.buffers[self.active];
                if b.anchor.is_some() {
                    b.anchor = None;
                } else {
                    self.editing = false;
                    if let Some(path) = self.buffer().map(|b| b.path.clone()) {
                        self.tree.reveal(&path);
                    }
                }
                return;
            }
            KeyCode::Char('f') if ctrl => {
                let seed = self
                    .buffer()
                    .and_then(Buffer::selected_text)
                    .filter(|s| !s.contains('\n'))
                    .unwrap_or_else(|| self.query.clone());
                self.prompt = Some(Prompt::new(PromptKind::Find, seed));
                return;
            }
            KeyCode::Char('h' | 'r') if ctrl => {
                let seed = self
                    .buffer()
                    .and_then(Buffer::selected_text)
                    .filter(|s| !s.contains('\n'))
                    .unwrap_or_else(|| self.query.clone());
                self.prompt = Some(Prompt::new(PromptKind::Replace(None), seed));
                return;
            }
            KeyCode::Char('g') if ctrl => {
                self.prompt = Some(Prompt::new(PromptKind::Goto, String::new()));
                return;
            }
            KeyCode::Left if alt => return self.cycle_tab(-1),
            KeyCode::Right if alt => return self.cycle_tab(1),
            KeyCode::Char('c') if ctrl => return self.copy(false),
            KeyCode::Char('x') if ctrl => return self.copy(true),
            KeyCode::Insert if ctrl => return self.copy(false),
            KeyCode::Char('v') if ctrl => {
                // The terminal pastes by itself and this only comes through
                // when it did not; the clipboard is read here instead.
                let text = clipimg::read_text().unwrap_or_else(|| self.clipboard.clone());
                return self.paste(&text);
            }
            _ => {}
        }

        let b = &mut self.buffers[self.active];
        match key.code {
            KeyCode::Char('z') if ctrl => {
                if !b.undo() {
                    self.say("nothing to undo");
                }
            }
            KeyCode::Char('y') if ctrl => {
                if !b.redo() {
                    self.say("nothing to redo");
                }
            }
            KeyCode::Char('Z') if ctrl => {
                b.redo();
            }
            KeyCode::Char('a') if ctrl => b.select_all(),
            KeyCode::Char('d') if ctrl => b.duplicate(),
            KeyCode::Char('k') if ctrl => b.delete_lines(),
            KeyCode::Char('/' | '_' | '7') if ctrl => {
                if !b.toggle_comment() {
                    let lang = b.lang.name;
                    self.say(format!("{lang} has no line comments"));
                }
            }
            KeyCode::Up if alt => b.move_lines(true),
            KeyCode::Down if alt => b.move_lines(false),
            KeyCode::Left if ctrl => b.word_left(shift),
            KeyCode::Right if ctrl => b.word_right(shift),
            KeyCode::Left => b.left(shift),
            KeyCode::Right => b.right(shift),
            KeyCode::Up if ctrl => b.scroll_by(-1),
            KeyCode::Down if ctrl => b.scroll_by(1),
            KeyCode::Up => b.vertical(-1, shift),
            KeyCode::Down => b.vertical(1, shift),
            KeyCode::PageUp => b.vertical(-page, shift),
            KeyCode::PageDown => b.vertical(page, shift),
            KeyCode::Home if ctrl => b.doc_start(shift),
            KeyCode::End if ctrl => b.doc_end(shift),
            KeyCode::Home => b.home(shift),
            KeyCode::End => b.end_of_line(shift),
            KeyCode::Enter => b.newline(),
            KeyCode::Tab | KeyCode::Char('\t') => b.tab(),
            KeyCode::BackTab => b.backtab(),
            KeyCode::Backspace if ctrl || alt => b.delete_word_back(),
            KeyCode::Backspace => b.backspace(),
            KeyCode::Delete if ctrl => b.delete_word_forward(),
            KeyCode::Delete => b.delete(),
            KeyCode::Char(c) if !ctrl && !alt => b.insert(c.encode_utf8(&mut [0; 4])),
            _ => {}
        }
    }

    /// Ctrl+C / Ctrl+X: the selection, or the whole line when nothing is
    /// selected, the way most editors do it.
    fn copy(&mut self, cut: bool) {
        let b = &mut self.buffers[self.active];
        let (text, whole_line) = match b.selected_text() {
            Some(t) => (t, false),
            None => (format!("{}\n", b.lines[b.cursor.line]), true),
        };
        if cut {
            if whole_line {
                b.delete_lines();
            } else {
                b.delete();
            }
        }
        let lines = text.lines().count().max(1);
        self.clipboard = text.clone();
        if clipimg::copy_text(&text) {
            self.say(format!(
                "{} {lines} line{}",
                if cut { "cut" } else { "copied" },
                if lines == 1 { "" } else { "s" }
            ));
        }
    }

    /// Text from a paste goes in as it is: no indenting, no key handling.
    pub fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if let Some(p) = self.prompt.as_mut() {
            if !p.is_question() {
                p.input.push_str(text.lines().next().unwrap_or(""));
                p.pick = 0;
            }
            return;
        }
        if !self.editing {
            return;
        }
        if let Some(b) = self.buffers.get_mut(self.active) {
            b.insert(&text);
        }
    }

    fn prompt_key(&mut self, key: KeyEvent) {
        let Some(p) = self.prompt.as_mut() else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if p.is_question() {
            let kind = p.kind.clone();
            self.prompt = None;
            self.answer(kind, key.code);
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.prompt = None;
            }
            KeyCode::Enter => {
                let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                self.submit(shift);
            }
            KeyCode::Backspace if ctrl => {
                let mut cur = p.input.len();
                crate::msgedit::delete_word_back(&mut p.input, &mut cur);
                p.pick = 0;
            }
            KeyCode::Backspace => {
                p.input.pop();
                p.pick = 0;
            }
            KeyCode::Up if p.kind == PromptKind::Open => p.pick = p.pick.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab if p.kind == PromptKind::Open => {
                let n = p.matches().len();
                p.pick = (p.pick + 1).min(n.saturating_sub(1));
            }
            KeyCode::Char('u') if ctrl => p.input.clear(),
            KeyCode::Char(c) if !ctrl => {
                p.input.push(c);
                p.pick = 0;
            }
            _ => {}
        }
    }

    /// Enter in a prompt.
    fn submit(&mut self, shift: bool) {
        let Some(p) = self.prompt.take() else {
            return;
        };
        let input = p.input.clone();
        match p.kind {
            PromptKind::Find => {
                self.query = input.clone();
                let Some(b) = self.buffers.get_mut(self.active) else {
                    return;
                };
                if !b.find(&input, !shift) {
                    self.say(format!("\"{input}\" is not in this file"));
                }
                // Stays open, so enter goes on to the next one.
                self.prompt = Some(p);
            }
            PromptKind::Replace(None) => {
                if input.is_empty() {
                    return;
                }
                self.query = input.clone();
                self.prompt = Some(Prompt::new(PromptKind::Replace(Some(input)), String::new()));
            }
            PromptKind::Replace(Some(q)) => {
                let Some(b) = self.buffers.get_mut(self.active) else {
                    return;
                };
                let n = b.replace_all(&q, &input);
                self.say(match n {
                    0 => format!("\"{q}\" is not in this file"),
                    1 => "replaced 1 match — ctrl+z takes it back".to_string(),
                    n => format!("replaced {n} matches — ctrl+z takes them back"),
                });
            }
            PromptKind::Goto => match input.trim().parse::<usize>() {
                Ok(n) if n > 0 => {
                    if let Some(b) = self.buffers.get_mut(self.active) {
                        b.goto_line(n);
                    }
                }
                _ => self.say("a line number, counting from 1"),
            },
            PromptKind::Open => {
                let picked = p.matches().get(p.pick).map(|s| s.to_string());
                if let Some(rel) = picked {
                    let path = self.tree.root.join(rel);
                    if self.open(&path) {
                        self.tree.reveal(&path);
                    }
                }
            }
            PromptKind::NewFile(dir) => self.create(&dir, input.trim()),
            PromptKind::Rename(from) => self.rename(&from, input.trim()),
            PromptKind::Delete(_) | PromptKind::CloseDirty(_) | PromptKind::Overwrite => {}
        }
    }

    /// A one-key answer to a question prompt.
    fn answer(&mut self, kind: PromptKind, code: KeyCode) {
        match (kind, code) {
            (PromptKind::Delete(path), KeyCode::Char('y')) => self.delete(&path),
            (PromptKind::Delete(_), _) => self.say("kept"),
            (PromptKind::CloseDirty(i), KeyCode::Char('s')) => {
                self.active = i;
                self.save(false);
                if self.buffers.get(i).is_some_and(|b| !b.dirty()) {
                    self.close_tab(i);
                }
            }
            (PromptKind::CloseDirty(i), KeyCode::Char('d')) => {
                let name = self.buffers.get(i).map(Buffer::name).unwrap_or_default();
                self.close_tab(i);
                self.say(format!("closed {name} without saving"));
            }
            (PromptKind::Overwrite, KeyCode::Char('o')) => self.save(true),
            (PromptKind::Overwrite, KeyCode::Char('r')) => {
                if let Some(b) = self.buffers.get_mut(self.active) {
                    let name = b.name();
                    match b.reload() {
                        Ok(_) => self.say(format!("{name}: took the version on disk")),
                        Err(e) => self.say(format!("{name}: {e}")),
                    }
                }
            }
            _ => {}
        }
    }

    fn create(&mut self, dir: &Path, name: &str) {
        if name.is_empty() {
            return;
        }
        let folder = name.ends_with(['/', '\\']);
        let path = dir.join(name.trim_end_matches(['/', '\\']));
        if path.exists() {
            self.say(format!("{} is there already", path.display()));
            return;
        }
        let result = if folder {
            std::fs::create_dir_all(&path)
        } else {
            path.parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, ""))
        };
        match result {
            Ok(()) => {
                self.tree.refresh();
                self.tree.reveal(&path);
                if !folder {
                    self.open(&path);
                }
            }
            Err(e) => self.say(format!("could not create it: {e}")),
        }
    }

    fn rename(&mut self, from: &Path, name: &str) {
        if name.is_empty() {
            return;
        }
        let Some(parent) = from.parent() else {
            return;
        };
        let to = parent.join(name);
        if to == from {
            return;
        }
        if to.exists() {
            self.say(format!("{name} is there already"));
            return;
        }
        match std::fs::rename(from, &to) {
            Ok(()) => {
                // Open files inside it move with it.
                for b in &mut self.buffers {
                    if let Ok(rest) = b.path.strip_prefix(from) {
                        b.path = to.join(rest);
                        b.lang = crate::syntax::detect(&b.path);
                    }
                }
                self.tree.refresh();
                self.tree.reveal(&to);
            }
            Err(e) => self.say(format!("could not rename it: {e}")),
        }
    }

    fn delete(&mut self, path: &Path) {
        let result = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        match result {
            Ok(()) => {
                let name = path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                // Its tabs go with it, unsaved edits and all: the question
                // was asked.
                while let Some(i) = self.buffers.iter().position(|b| b.path.starts_with(path)) {
                    self.close_tab(i);
                }
                self.tree.refresh();
                self.say(format!("deleted {name}"));
            }
            Err(e) => self.say(format!("could not delete it: {e}")),
        }
    }

    // ---- the mouse -------------------------------------------------------

    /// A mouse event over the pane. Returns whether it landed on something
    /// here.
    pub fn mouse(&mut self, m: MouseEvent) -> bool {
        let at = Position::new(m.column, m.row);
        let in_text = self.areas.text.contains(at) || self.areas.gutter.contains(at);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(&(_, i)) = self.areas.tabs.iter().find(|(r, _)| r.contains(at)) {
                    self.active = i;
                    self.editing = true;
                    return true;
                }
                if self.areas.tree.contains(at) {
                    let row = self.tree.scroll + usize::from(m.row - self.areas.tree.y);
                    let was = self.tree.cursor.clone();
                    if let Some(r) = self.tree.select_index(row) {
                        if r.dir {
                            // A click on the folder already picked opens it;
                            // the first only picks it, so it can be renamed.
                            if was.as_ref() == Some(&r.path) || !r.open {
                                self.tree.activate();
                            }
                        } else {
                            self.open(&r.path);
                        }
                    }
                    if self.tree.selected().is_some_and(|r| r.dir) {
                        self.editing = false;
                    }
                    return true;
                }
                if in_text && !self.buffers.is_empty() {
                    let p = self.text_pos(m.column, m.row);
                    let shift = m.modifiers.contains(KeyModifiers::SHIFT);
                    self.buffers[self.active].set_cursor(p, shift);
                    self.editing = true;
                    self.dragging = true;
                    return true;
                }
                // Anywhere else in the view is still the view: a click on an
                // empty editor must not hand the keyboard to the session.
                self.areas.all.contains(at)
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                let p = self.text_pos(m.column, m.row);
                if let Some(b) = self.buffers.get_mut(self.active) {
                    b.set_cursor(p, true);
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) if self.dragging => {
                self.dragging = false;
                true
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let delta = if m.kind == MouseEventKind::ScrollUp {
                    -3
                } else {
                    3
                };
                if self.areas.tree.contains(at) {
                    self.tree.scroll = self.tree.scroll.saturating_add_signed(delta);
                    self.tree.follow = false;
                    return true;
                }
                if !self.areas.all.contains(at) {
                    return false;
                }
                if let Some(b) = self.buffers.get_mut(self.active) {
                    b.scroll_by(delta);
                }
                true
            }
            _ => false,
        }
    }

    /// The buffer position under a screen cell of the text area, clamped to
    /// it, so a drag past an edge keeps selecting.
    fn text_pos(&self, x: u16, y: u16) -> Pos {
        let Some(b) = self.buffer() else {
            return Pos::default();
        };
        let t = self.areas.text;
        let row = y.clamp(t.y, t.bottom().saturating_sub(1)) - t.y;
        let line = (b.scroll + usize::from(row)).min(b.lines.len() - 1);
        let col = if x < t.x {
            0
        } else {
            let dx = usize::from(x.min(t.right().saturating_sub(1)) - t.x);
            editor::col_at_display(&b.lines[line], b.hscroll + dx)
        };
        Pos::new(line, col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn sandbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fleet-ide-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        dir
    }

    #[test]
    fn open_edit_save_from_the_keyboard() {
        let dir = sandbox("edit");
        let mut ide = Ide::new(dir.clone());
        // src, then enter opens it; down to main.rs, enter opens the file.
        ide.key(key(KeyCode::Enter), 10);
        ide.key(key(KeyCode::Down), 10);
        ide.key(key(KeyCode::Enter), 10);
        assert!(ide.editing);
        assert_eq!(ide.buffers.len(), 1);
        for c in "// hi".chars() {
            ide.key(key(KeyCode::Char(c)), 10);
        }
        ide.key(key(KeyCode::Enter), 10);
        assert_eq!(ide.unsaved(), vec!["main.rs"]);
        ide.key(ctrl('s'), 10);
        assert!(ide.unsaved().is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.join("src/main.rs")).unwrap(),
            "// hi\nfn main() {}\n"
        );
        // Esc leaves the text for the tree, and the tree's esc leaves.
        ide.key(key(KeyCode::Esc), 10);
        assert!(!ide.editing);
        assert_eq!(ide.key(key(KeyCode::Esc), 10), Outcome::Leave);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closing_a_dirty_tab_asks_first() {
        let dir = sandbox("close");
        let mut ide = Ide::new(dir.clone());
        assert!(ide.open(&dir.join("src/main.rs")));
        ide.key(key(KeyCode::Char('x')), 10);
        ide.key(ctrl('w'), 10);
        assert!(matches!(
            ide.prompt.as_ref().map(|p| &p.kind),
            Some(PromptKind::CloseDirty(0))
        ));
        ide.key(key(KeyCode::Char('d')), 10);
        assert!(ide.buffers.is_empty());
        // The file on disk never saw the edit.
        assert_eq!(
            std::fs::read_to_string(dir.join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_files_and_folders_from_the_tree() {
        let dir = sandbox("new");
        let mut ide = Ide::new(dir.clone());
        ide.key(key(KeyCode::Char('a')), 10);
        for c in "docs/".chars() {
            ide.key(key(KeyCode::Char(c)), 10);
        }
        ide.key(key(KeyCode::Enter), 10);
        // The cursor was on `src`, so the folder went into it.
        assert!(dir.join("src/docs").is_dir());
        ide.key(key(KeyCode::Char('a')), 10);
        for c in "notes.md".chars() {
            ide.key(key(KeyCode::Char(c)), 10);
        }
        ide.key(key(KeyCode::Enter), 10);
        // And now it is on the new folder.
        assert!(dir.join("src/docs/notes.md").is_file());
        assert!(ide.editing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quick_open_takes_the_best_match() {
        let dir = sandbox("quick");
        let mut ide = Ide::new(dir.clone());
        ide.key(ctrl('p'), 10);
        for c in "mai".chars() {
            ide.key(key(KeyCode::Char(c)), 10);
        }
        ide.key(key(KeyCode::Enter), 10);
        assert_eq!(ide.buffer().map(Buffer::name), Some("main.rs".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
