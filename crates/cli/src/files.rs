//! The file tree beside the editor: a directory, the folders opened in it,
//! and a cursor.
//!
//! Directories are listed when first shown and listed again on `refresh`,
//! which the app calls every couple of seconds while the tree is on screen:
//! sessions create and delete files all the time, and the tree should notice
//! without being told. The cursor is held as a path, not an index, so it
//! stays on its file while rows around it come and go.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

/// Names never listed: git's own database says nothing about the project.
const HIDDEN: &[&str] = &[".git"];

/// Directories the quick-open search does not walk into. They are listed in
/// the tree like any other; only the search skips them, since a
/// `node_modules` alone can outnumber the project a hundred to one.
const SKIP_SEARCH: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".cache",
];

/// Files the quick-open search collects at most.
const SEARCH_LIMIT: usize = 50_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub dir: bool,
    pub depth: usize,
    pub open: bool,
}

pub struct Tree {
    pub root: PathBuf,
    /// Opened folders, by absolute path, so they stay open across a change of
    /// root and back.
    pub expanded: HashSet<PathBuf>,
    listings: HashMap<PathBuf, Vec<Entry>>,
    pub cursor: Option<PathBuf>,
    last_index: usize,
    /// First row on screen.
    pub scroll: usize,
    /// The view should keep the cursor in sight; the wheel turns it off.
    pub follow: bool,
}

impl Tree {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            expanded: HashSet::new(),
            listings: HashMap::new(),
            cursor: None,
            last_index: 0,
            scroll: 0,
            follow: true,
        }
    }

    /// Point the tree at another directory. The same one again changes
    /// nothing, so the cursor stays where it was.
    pub fn set_root(&mut self, root: PathBuf) {
        if root == self.root {
            return;
        }
        self.root = root;
        self.listings.clear();
        self.cursor = None;
        self.last_index = 0;
        self.scroll = 0;
    }

    /// Forget every listing, so the next draw reads the directories again.
    pub fn refresh(&mut self) {
        self.listings.clear();
    }

    fn listing(&mut self, dir: &Path) -> &Vec<Entry> {
        self.listings
            .entry(dir.to_path_buf())
            .or_insert_with(|| list_dir(dir))
    }

    /// Every visible row: the root's entries, and inside each opened folder
    /// its own, depth first.
    pub fn rows(&mut self) -> Vec<Row> {
        let mut out = Vec::new();
        let root = self.root.clone();
        self.push_rows(&root, 0, &mut out);
        out
    }

    fn push_rows(&mut self, dir: &Path, depth: usize, out: &mut Vec<Row>) {
        let entries = self.listing(dir).clone();
        for e in entries {
            let open = e.dir && self.expanded.contains(&e.path);
            out.push(Row {
                path: e.path.clone(),
                name: e.name.clone(),
                dir: e.dir,
                depth,
                open,
            });
            if open {
                self.push_rows(&e.path, depth + 1, out);
            }
        }
    }

    /// The cursor's place in `rows`, moving it to a row that exists when its
    /// own went away.
    pub fn cursor_index(&mut self, rows: &[Row]) -> usize {
        if rows.is_empty() {
            return 0;
        }
        if let Some(i) = self
            .cursor
            .as_ref()
            .and_then(|c| rows.iter().position(|r| &r.path == c))
        {
            self.last_index = i;
            return i;
        }
        let i = self.last_index.min(rows.len() - 1);
        self.cursor = Some(rows[i].path.clone());
        self.last_index = i;
        i
    }

    pub fn selected(&mut self) -> Option<Row> {
        let rows = self.rows();
        let i = self.cursor_index(&rows);
        rows.get(i).cloned()
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let rows = self.rows();
        if rows.is_empty() {
            return;
        }
        let i = self.cursor_index(&rows);
        let j = i.saturating_add_signed(delta).min(rows.len() - 1);
        self.cursor = Some(rows[j].path.clone());
        self.last_index = j;
    }

    pub fn select_index(&mut self, i: usize) -> Option<Row> {
        let rows = self.rows();
        let row = rows.get(i)?.clone();
        self.cursor = Some(row.path.clone());
        self.last_index = i;
        Some(row)
    }

    /// Open or close the folder under the cursor. Returns the file under it
    /// instead, when it is one, for the caller to open.
    pub fn activate(&mut self) -> Option<PathBuf> {
        let row = self.selected()?;
        if row.dir {
            if !self.expanded.remove(&row.path) {
                self.expanded.insert(row.path);
            }
            None
        } else {
            Some(row.path)
        }
    }

    /// Right: open the folder, or step into it when it already is.
    pub fn open(&mut self) -> Option<PathBuf> {
        let row = self.selected()?;
        if !row.dir {
            return Some(row.path);
        }
        if self.expanded.insert(row.path) {
            return None;
        }
        self.move_cursor(1);
        None
    }

    /// Left: close the folder under the cursor, or climb to the one holding
    /// it. Returns false at the top, where there is nowhere to climb.
    pub fn close(&mut self) -> bool {
        let Some(row) = self.selected() else {
            return false;
        };
        if row.open {
            self.expanded.remove(&row.path);
            return true;
        }
        match row.path.parent() {
            Some(p) if p != self.root && p.starts_with(&self.root) => {
                self.cursor = Some(p.to_path_buf());
                true
            }
            _ => false,
        }
    }

    /// Open every folder above `path` and put the cursor on it.
    pub fn reveal(&mut self, path: &Path) {
        if !path.starts_with(&self.root) {
            return;
        }
        let mut p = path.parent();
        while let Some(dir) = p {
            if dir == self.root || !dir.starts_with(&self.root) {
                break;
            }
            self.expanded.insert(dir.to_path_buf());
            p = dir.parent();
        }
        self.cursor = Some(path.to_path_buf());
        self.follow = true;
    }

    /// The folder new files go into: the one under the cursor, the one
    /// holding the file under it, or the root.
    pub fn target_dir(&mut self) -> PathBuf {
        match self.selected() {
            Some(r) if r.dir => r.path,
            Some(r) => r
                .path
                .parent()
                .map_or_else(|| self.root.clone(), Path::to_path_buf),
            None => self.root.clone(),
        }
    }
}

/// A directory's entries, folders first, each group by name regardless of
/// case. An unreadable directory lists as empty.
pub fn list_dir(dir: &Path) -> Vec<Entry> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Entry> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if HIDDEN.contains(&name.as_str()) {
                return None;
            }
            // A link to a directory counts as one: that is how it opens.
            let dir = e.path().is_dir();
            Some(Entry {
                path: e.path(),
                name,
                dir,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// Every file under `root`, as paths relative to it with `/` between parts,
/// for the quick-open search.
pub fn all_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.filter_map(Result::ok) {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() {
                if !SKIP_SEARCH.contains(&name.as_str()) {
                    stack.push(e.path());
                }
            } else if let Ok(rel) = e.path().strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
                if out.len() >= SEARCH_LIMIT {
                    return out;
                }
            }
        }
    }
    out
}

/// How well `query` matches `path`: its characters in order, anywhere in it.
/// `None` when they are not all there. Lower is better; a match in the file
/// name and a run of adjacent characters beat one spread over the path.
pub fn fuzzy_score(path: &str, query: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(path.len() as i64);
    }
    let lower = path.to_lowercase();
    let hay: Vec<char> = lower.chars().collect();
    let name_start = lower.rfind('/').map_or(0, |i| lower[..=i].chars().count());
    let mut score: i64 = 0;
    let mut at = 0;
    let mut prev: Option<usize> = None;
    for q in query.to_lowercase().chars() {
        if q == ' ' {
            continue;
        }
        let i = (at..hay.len()).find(|&i| hay[i] == q)?;
        score += match prev {
            Some(p) if p + 1 == i => 0,
            _ => 10,
        };
        if i < name_start {
            score += 5;
        }
        prev = Some(i);
        at = i + 1;
    }
    Some(score * 100 + path.len() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sandbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fleet-files-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/deep")).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join("README.md"), "x").unwrap();
        fs::write(dir.join("src/main.rs"), "x").unwrap();
        fs::write(dir.join("src/deep/a.rs"), "x").unwrap();
        dir
    }

    #[test]
    fn folders_come_first_and_git_is_left_out() {
        let dir = sandbox("list");
        let names: Vec<String> = list_dir(&dir).into_iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["src", "README.md"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn opening_folders_and_climbing_back() {
        let dir = sandbox("walk");
        let mut t = Tree::new(dir.clone());
        assert_eq!(t.rows().len(), 2);
        // Cursor on `src`: right opens it, right again steps in.
        assert_eq!(t.open(), None);
        assert_eq!(t.rows().len(), 4);
        t.open();
        assert_eq!(t.cursor, Some(dir.join("src").join("deep")));
        // Left from a closed folder climbs to its parent, then closes that.
        assert!(t.close());
        assert_eq!(t.cursor, Some(dir.join("src")));
        assert!(t.close());
        assert_eq!(t.rows().len(), 2);
        assert!(!t.close());

        t.reveal(&dir.join("src").join("deep").join("a.rs"));
        let row = t.selected().unwrap();
        assert_eq!(row.name, "a.rs");
        assert_eq!(row.depth, 2);
        assert_eq!(
            t.activate(),
            Some(dir.join("src").join("deep").join("a.rs"))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn quick_open_finds_by_scattered_letters() {
        let dir = sandbox("search");
        let files = all_files(&dir);
        assert_eq!(files.len(), 3);
        assert!(fuzzy_score("src/deep/a.rs", "dar").is_some());
        assert!(fuzzy_score("src/main.rs", "xyz").is_none());
        // The file name beats the directory it sits in.
        assert!(fuzzy_score("src/main.rs", "main") < fuzzy_score("main/src/x.rs", "main"));
        let _ = fs::remove_dir_all(&dir);
    }
}
