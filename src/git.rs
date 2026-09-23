//! What git has to say about the directory a session works in: the branch,
//! the uncommitted changes, and the last commits.
//!
//! Read by running `git` itself rather than a library: it is on every machine
//! that has Claude Code working in a repository, it knows every repository
//! layout there is (worktrees, submodules, `safe.directory`), and a snapshot
//! every couple of seconds is cheap next to what the sessions are doing.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{LazyLock, Mutex},
};

/// How many commits the panel lists. More than any terminal is tall; the
/// panel draws what fits.
const LOG_LIMIT: usize = 60;

/// How many changed files are kept. A tree with thousands of them (a fresh
/// `node_modules` nobody ignored) is summed up by its count, not listed.
const CHANGES_LIMIT: usize = 200;

/// Separates the fields of one `git log` line. A unit separator cannot turn up
/// in a subject line, so a subject with tabs or pipes in it stays whole.
const SEP: char = '\u{1f}';

/// The longest diff kept for the preview. Past this it is a generated file or
/// a lockfile, and the first few thousand lines say what it is.
const DIFF_LIMIT: usize = 5000;

/// Untracked files larger than this are not read to count their lines.
const COUNT_LIMIT: u64 = 512 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The top of the work tree, which may be above the session's directory.
    pub root: PathBuf,
    /// The branch checked out, or `None` on a detached HEAD.
    pub branch: Option<String>,
    /// Commits ahead of and behind the upstream, when there is one.
    pub ahead: u32,
    pub behind: u32,
    pub upstream: Option<String>,
    pub changes: Vec<Change>,
    /// How many changes there were before `changes` was cut to the limit.
    pub changes_total: usize,
    /// Lines added and removed across all of `changes`, text files only.
    pub added: u32,
    pub removed: u32,
    pub log: Vec<Commit>,
}

impl Snapshot {
    /// Whether HEAD points at a commit. A fresh repository has none, and
    /// diffing against it is then an error.
    pub fn has_head(&self) -> bool {
        !self.log.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// The two-letter porcelain code: index then work tree, e.g. `M `, ` M`, `??`.
    pub code: String,
    pub path: String,
    /// Lines added and removed against HEAD. `None` for a binary file, or one
    /// nothing could be counted for (an untracked directory, a huge file).
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

impl Change {
    pub fn untracked(&self) -> bool {
        self.code == "??"
    }

    /// Staged, as in: something is in the index for it.
    pub fn staged(&self) -> bool {
        let x = self.code.chars().next().unwrap_or(' ');
        x != ' ' && x != '?'
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub subject: String,
    /// Committer time, in seconds since the epoch.
    pub time: u64,
    pub author: String,
}

/// Lines added and removed in one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStat {
    pub path: String,
    /// `None` for a binary file.
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

/// Everything about one commit the preview shows: who, when, the whole
/// message, and the files it touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitDetail {
    pub author: String,
    pub email: String,
    pub date: String,
    pub message: String,
    pub files: Vec<FileStat>,
}

/// What a diff in the preview is of.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DiffSource {
    /// A file with uncommitted changes, against HEAD.
    Worktree {
        path: String,
        untracked: bool,
        has_head: bool,
    },
    /// A file as one commit changed it, against its first parent.
    Commit { hash: String, path: String },
}

/// A branch one can switch to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    /// `main` for a local branch, `origin/feature` for a remote one.
    pub name: String,
    /// Checked out right now.
    pub current: bool,
    /// Only on a remote: switching to it creates the local branch first.
    pub remote: bool,
    /// Committer time of its tip, in seconds since the epoch.
    pub time: u64,
}

impl Branch {
    /// The local name switching to it lands on: `origin/feature` is `feature`.
    pub fn local_name(&self) -> &str {
        if self.remote {
            self.name.split_once('/').map_or(&self.name, |(_, b)| b)
        } else {
            &self.name
        }
    }
}

/// What a read of the directory found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Repo(Snapshot),
    /// Git answered, and this is not a repository.
    NotARepo,
    /// Git could not be run at all.
    NoGit,
}

/// Read the repository `cwd` sits in. Blocks for as long as git takes, so it
/// belongs on a thread of its own.
pub fn read(cwd: &Path) -> State {
    let status = match git(cwd, &["status", "--porcelain=v1", "--branch", "-z"]) {
        Ok(Some(out)) => out,
        Ok(None) => {
            forget_root(cwd);
            return State::NotARepo;
        }
        Err(()) => return State::NoGit,
    };
    let root = root_of(cwd);

    let mut snap = parse_status(&status);

    // An empty repository has no HEAD, and `git log` says so on stderr with a
    // failing exit code. That is a repository with no history, not an error.
    let format = format!("--format=%h{SEP}%s{SEP}%ct{SEP}%an");
    let limit = format!("-n{LOG_LIMIT}");
    if let Ok(Some(log)) = git(cwd, &["log", &limit, &format]) {
        snap.log = parse_log(&log);
    }

    // Staged and unstaged together, against HEAD: what committing everything
    // would add up to. Paths in both outputs are relative to the root.
    let diff_args: &[&str] = if snap.has_head() {
        &["diff", "HEAD", "--numstat", "-z", "--no-ext-diff"]
    } else {
        &["diff", "--cached", "--numstat", "-z", "--no-ext-diff"]
    };
    let stats: HashMap<String, FileStat> = git(&root, diff_args)
        .ok()
        .flatten()
        .map(|out| parse_numstat(&out))
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.path.clone(), s))
        .collect();
    for c in &mut snap.changes {
        if c.untracked() {
            c.added = count_lines(&root.join(&c.path));
            c.removed = c.added.map(|_| 0);
        } else if let Some(s) = stats.get(&c.path) {
            c.added = s.added;
            c.removed = s.removed;
        }
        snap.added += c.added.unwrap_or(0);
        snap.removed += c.removed.unwrap_or(0);
    }

    snap.root = root;
    State::Repo(snap)
}

/// Work-tree tops already asked for, by the directory they were asked from.
///
/// The panel reads every two seconds and each `git` started costs tens of
/// milliseconds on Windows; where a directory's work tree begins does not move
/// between reads, so it is asked once.
static ROOTS: LazyLock<Mutex<HashMap<PathBuf, PathBuf>>> = LazyLock::new(Default::default);

pub fn root_of(cwd: &Path) -> PathBuf {
    if let Some(root) = ROOTS.lock().ok().and_then(|m| m.get(cwd).cloned()) {
        return root;
    }
    match git(cwd, &["rev-parse", "--show-toplevel"]).ok().flatten() {
        Some(out) => {
            let root = PathBuf::from(out.trim());
            if let Ok(mut m) = ROOTS.lock() {
                m.insert(cwd.to_path_buf(), root.clone());
            }
            root
        }
        None => cwd.to_path_buf(),
    }
}

/// A directory that stopped being a repository may become a different one.
fn forget_root(cwd: &Path) {
    if let Ok(mut m) = ROOTS.lock() {
        m.remove(cwd);
    }
}

/// Who made a commit, when, what it says, and what it touched.
pub fn commit_detail(root: &Path, hash: &str) -> Option<CommitDetail> {
    let format = format!("--format=%an{SEP}%ae{SEP}%ad{SEP}%B");
    let head = git(
        root,
        &["show", "-s", "--date=format:%Y-%m-%d %H:%M", &format, hash],
    )
    .ok()??;
    let mut f = head.splitn(4, SEP);
    let author = f.next()?.to_string();
    let email = f.next()?.to_string();
    let date = f.next()?.to_string();
    let message = f.next().unwrap_or("").trim_end().to_string();
    // Against the first parent, so a merge shows what it brought in; `--root`
    // makes the first commit show its files instead of nothing.
    let files = git(
        root,
        &[
            "diff-tree",
            "--no-commit-id",
            "-r",
            "--root",
            "-m",
            "--first-parent",
            "--numstat",
            "-z",
            "--no-ext-diff",
            hash,
        ],
    )
    .ok()
    .flatten()
    .map(|out| parse_numstat(&out))
    .unwrap_or_default();
    Some(CommitDetail {
        author,
        email,
        date,
        message,
        files,
    })
}

/// The diff behind one row of the panel, as plain lines.
pub fn diff(root: &Path, src: &DiffSource) -> Vec<String> {
    let out = match src {
        DiffSource::Worktree {
            path,
            untracked: true,
            ..
        } => return untracked_diff(&root.join(path)),
        DiffSource::Worktree { path, has_head, .. } => {
            let spec = format!(":(top){path}");
            let base = if *has_head { "HEAD" } else { "--cached" };
            git(
                root,
                &["diff", base, "--no-color", "--no-ext-diff", "--", &spec],
            )
        }
        DiffSource::Commit { hash, path } => {
            let spec = format!(":(top){path}");
            git(
                root,
                &[
                    "diff-tree",
                    "-p",
                    "--no-commit-id",
                    "-r",
                    "--root",
                    "-m",
                    "--first-parent",
                    "--no-color",
                    "--no-ext-diff",
                    hash,
                    "--",
                    &spec,
                ],
            )
        }
    };
    let out = out.ok().flatten().unwrap_or_default();
    let mut lines: Vec<String> = out.lines().take(DIFF_LIMIT).map(str::to_string).collect();
    if lines.is_empty() {
        lines.push("(no textual changes)".to_string());
    }
    lines
}

/// An untracked file has nothing to diff against, so all of it is new.
fn untracked_diff(path: &Path) -> Vec<String> {
    if path.is_dir() {
        return vec!["(an untracked directory)".to_string()];
    }
    let Ok(meta) = fs::metadata(path) else {
        return vec!["(gone)".to_string()];
    };
    if meta.len() > COUNT_LIMIT * 4 {
        return vec![format!(
            "(new file, {} KB — too big to show)",
            meta.len() / 1024
        )];
    }
    let Ok(bytes) = fs::read(path) else {
        return vec!["(could not be read)".to_string()];
    };
    if bytes.contains(&0) {
        return vec!["(new binary file)".to_string()];
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = vec!["new file".to_string()];
    lines.extend(text.lines().take(DIFF_LIMIT).map(|l| format!("+{l}")));
    lines
}

/// Lines in a new text file, or `None` when it is not one worth reading.
fn count_lines(path: &Path) -> Option<u32> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > COUNT_LIMIT {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    let newlines = bytes.iter().filter(|&&b| b == b'\n').count();
    let unterminated = !bytes.is_empty() && !bytes.ends_with(b"\n");
    Some((newlines + usize::from(unterminated)) as u32)
}

/// `--numstat -z`: `added TAB removed TAB path NUL`, or for a rename an empty
/// path followed by the old and the new one, each ended by a NUL. A binary
/// file has `-` for both counts.
fn parse_numstat(out: &str) -> Vec<FileStat> {
    let mut stats = Vec::new();
    let mut it = out.split('\0');
    while let Some(e) = it.next() {
        let e = e.trim_start_matches('\n');
        if e.is_empty() {
            continue;
        }
        let mut f = e.splitn(3, '\t');
        let (Some(a), Some(r), Some(p)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let path = if p.is_empty() {
            it.next();
            it.next().unwrap_or("").to_string()
        } else {
            p.to_string()
        };
        stats.push(FileStat {
            path,
            added: a.parse().ok(),
            removed: r.parse().ok(),
        });
    }
    stats
}

/// The local branches, then the remote ones nobody has checked out yet, each
/// newest first.
pub fn branches(root: &Path) -> Vec<Branch> {
    let format = format!("--format=%(HEAD){SEP}%(refname){SEP}%(committerdate:unix)");
    let Ok(Some(out)) = git(
        root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            &format,
            "refs/heads",
            "refs/remotes",
        ],
    ) else {
        return Vec::new();
    };
    parse_branches(&out)
}

fn parse_branches(out: &str) -> Vec<Branch> {
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for line in out.lines() {
        let mut f = line.split(SEP);
        let (Some(head), Some(refname)) = (f.next(), f.next()) else {
            continue;
        };
        let time = f.next().and_then(|t| t.parse().ok()).unwrap_or(0);
        if let Some(name) = refname.strip_prefix("refs/heads/") {
            local.push(Branch {
                name: name.to_string(),
                current: head == "*",
                remote: false,
                time,
            });
        } else if let Some(name) = refname.strip_prefix("refs/remotes/") {
            // `origin/HEAD` only points at another remote branch.
            if name.ends_with("/HEAD") {
                continue;
            }
            remote.push(Branch {
                name: name.to_string(),
                current: false,
                remote: true,
                time,
            });
        }
    }
    // A remote branch with a local one of the same name is that local one:
    // listing both offers two ways to the same place.
    remote.retain(|r| !local.iter().any(|l: &Branch| l.name == r.local_name()));
    local.extend(remote);
    local
}

/// Check `branch` out. A remote one gets a local branch tracking it. The error
/// is what git said, for the status line.
pub fn switch(root: &Path, branch: &Branch) -> Result<(), String> {
    if branch.remote {
        run(root, &["switch", "--track", &branch.name])
    } else {
        run(root, &["switch", &branch.name])
    }
}

/// Start a new branch at HEAD and check it out.
pub fn create_branch(root: &Path, name: &str) -> Result<(), String> {
    run(root, &["switch", "-c", name])
}

/// Whether anything is in the index waiting to be committed.
fn has_staged(root: &Path) -> bool {
    // `--quiet` answers by exit code: 1 when there is a difference.
    matches!(git(root, &["diff", "--cached", "--quiet"]), Ok(None))
}

/// Commit with `message`. What is staged is what goes in; with nothing staged,
/// every change is, untracked files included — the panel has no staging of its
/// own, and a commit button that commits nothing would only be in the way.
/// Returns the new commit's short hash.
pub fn commit(root: &Path, message: &str) -> Result<String, String> {
    if !has_staged(root) {
        run(root, &["add", "-A"])?;
        if !has_staged(root) {
            return Err("nothing to commit".to_string());
        }
    }
    let mut child = command(root, &["commit", "-F", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git could not be run: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(message.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git could not be run: {e}"))?;
    if !out.status.success() {
        return Err(reason(&String::from_utf8_lossy(&out.stderr)));
    }
    Ok(git(root, &["rev-parse", "--short", "HEAD"])
        .ok()
        .flatten()
        .map(|s| s.trim().to_string())
        .unwrap_or_default())
}

/// Bring the remote-tracking branches up to date, so ahead and behind count
/// against what the remote has now rather than at the last fetch.
pub fn fetch(root: &Path) -> Result<(), String> {
    run(root, &["fetch", "--prune"])
}

/// Fetch, then replay the local commits on top of the upstream. Uncommitted
/// changes are stashed around it, so a dirty tree is no reason to refuse.
///
/// A rebase that stops on conflicts is aborted: the sessions keep working in
/// this tree, and one left halfway through a rebase is in nobody's interest.
/// The error then says so, with git's words.
pub fn pull(root: &Path) -> Result<(), String> {
    let (ok, output) = capture(root, &["pull", "--rebase", "--autostash"])?;
    if ok {
        return Ok(());
    }
    if in_conflict(root) {
        let _ = run(root, &["rebase", "--abort"]);
        return Err(format!(
            "{}\n\nfleet ran `git pull --rebase --autostash` and it stopped on \
             conflicting files; the rebase was aborted, so the branch is back \
             where it was before the pull.",
            output.trim_end()
        ));
    }
    Err(output.trim_end().to_string())
}

/// Push the branch checked out. One without an upstream gets one on `origin`,
/// the way the first push of a new branch is nearly always meant.
///
/// A push the remote rejects for being behind pulls the new commits in (see
/// `pull`) and goes again, so new work on the remote does not stand in the
/// way. Only when that pull cannot be done cleanly does the push fail.
pub fn push(root: &Path, branch: &str, has_upstream: bool) -> Result<(), PushFailure> {
    let args: &[&str] = if has_upstream {
        &["push"]
    } else {
        &["push", "-u", "origin", branch]
    };
    let failure = |kind: PushError, output: String| PushFailure {
        kind,
        reason: reason(&output),
        output: output.trim_end().to_string(),
        log: push_log(root),
    };
    let (ok, output) = capture(root, args).map_err(|e| PushFailure {
        kind: PushError::Other,
        reason: e,
        output: String::new(),
        log: String::new(),
    })?;
    if ok {
        return Ok(());
    }
    let kind = if in_conflict(root) {
        PushError::Conflict
    } else {
        classify_push(&output)
    };
    if kind != PushError::Rejected || !has_upstream {
        return Err(failure(kind, output));
    }

    if let Err(why) = pull(root) {
        let kind = if why.contains("conflicting files") {
            PushError::Conflict
        } else {
            PushError::Rejected
        };
        return Err(failure(kind, format!("{output}\n{why}")));
    }
    match capture(root, args) {
        Ok((true, _)) => Ok(()),
        Ok((false, again)) => {
            let kind = if in_conflict(root) {
                PushError::Conflict
            } else {
                classify_push(&again)
            };
            Err(failure(kind, again))
        }
        Err(e) => Err(failure(PushError::Other, e)),
    }
}

/// Run git and keep everything it said, stderr first. `Ok((success, output))`;
/// `Err` only when git could not be run.
fn capture(root: &Path, args: &[&str]) -> Result<(bool, String), String> {
    let out = command(root, args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("git could not be run: {e}"))?;
    let mut output = String::from_utf8_lossy(&out.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !stdout.trim().is_empty() {
        output.push_str(&stdout);
    }
    Ok((out.status.success(), output))
}

/// What went wrong with a push, as far as git's words tell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PushError {
    /// A merge or rebase is stopped halfway on conflicting files.
    Conflict,
    /// The remote has commits this branch does not; it needs them first.
    Rejected,
    /// The remote wants credentials it did not get, or refuses them.
    Auth,
    /// The remote could not be reached at all.
    Network,
    /// A hook or a branch protection rule on the remote said no.
    Declined,
    /// A file is past the remote's size limit.
    TooLarge,
    /// No remote to push to, or it is not a repository.
    NoRemote,
    Other,
}

impl PushError {
    pub fn name(self) -> &'static str {
        match self {
            PushError::Conflict => "merge conflict",
            PushError::Rejected => "rejected — remote is ahead",
            PushError::Auth => "authentication",
            PushError::Network => "network",
            PushError::Declined => "declined by the remote",
            PushError::TooLarge => "file too large",
            PushError::NoRemote => "no remote",
            PushError::Other => "other error",
        }
    }
}

/// A push that did not go through: what kind of failure, git's own words,
/// and where the branch stood against its remote when it happened.
#[derive(Clone, Debug)]
pub struct PushFailure {
    pub kind: PushError,
    /// The one line of `output` that says it.
    pub reason: String,
    pub output: String,
    pub log: String,
}

/// Sort git's complaint into a kind. The checks go from the most particular
/// wording to the most general, since a rejection can also mention the URL.
pub fn classify_push(output: &str) -> PushError {
    let o = output.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| o.contains(n));
    if has(&["conflict"]) {
        PushError::Conflict
    } else if has(&[
        "exceeds github's file size limit",
        "gh001",
        "large files detected",
    ]) {
        PushError::TooLarge
    } else if has(&[
        "pre-receive hook declined",
        "protected branch",
        "gh006",
        "gh013",
        "hook declined",
    ]) {
        PushError::Declined
    } else if has(&[
        "non-fast-forward",
        "fetch first",
        "[rejected]",
        "tip of your current branch is behind",
    ]) {
        PushError::Rejected
    } else if has(&[
        "authentication failed",
        "permission denied",
        "could not read username",
        "could not read password",
        "terminal prompts disabled",
        "returned error: 403",
        "returned error: 401",
        "invalid username or password",
        "permission to",
    ]) {
        PushError::Auth
    } else if has(&[
        "could not resolve host",
        "failed to connect",
        "connection timed out",
        "connection refused",
        "network is unreachable",
        "ssl",
        "the remote end hung up",
        "early eof",
    ]) {
        PushError::Network
    } else if has(&[
        "does not appear to be a git repository",
        "no configured push destination",
        "no such remote",
        "repository not found",
        "has no upstream branch",
    ]) {
        PushError::NoRemote
    } else {
        PushError::Other
    }
}

/// Whether a merge, rebase or cherry-pick is stopped on conflicting files.
fn in_conflict(root: &Path) -> bool {
    let unmerged = git(root, &["diff", "--name-only", "--diff-filter=U"])
        .ok()
        .flatten()
        .is_some_and(|out| !out.trim().is_empty());
    let halfway = [
        "MERGE_HEAD",
        "rebase-merge",
        "rebase-apply",
        "CHERRY_PICK_HEAD",
    ]
    .iter()
    .any(|p| {
        git(root, &["rev-parse", "--git-path", p])
            .ok()
            .flatten()
            .is_some_and(|path| root.join(path.trim()).exists())
    });
    unmerged || halfway
}

/// The branch against its remote, as text for the failure dialog: the status
/// line with ahead and behind, then the last commits on both sides.
fn push_log(root: &Path) -> String {
    let mut out = String::new();
    if let Ok(Some(status)) = git(root, &["status", "-sb", "--no-renames"]) {
        out.push_str(status.trim_end());
        out.push_str(
            "

",
        );
    }
    // `@{u}` is missing on a branch never pushed; the plain log is then all.
    let log = git(
        root,
        &[
            "log",
            "--oneline",
            "--graph",
            "--decorate",
            "-n20",
            "HEAD",
            "@{u}",
        ],
    )
    .ok()
    .flatten()
    .or_else(|| {
        git(root, &["log", "--oneline", "--graph", "--decorate", "-n20"])
            .ok()
            .flatten()
    })
    .unwrap_or_default();
    out.push_str(log.trim_end());
    out
}

/// The longest diff handed to the model writing a commit message. Past this
/// the file list and the start of the diff say enough.
const MESSAGE_DIFF_LIMIT: usize = 40_000;

/// What the commit about to be made contains, as text for whoever writes its
/// message: recent subjects for the house style, the files, then the diff.
/// Mirrors `commit`: the staged changes when there are some, otherwise all of
/// them.
pub fn message_context(root: &Path) -> String {
    let staged = has_staged(root);
    let has_head = matches!(
        git(root, &["rev-parse", "--verify", "-q", "HEAD"]),
        Ok(Some(_))
    );
    let base: &[&str] = if staged || !has_head {
        &["--cached"]
    } else {
        &["HEAD"]
    };
    let with = |extra: &[&str]| {
        let mut args = vec!["diff"];
        args.extend_from_slice(base);
        args.extend_from_slice(&["--no-color", "--no-ext-diff"]);
        args.extend_from_slice(extra);
        git(root, &args).ok().flatten().unwrap_or_default()
    };

    let mut out = String::new();
    if let Ok(Some(log)) = git(root, &["log", "-8", "--format=%s"])
        && !log.trim().is_empty()
    {
        out.push_str("Recent commit subjects in this repository:\n");
        out.push_str(&log);
        out.push('\n');
    }
    // Untracked files only go in when nothing is staged, as with `commit`.
    let new = if staged {
        String::new()
    } else {
        git(root, &["ls-files", "--others", "--exclude-standard"])
            .ok()
            .flatten()
            .unwrap_or_default()
    };
    out.push_str("Files changed:\n");
    out.push_str(&with(&["--stat"]));
    if !new.trim().is_empty() {
        out.push_str("\nNew files:\n");
        out.push_str(&new);
    }
    out.push_str("\nDiff:\n");
    out.push_str(&with(&[]));
    // Untracked files have no diff; their start is what they are.
    for path in new.lines().take(20) {
        out.push_str(&format!("\n+++ {path}\n"));
        for l in untracked_diff(&root.join(path)).iter().take(60) {
            out.push_str(l);
            out.push('\n');
        }
    }
    if out.len() > MESSAGE_DIFF_LIMIT {
        let mut cut = MESSAGE_DIFF_LIMIT;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("\n[diff cut here]\n");
    }
    out
}

/// Run a git command that changes something, keeping what it says when it
/// refuses: a switch blocked by uncommitted changes has to say so.
fn run(cwd: &Path, args: &[&str]) -> Result<(), String> {
    let out = command(cwd, args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("git could not be run: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(reason(&String::from_utf8_lossy(&out.stderr)))
}

/// The line of git's complaint worth a status line.
pub fn reason(err: &str) -> String {
    // The first `error:`/`fatal:` line is the reason; the hints under it are
    // advice for a terminal the user is not looking at.
    err.lines()
        .map(str::trim)
        .find(|l| l.starts_with("error:") || l.starts_with("fatal:"))
        .or_else(|| err.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("git refused")
        .to_string()
}

/// Run git in `cwd`. `Ok(None)` is git refusing (not a repository, no HEAD);
/// `Err` is git not being there to ask.
fn git(cwd: &Path, args: &[&str]) -> Result<Option<String>, ()> {
    let out = command(cwd, args)
        .stderr(Stdio::null())
        .output()
        .map_err(|_| ())?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// `git -C cwd args`, with no console window and no prompt to wait on.
fn command(cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(cwd)
        // `status` refreshes the index when it can, and that takes the index
        // lock. A session committing at the same moment would then fail on a
        // lock held by a panel that only wanted to look.
        .env("GIT_OPTIONAL_LOCKS", "0")
        // A push asking for a password on a terminal nobody can see would hang
        // until killed; failing says what is missing instead.
        .env("GIT_TERMINAL_PROMPT", "0")
        // Paths come back as they are, not octal-escaped.
        .args(["-c", "core.quotepath=off"])
        .args(args)
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// `git status --porcelain=v1 --branch -z`: a `## ` header, then one entry per
/// file, each ended by a NUL. A rename carries its old path as one more entry.
fn parse_status(out: &str) -> Snapshot {
    let mut snap = Snapshot {
        root: PathBuf::new(),
        branch: None,
        ahead: 0,
        behind: 0,
        upstream: None,
        changes: Vec::new(),
        changes_total: 0,
        added: 0,
        removed: 0,
        log: Vec::new(),
    };

    let mut entries = out.split('\0').filter(|e| !e.is_empty());
    while let Some(e) = entries.next() {
        if let Some(head) = e.strip_prefix("## ") {
            parse_branch(head, &mut snap);
            continue;
        }
        if e.len() < 4 {
            continue;
        }
        let code = e[..2].to_string();
        let path = e[3..].to_string();
        if code.starts_with('R') || code.starts_with('C') {
            // The path the file came from; the new one is what matters here.
            entries.next();
        }
        snap.changes_total += 1;
        if snap.changes.len() < CHANGES_LIMIT {
            snap.changes.push(Change {
                code,
                path,
                added: None,
                removed: None,
            });
        }
    }
    snap
}

/// The header line: `main...origin/main [ahead 1, behind 2]`, or
/// `No commits yet on main`, or `HEAD (no branch)`.
fn parse_branch(head: &str, snap: &mut Snapshot) {
    let head = head
        .strip_prefix("No commits yet on ")
        .or_else(|| head.strip_prefix("Initial commit on "))
        .unwrap_or(head);
    let (names, counts) = match head.split_once(" [") {
        Some((n, c)) => (n, c.trim_end_matches(']')),
        None => (head, ""),
    };
    let (local, upstream) = match names.split_once("...") {
        Some((l, u)) => (l, Some(u.to_string())),
        None => (names, None),
    };
    if !local.starts_with("HEAD (no branch)") {
        snap.branch = Some(local.to_string());
    }
    snap.upstream = upstream;
    for part in counts.split(", ") {
        if let Some(n) = part.strip_prefix("ahead ") {
            snap.ahead = n.parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            snap.behind = n.parse().unwrap_or(0);
        }
    }
}

fn parse_log(out: &str) -> Vec<Commit> {
    out.lines()
        .filter_map(|line| {
            let mut f = line.split(SEP);
            Some(Commit {
                hash: f.next()?.to_string(),
                subject: f.next()?.to_string(),
                time: f.next()?.parse().unwrap_or(0),
                author: f.next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_errors_are_sorted_by_kind() {
        let cases = [
            (
                " ! [rejected]        main -> main (fetch first)
error: failed to push some refs to 'github.com:a/b.git'",
                PushError::Rejected,
            ),
            (
                " ! [rejected]        main -> main (non-fast-forward)",
                PushError::Rejected,
            ),
            (
                "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
                PushError::Auth,
            ),
            (
                "fatal: unable to access 'https://github.com/a/b.git/': Could not resolve host: github.com",
                PushError::Network,
            ),
            (
                "remote: error: GH006: Protected branch update failed for refs/heads/main.
 ! [remote rejected] main -> main (protected branch hook declined)",
                PushError::Declined,
            ),
            (
                "remote: error: File big.bin is 120.00 MB; this exceeds GitHub's file size limit of 100.00 MB",
                PushError::TooLarge,
            ),
            (
                "fatal: 'origin' does not appear to be a git repository",
                PushError::NoRemote,
            ),
            ("something else entirely", PushError::Other),
        ];
        for (output, kind) in cases {
            assert_eq!(classify_push(output), kind, "{output}");
        }
    }

    #[test]
    fn a_commit_with_nothing_staged_takes_every_change() {
        let dir = std::env::temp_dir().join(format!("fleet-commit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let ok = |args: &[&str]| assert!(run(&dir, args).is_ok(), "git {args:?}");
        ok(&["init", "-q"]);
        ok(&["config", "user.name", "t"]);
        ok(&["config", "user.email", "t@t"]);
        fs::write(dir.join("a.txt"), "one\n").unwrap();

        assert!(message_context(&dir).contains("a.txt"));
        let hash = commit(&dir, "Add a\n\nBody").unwrap();
        assert!(!hash.is_empty());
        let State::Repo(snap) = read(&dir) else {
            panic!("not read as a repository");
        };
        assert_eq!(snap.changes_total, 0);
        assert_eq!(snap.log[0].subject, "Add a");
        // Nothing left: saying so beats an empty commit.
        assert_eq!(commit(&dir, "again"), Err("nothing to commit".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_push_behind_the_remote_pulls_first_and_a_conflict_is_undone() {
        let base = std::env::temp_dir().join(format!("fleet-push-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let ok = |dir: &Path, args: &[&str]| assert!(run(dir, args).is_ok(), "git {args:?}");
        let (remote, a, b) = (base.join("remote"), base.join("a"), base.join("b"));
        ok(&base, &["init", "-q", "--bare", "-b", "main", "remote"]);
        for dir in [&a, &b] {
            ok(
                &base,
                &[
                    "clone",
                    "-q",
                    remote.to_str().unwrap(),
                    dir.to_str().unwrap(),
                ],
            );
            ok(dir, &["config", "user.name", "t"]);
            ok(dir, &["config", "user.email", "t@t"]);
            ok(dir, &["checkout", "-q", "-B", "main"]);
        }
        fs::write(a.join("one.txt"), "a\n").unwrap();
        commit(&a, "one").unwrap();
        assert!(push(&a, "main", false).is_ok());
        ok(&b, &["pull", "-q", "origin", "main"]);
        ok(&b, &["branch", "-q", "-u", "origin/main"]);

        // Both add a commit; b is behind and its push still goes through.
        fs::write(a.join("two.txt"), "a\n").unwrap();
        commit(&a, "two").unwrap();
        assert!(push(&a, "main", true).is_ok());
        fs::write(b.join("three.txt"), "b\n").unwrap();
        commit(&b, "three").unwrap();
        fs::write(b.join("dirty.txt"), "not committed\n").unwrap();
        push(&b, "main", true).unwrap();
        assert!(b.join("two.txt").exists() && b.join("dirty.txt").exists());

        // Both change the same line: the push fails as a conflict, and b is
        // not left in the middle of a rebase.
        ok(&a, &["pull", "-q"]);
        fs::write(a.join("one.txt"), "from a\n").unwrap();
        commit(&a, "a edits").unwrap();
        assert!(push(&a, "main", true).is_ok());
        fs::write(b.join("one.txt"), "from b\n").unwrap();
        commit(&b, "b edits").unwrap();
        let err = push(&b, "main", true).unwrap_err();
        assert_eq!(err.kind, PushError::Conflict, "{}", err.output);
        assert!(!in_conflict(&b));
        // Trimmed: a machine with `core.autocrlf` checks it out with CRLF.
        let one = fs::read_to_string(b.join("one.txt")).unwrap();
        assert_eq!(one.trim_end(), "from b");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_branch_with_an_upstream_says_how_far_apart_they_are() {
        let s = parse_status("## main...origin/main [ahead 2, behind 1]\0");
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.upstream.as_deref(), Some("origin/main"));
        assert_eq!((s.ahead, s.behind), (2, 1));
    }

    #[test]
    fn a_fresh_repository_still_has_a_branch_name() {
        let s = parse_status("## No commits yet on main\0?? a.txt\0");
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.changes.len(), 1);
    }

    #[test]
    fn a_detached_head_has_no_branch() {
        let s = parse_status("## HEAD (no branch)\0");
        assert_eq!(s.branch, None);
    }

    #[test]
    fn a_rename_is_one_change_under_its_new_name() {
        let s = parse_status("## main\0R  new.rs\0old.rs\0 M src/ui.rs\0");
        assert_eq!(s.changes_total, 2);
        assert_eq!(s.changes[0].path, "new.rs");
        assert!(s.changes[0].staged());
        assert_eq!(s.changes[1].path, "src/ui.rs");
        assert!(!s.changes[1].staged());
    }

    #[test]
    fn untracked_files_are_not_staged() {
        let s = parse_status("## main\0?? notes.md\0");
        assert!(!s.changes[0].staged());
    }

    #[test]
    fn a_subject_with_pipes_and_tabs_stays_whole() {
        let line = format!("41c45ad{SEP}fix a | b\tc{SEP}1700000000{SEP}Ann Lee");
        let log = parse_log(&line);
        assert_eq!(log[0].subject, "fix a | b\tc");
        assert_eq!(log[0].time, 1_700_000_000);
        assert_eq!(log[0].author, "Ann Lee");
    }

    #[test]
    fn remote_branches_with_a_local_twin_are_left_out() {
        let out = [
            format!("*{SEP}refs/heads/main{SEP}300"),
            format!(" {SEP}refs/heads/fix{SEP}200"),
            format!(" {SEP}refs/remotes/origin/HEAD{SEP}300"),
            format!(" {SEP}refs/remotes/origin/main{SEP}300"),
            format!(" {SEP}refs/remotes/origin/feature/x{SEP}100"),
        ]
        .join(
            "
",
        );
        let b = parse_branches(&out);
        let names: Vec<&str> = b.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["main", "fix", "origin/feature/x"]);
        assert!(b[0].current && !b[1].current);
        assert!(b[2].remote);
        assert_eq!(b[2].local_name(), "feature/x");
    }

    #[test]
    fn numstat_counts_renames_under_the_new_name_and_binaries_as_unknown() {
        let s = parse_numstat("3\t1\tsrc/a.rs\0-\t-\tlogo.png\0\x30\t0\t\0old.rs\0new.rs\0");
        assert_eq!(s.len(), 3);
        assert_eq!((s[0].added, s[0].removed), (Some(3), Some(1)));
        assert_eq!((s[1].added, s[1].removed), (None, None));
        assert_eq!(s[2].path, "new.rs");
    }
}
