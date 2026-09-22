//! The git panel as something one can move around in: which row the cursor is
//! on, which commits are opened up to show their files, and the commit
//! details and diffs fetched for them.
//!
//! The rows are rebuilt from the latest snapshot on every use rather than
//! kept, so a commit a session makes shows up without anything being told.
//! The cursor is held as the row's identity, not its index, so it stays on the
//! same file or commit while the rows around it come and go.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

use crate::git::{self, CommitDetail, DiffSource, Snapshot};

/// Commit details kept per repository. They never change, so this is only
/// about memory; a few hundred is more than anyone opens in one sitting.
const DETAIL_CACHE: usize = 300;

/// One row of the panel's list.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Row {
    /// The heading over the uncommitted changes; opens and closes them.
    Changes,
    Change(String),
    /// Changes that did not fit, summed up.
    More(usize),
    Gap,
    /// The heading over the history; opens and closes it.
    History,
    Commit(String),
    /// A file one opened commit touched: the commit's hash, then the path.
    CommitFile(String, String),
    /// An opened commit whose files are still being fetched.
    Loading(String),
}

impl Row {
    /// Rows the cursor stops on. The rest are there to be read.
    pub fn selectable(&self) -> bool {
        !matches!(self, Row::More(_) | Row::Gap | Row::Loading(_))
    }
}

/// What the loader thread hands back.
enum Loaded {
    Detail(PathBuf, String, Option<CommitDetail>),
    Diff(PathBuf, DiffSource, Vec<String>),
}

pub struct GitView {
    pub cursor: Option<Row>,
    /// Where the cursor was in the list, for when its row disappears: the
    /// cursor lands on whatever took that place.
    last_index: usize,
    pub changes_open: bool,
    pub history_open: bool,
    /// Commits opened up to show their files.
    pub expanded: HashSet<String>,
    pub details: HashMap<String, CommitDetail>,
    /// Commits git had nothing to say about, so they are not asked again.
    failed: HashSet<String>,
    /// The diff on show, and what it is of.
    pub diff: Option<(DiffSource, Vec<String>)>,
    /// Whether the pane shows the preview instead of the session. It opens
    /// only when a change or a commit is entered, so putting the keyboard on
    /// the panel does not take the session off the screen.
    pub preview_open: bool,
    /// First line of the preview on screen.
    pub preview_scroll: usize,
    /// First row of the list on screen.
    pub list_scroll: usize,
    /// The repository all of the above belongs to.
    root: Option<PathBuf>,
    busy: Arc<AtomicBool>,
    tx: mpsc::Sender<Loaded>,
    rx: mpsc::Receiver<Loaded>,
}

impl GitView {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            cursor: None,
            last_index: 0,
            changes_open: true,
            history_open: true,
            expanded: HashSet::new(),
            details: HashMap::new(),
            failed: HashSet::new(),
            diff: None,
            preview_open: false,
            preview_scroll: 0,
            list_scroll: 0,
            root: None,
            busy: Arc::new(AtomicBool::new(false)),
            tx,
            rx,
        }
    }

    /// The list for a snapshot. `change_limit` caps the changes shown, for the
    /// panel at rest; a focused panel lists every one and scrolls.
    pub fn rows(&self, snap: &Snapshot, change_limit: Option<usize>) -> Vec<Row> {
        let mut rows = vec![Row::Changes];
        if self.changes_open {
            let limit = change_limit.unwrap_or(usize::MAX).max(1);
            let shown = if snap.changes_total > limit {
                limit - 1
            } else {
                limit
            };
            rows.extend(
                snap.changes
                    .iter()
                    .take(shown)
                    .map(|c| Row::Change(c.path.clone())),
            );
            if snap.changes_total > shown {
                rows.push(Row::More(snap.changes_total - shown));
            }
        }
        rows.push(Row::Gap);
        rows.push(Row::History);
        if self.history_open {
            for c in &snap.log {
                rows.push(Row::Commit(c.hash.clone()));
                if !self.expanded.contains(&c.hash) {
                    continue;
                }
                match self.details.get(&c.hash) {
                    Some(d) => rows.extend(
                        d.files
                            .iter()
                            .map(|f| Row::CommitFile(c.hash.clone(), f.path.clone())),
                    ),
                    None if self.failed.contains(&c.hash) => {}
                    None => rows.push(Row::Loading(c.hash.clone())),
                }
            }
        }
        rows
    }

    /// The cursor's place in `rows`, putting it on a row that exists if the
    /// one it was on has gone.
    pub fn cursor_index(&mut self, rows: &[Row]) -> usize {
        if let Some(i) = self
            .cursor
            .as_ref()
            .and_then(|c| rows.iter().position(|r| r == c))
        {
            self.last_index = i;
            return i;
        }
        // Gone, or never set: the nearest selectable row at or above where
        // it used to be.
        let start = self.last_index.min(rows.len().saturating_sub(1));
        let i = (0..=start)
            .rev()
            .chain(start + 1..rows.len())
            .find(|&i| rows[i].selectable())
            .unwrap_or(0);
        self.set_cursor(rows.get(i).cloned());
        self.last_index = i;
        i
    }

    fn set_cursor(&mut self, row: Option<Row>) {
        if self.cursor != row {
            self.cursor = row;
            self.preview_scroll = 0;
        }
    }

    /// Move by `delta` selectable rows, stopping at either end. Returns
    /// whether the cursor moved at all.
    pub fn move_cursor(&mut self, snap: &Snapshot, delta: isize) -> bool {
        let rows = self.rows(snap, None);
        let mut i = self.cursor_index(&rows);
        let mut left = delta.unsigned_abs();
        while left > 0 {
            let next = if delta < 0 {
                (0..i).rev().find(|&j| rows[j].selectable())
            } else {
                (i + 1..rows.len()).find(|&j| rows[j].selectable())
            };
            match next {
                Some(j) => i = j,
                None => break,
            }
            left -= 1;
        }
        let moved = rows.get(i) != self.cursor.as_ref();
        self.last_index = i;
        self.set_cursor(rows.get(i).cloned());
        moved
    }

    pub fn home(&mut self, snap: &Snapshot) {
        self.move_cursor(snap, -(isize::MAX));
    }

    pub fn end(&mut self, snap: &Snapshot) {
        self.move_cursor(snap, isize::MAX);
    }

    /// Enter or space: open or close what the cursor is on. Entering a
    /// change or a commit puts its preview on the pane.
    pub fn toggle(&mut self) {
        match self.cursor.clone() {
            Some(Row::Changes) => self.changes_open = !self.changes_open,
            Some(Row::History) => self.history_open = !self.history_open,
            // The first enter shows the commit; the next ones fold it.
            Some(Row::Commit(h)) if self.preview_open && self.expanded.contains(&h) => {
                self.expanded.remove(&h);
            }
            Some(Row::Commit(h)) => {
                self.expanded.insert(h);
                self.preview_open = true;
            }
            Some(Row::Change(_) | Row::CommitFile(..)) => self.preview_open = true,
            _ => {}
        }
    }

    /// Right: open what the cursor is on.
    pub fn open(&mut self) {
        match self.cursor.clone() {
            Some(Row::Changes) => self.changes_open = true,
            Some(Row::History) => self.history_open = true,
            Some(Row::Commit(h)) => {
                self.expanded.insert(h);
                self.preview_open = true;
            }
            Some(Row::Change(_) | Row::CommitFile(..)) => self.preview_open = true,
            _ => {}
        }
    }

    /// Left: close the preview, then what the cursor is on, or climb to the
    /// commit a file belongs to. Returns false when there was nothing to
    /// close, which is the signal to leave the panel.
    pub fn close(&mut self) -> bool {
        if self.preview_open {
            self.preview_open = false;
            return true;
        }
        match self.cursor.clone() {
            Some(Row::Changes) if self.changes_open => self.changes_open = false,
            Some(Row::History) if self.history_open => self.history_open = false,
            Some(Row::Commit(h)) if self.expanded.contains(&h) => {
                self.expanded.remove(&h);
            }
            Some(Row::CommitFile(h, _)) => self.set_cursor(Some(Row::Commit(h))),
            Some(Row::Change(_)) => self.set_cursor(Some(Row::Changes)),
            _ => return false,
        }
        true
    }

    pub fn scroll_preview(&mut self, delta: isize) {
        self.preview_scroll = self.preview_scroll.saturating_add_signed(delta);
    }

    /// What the preview should show a diff of, for the row under the cursor.
    pub fn wanted_diff(&self, snap: &Snapshot) -> Option<DiffSource> {
        match self.cursor.as_ref()? {
            Row::Change(path) => {
                let c = snap.changes.iter().find(|c| &c.path == path)?;
                Some(DiffSource::Worktree {
                    path: path.clone(),
                    untracked: c.untracked(),
                    has_head: snap.has_head(),
                })
            }
            Row::CommitFile(hash, path) => Some(DiffSource::Commit {
                hash: hash.clone(),
                path: path.clone(),
            }),
            _ => None,
        }
    }

    /// The diff for `src`, if it is the one loaded.
    pub fn diff_for(&self, src: &DiffSource) -> Option<&[String]> {
        self.diff
            .as_ref()
            .filter(|(s, _)| s == src)
            .map(|(_, lines)| lines.as_slice())
    }

    /// The uncommitted changes moved: a diff of them is out of date.
    pub fn worktree_changed(&mut self) {
        if matches!(self.diff, Some((DiffSource::Worktree { .. }, _))) {
            self.diff = None;
        }
    }

    /// Take in what the loader finished, and start the next load the cursor
    /// or an opened commit needs. Returns whether anything on screen changed.
    ///
    /// One load at a time: holding an arrow key down would otherwise start a
    /// git process for every row it passes. The cursor's diff goes first,
    /// since it is what one is looking at.
    pub fn poll(&mut self, snap: &Snapshot) -> bool {
        let mut changed = false;
        if self.root.as_ref() != Some(&snap.root) {
            // Another repository: nothing here describes it.
            let root = snap.root.clone();
            *self = Self::new();
            self.root = Some(root);
            changed = true;
        }
        while let Ok(loaded) = self.rx.try_recv() {
            match loaded {
                Loaded::Detail(root, hash, detail) if Some(&root) == self.root.as_ref() => {
                    match detail {
                        Some(d) => {
                            if self.details.len() >= DETAIL_CACHE {
                                self.details.clear();
                            }
                            self.details.insert(hash, d);
                        }
                        None => {
                            self.failed.insert(hash);
                        }
                    }
                    changed = true;
                }
                Loaded::Diff(root, src, lines) if Some(&root) == self.root.as_ref() => {
                    self.diff = Some((src, lines));
                    changed = true;
                }
                _ => {}
            }
        }

        if self.busy.load(Ordering::Relaxed) {
            return changed;
        }
        let root = snap.root.clone();
        if self.preview_open
            && let Some(src) = self.wanted_diff(snap)
            && self.diff_for(&src).is_none()
        {
            self.spawn(move |tx| {
                let lines = git::diff(&root, &src);
                let _ = tx.send(Loaded::Diff(root, src, lines));
            });
            return changed;
        }
        let cursor_commit = match &self.cursor {
            Some(Row::Commit(h)) => Some(h.clone()),
            _ => None,
        };
        let missing = cursor_commit
            .into_iter()
            .chain(self.expanded.iter().cloned())
            .find(|h| !self.details.contains_key(h) && !self.failed.contains(h));
        if let Some(hash) = missing {
            self.spawn(move |tx| {
                let detail = git::commit_detail(&root, &hash);
                let _ = tx.send(Loaded::Detail(root, hash, detail));
            });
        }
        changed
    }

    fn spawn(&self, job: impl FnOnce(&mpsc::Sender<Loaded>) + Send + 'static) {
        self.busy.store(true, Ordering::Relaxed);
        let busy = Arc::clone(&self.busy);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            job(&tx);
            busy.store(false, Ordering::Relaxed);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{Change, Commit, FileStat};

    fn snap() -> Snapshot {
        Snapshot {
            root: "C:/repo".into(),
            branch: Some("main".into()),
            ahead: 0,
            behind: 0,
            upstream: None,
            changes: ["a.rs", "b.rs"]
                .iter()
                .map(|p| Change {
                    code: " M".into(),
                    path: p.to_string(),
                    added: Some(1),
                    removed: Some(0),
                })
                .collect(),
            changes_total: 2,
            added: 2,
            removed: 0,
            log: ["c1", "c2"]
                .iter()
                .map(|h| Commit {
                    hash: h.to_string(),
                    subject: "s".into(),
                    time: 0,
                    author: "a".into(),
                })
                .collect(),
        }
    }

    fn detail(files: &[&str]) -> CommitDetail {
        CommitDetail {
            author: "a".into(),
            email: "a@b".into(),
            date: "d".into(),
            message: "m".into(),
            files: files
                .iter()
                .map(|p| FileStat {
                    path: p.to_string(),
                    added: Some(1),
                    removed: Some(1),
                })
                .collect(),
        }
    }

    #[test]
    fn the_cursor_skips_rows_that_are_only_there_to_be_read() {
        let s = snap();
        let mut v = GitView::new();
        v.cursor = Some(Row::Change("b.rs".into()));
        // Below b.rs is the gap, then the history heading.
        v.move_cursor(&s, 1);
        assert_eq!(v.cursor, Some(Row::History));
    }

    #[test]
    fn an_opened_commit_lists_its_files_once_they_are_known() {
        let s = snap();
        let mut v = GitView::new();
        v.cursor = Some(Row::Commit("c1".into()));
        v.toggle();
        assert!(v.rows(&s, None).contains(&Row::Loading("c1".into())));

        v.details.insert("c1".into(), detail(&["x.rs", "y.rs"]));
        let rows = v.rows(&s, None);
        assert!(rows.contains(&Row::CommitFile("c1".into(), "y.rs".into())));
        assert!(!rows.contains(&Row::Loading("c1".into())));

        v.move_cursor(&s, 2);
        assert_eq!(v.cursor, Some(Row::CommitFile("c1".into(), "y.rs".into())));
        // Entering the commit showed it; left takes the preview away first,
        // then climbs from a file to its commit, and then closes that.
        assert!(v.preview_open);
        assert!(v.close());
        assert!(!v.preview_open);
        assert!(v.close());
        assert_eq!(v.cursor, Some(Row::Commit("c1".into())));
        assert!(v.close());
        assert!(
            !v.rows(&s, None)
                .contains(&Row::CommitFile("c1".into(), "x.rs".into()))
        );
    }

    #[test]
    fn a_cursor_whose_row_went_away_lands_next_to_where_it_was() {
        let mut s = snap();
        let mut v = GitView::new();
        v.cursor = Some(Row::Change("b.rs".into()));
        let _ = v.cursor_index(&v.rows(&s, None));
        // b.rs was committed: the row is gone, a.rs is the nearest one left.
        s.changes.pop();
        s.changes_total = 1;
        let rows = v.rows(&s, None);
        let i = v.cursor_index(&rows);
        assert_eq!(rows[i], Row::Change("a.rs".into()));
    }

    #[test]
    fn a_capped_list_says_how_many_it_left_out() {
        let s = snap();
        let v = GitView::new();
        let rows = v.rows(&s, Some(1));
        assert!(rows.contains(&Row::More(2)));
        assert!(!rows.contains(&Row::Change("a.rs".into())));
    }
}
