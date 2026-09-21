# claude-fleet

A terminal multiplexer for Claude Code sessions. The left pane is the session
list, the right one is the full, interactive terminal of the selected session.

```
┌─ SESSIONS ────┬─ api-a6 ────────────────────────────┐
│ * web-c3      │ > fix the test in auth.spec.ts      │
│   .           │                                     │
│ o api-a6  ◀   │ ⏺ Read(src/auth.spec.ts)            │
│   apps/api    │   ⎿ Read 120 lines                  │
│               │                                     │
│ --- UNREACHABLE                                     │
│ ! dashboard…  │ ✻ Thinking… (7s · ↑ 1.2k tokens)    │
├───────────────┴─────────────────────────────────────┤
│ [F10] LEAVE FOCUS   F1-F9 session   F11 new         │
└─────────────────────────────────────────────────────┘
```

> **Unofficial.** claude-fleet is a community project and is not affiliated
> with, endorsed by, or supported by Anthropic. "Claude" and "Claude Code" are
> trademarks of Anthropic.
>
> Fleet reads Claude Code's **undocumented** local state (`~/.claude/sessions`,
> `~/.claude.json`, the `cc-msg` named pipes). Any Claude Code update can
> change those and break parts of fleet without notice.

## Requirements

- **Windows 10 1809 or newer** (ConPTY). Windows only — fleet is built on
  ConPTY and named pipes; it compiles elsewhere but session liveness and the
  restart supervisor assume Windows.
- [Claude Code](https://docs.claude.com/en/docs/claude-code) installed, with
  `claude` on `PATH` (or in `~/.local/bin`).
- To build from source: Rust 1.85 or newer (edition 2024).

## Install

Download `claude-fleet.exe` from the
[latest release](https://github.com/Sowiastyy/claude-fleet/releases/latest),
or build it yourself:

```
cargo install --git https://github.com/Sowiastyy/claude-fleet
```

Then run `claude-fleet` in any directory.

## How it works

Fleet is a **supervisor**: it spawns every session itself, as a child process
under a ConPTY of its own. That is what makes the pane fully interactive — keys
go straight to the child's stdin, and its output runs through a `vt100`
emulator.

Sessions started **outside** fleet are visible in the list, but greyed out and
marked `!`. Their PTY belongs to another terminal and on Windows there is no
taking it over. Fleet only reads their metadata.

## How fleet learns about sessions

Claude Code keeps a local registry, of which fleet is strictly a reader:

| path | contents |
|---|---|
| `~/.claude/sessions/<pid>.json` | name, cwd, `status` (`busy`/`idle`/`waiting`), `waitingFor`, `sessionId`, `messagingSocketPath` |
| `~/.claude/sessions/<pid>.<sha>.key` | `peerToken` for the pipe |
| `~/.claude/projects/<slug>/<sessionId>.jsonl` | transcript, append-only |
| `~/.claude.json` | `cachedUsageUtilization` — account limits and reset times |
| `\\.\pipe\LOCAL\cc-msg-<hash>` | named pipe, the one `SendMessage` runs over |

A registry file survives a process that was killed outright, so **liveness is
decided by whether the pipe exists**, not by the file.

The "recent projects" list in the new-session dialog does not decode the
directory names under `projects/` — that slug is lossy (`-` is both a separator
and a character inside names). Fleet reads the `cwd` field from the newest
transcript instead.

## Shortcuts

**Navigation** (left pane focused)

| key | action |
|---|---|
| `↑` `↓` / `j` `k` | select a session |
| `Enter` / `Tab` | enter the session |
| `n` | new session |
| `R` | resume an old conversation (see below) |
| `U` | refresh the account limits now (see below) |
| `u` | understand project (see below) |
| `r` | restart into a new build (see below) |
| `i` | install a newer release from GitHub (see below) |
| `x` | kill the selected session (with a confirmation) |
| `w` | close a finished session's card right away |
| `g` | browse the git panel (see below) |
| `G` | show or hide the git panel (hidden at start) |
| `[` / `]` | sidebar narrower / wider |
| `{` / `}` | git panel narrower / wider |
| `b` | switch the selected session's repository to another branch |
| `?` | help |
| `q` | quit |

## Git panel

The column on the right follows the selected session: it shows the
repository that session works in — the branch and how it stands against its
upstream (`↑` ahead, `↓` behind), the uncommitted changes, and the history.
Switch sessions and it switches repositories.

In the history, `•` marks commits made while the session was running, and a
hash in the warning colour is one not pushed yet. The panel reads `git
status` and `git log` every two seconds with `GIT_OPTIONAL_LOCKS=0`, so it
never holds the index lock a session wants for its own commit. It steps
aside on its own when the terminal is too narrow to keep the pane usable.

`b` (in the list or on the panel) opens the branches of that repository,
local ones first, then remote ones nobody has checked out yet, newest first.
Typing filters the list; `enter` runs `git switch`. A remote branch gets a
local one tracking it, and a name that matches no branch is offered as a new
branch started at HEAD. When git refuses — uncommitted changes in the way —
its reason lands on the status line and the list stays open.

At the foot of the panel sit a message box and three buttons, **Commit**,
**Push** and **Generate**, which the mouse can press too. On the panel:

| key | does |
|---|---|
| `c` | type the commit message (clicking the box does the same) |
| `m` | have a model write the message from the diff |
| `p` | push; a branch without an upstream is pushed to `origin` with `-u` |

In the message box `enter` commits, `shift+enter` (or `ctrl+j`) starts a new
line, `ctrl+g` generates, `ctrl+p` pushes, `ctrl+u` clears and `esc` goes
back to the list. A commit takes what is staged, or every change, untracked
files included, when nothing is. Generate runs `claude -p` with tools off on
the diff, using `claude-haiku-4-5` unless `[commit] model` in the config says
otherwise; it takes a few seconds and the button says so meanwhile.

## Session state on a card

A card shows what the session reports in the registry:

| badge | means |
|---|---|
| `working` (yellow) | `status: busy` — the model is working |
| `idle` (green) | `status: idle` — nothing is happening |
| **`question` (blue)** | `status: waiting` — the session is stopped on a question and will not move until someone answers |
| `starting` (grey) | the process is up, but has not written its registry entry yet |

`waiting` is a third status, not a flavour of `idle`: it belongs to a session
that has opened a dialog, a permission prompt, or is waiting on a choice. The
registry carries a `waitingFor` field with it (`dialog open`, `input needed`,
`permission prompt`) — the pane border prints it next to `QUESTION`, and
`--list` prints it in brackets after the status.

A finished session's card disappears by itself **a minute after the process
exits** — until then the list counts down with `gone in 42s`. `w` closes it
sooner.

**Focus** (keys go to Claude)

| key | action |
|---|---|
| **`F10`** | **leave focus** |
| `Alt+G` | straight to the git panel; `Alt+G` there comes back into the session |
| `Alt+Shift+G` | show or hide the git panel |
| `←` at the start of the input | leave focus too — the arrow has nowhere left to go in the box |
| everything else | goes to Claude, including `Ctrl+anything` |
| mouse wheel | scroll the history (see "Scrolling") |

A left click moves the keyboard to what it lands on, in the list, in focus and
on the git panel alike: a card selects that session, the pane goes into it, the
git panel starts browsing.

Dragging a column border with the mouse resizes the sidebar or the git panel.
The widths are kept in `~/.claude/fleet-layout` for the next start.

## Pasting

PTY input is queued and pushed out in 8 KB chunks by a writer thread of its
own, so pasting a megabyte does not stall the UI — the pane keeps redrawing,
`F10` still leaves focus, and the pane border shows `pasting 1.4 MB` counting
the rest of the queue down.

Terminal input is read by a dedicated thread — nothing else touches the event
queue. The console input queue is a fixed-size ring, and records that land in
it while nobody is reading are lost without a trace. Reading from the event
loop meant every redraw dropped a piece of the paste — mid-word.

crossterm only assembles `Event::Paste` from the Unix input stream, so on
Windows a paste **always** arrives as an avalanche of key presses. Fleet
therefore collects a burst of plain characters (up to 64 KB per chunk) and
sends it as one paste. More than 8 characters in a burst is a paste, fewer is
typing. Modified keys, arrows and function keys never join such a burst.

A burst does not end at the first empty queue. A paste does not arrive in one
run but in waves with gaps between them; cutting at the first gap split a
single prompt into dozens of pieces, and every piece outside the paste brackets
sends itself on its own `\n`. A silence shorter than 60 ms therefore still
belongs to the burst.

A paste too large for one chunk goes out in several, but the `ESC[200~` /
`ESC[201~` brackets belong to the whole, not to a chunk: opened on the first,
closed on the last. While a paste is open, every input is its continuation —
including a tail shorter than 8 characters that fell just outside the silence
window. The first real key press closes it.

**Always** (in every mode)

| key | action |
|---|---|
| `F1`-`F9` | jump to a session — selects it and enters focus at once |
| `F<n+1>` | the first free slot starts a new session right away |
| `F11` | new session (with the dialog) |
| `F12` | help |

With two sessions, `F1` and `F2` jump, and `F3` — the first free slot — starts a
third session with no dialog at all, in the selected session's directory.
Further keys (`F4` and up) say which slot is next. The dialog with the
directory picker is on `F11` and `n`.

When the path typed into the dialog does not point at an existing directory,
fleet asks `create it?` — `y`/`enter` creates it (with any missing parents) and
starts the session, `n`/`esc` returns to the form with the path still in place.

Only the function keys are reserved — no fleet shortcut sits on a modifier.
Claude Code binds plenty of `Ctrl` combinations itself (`Ctrl+B` for background
tasks, among others), so a tmux-style prefix would swallow keys meant for the
session. The `u` in the section below is a sequence of plain keys rather than a
combination, and outside focus mode there is nothing to swallow anyway.
Since the function keys work everywhere regardless, there is always a way out
of a pane that is eating input.

`F10` is printed on the pane border in focus mode, not only in the help.

## understand project

`u` puts the text `understand project` into the prompt of the selected or a new
session — **without pressing enter**. The text lands in the box; adding details
and sending it stays with the human.

The chord works in both directions, because that is how people reach for it:

| sequence | effect |
|---|---|
| `u` `F1`..`F9` | that session; a free slot = a new session and the prompt right away |
| `u` `u` | new session in the first free slot, no directory dialog |
| `u` `n` | new session with the directory dialog |
| `u` `enter` | the selected session |
| `F1`..`F9`, then `u` | the same thing, the other way round |

Once `u` is armed, **every** key is an answer: it either names a target or
cancels the chord. An accidental press therefore starts no session and kills
nobody.

The reverse order runs on a time window: for 2 seconds after entering a
session, a lone `u` is the chord rather than a letter. Lone — a `u` that starts
a word arrives in a burst with the other letters and goes to Claude as usual.

A session spawned a moment ago has no prompt box yet and loses anything typed
into it, so the text waits in the session and goes in when `❯` appears on
screen (or the box border, in older versions). Should it never appear, the text
goes anyway after 8 seconds — a heuristic that misses has no business
swallowing a paste forever. To check it live: `--selftest understand`.

## Resuming conversations

`R` in the list opens the transcripts from `~/.claude/projects` — the twenty
most recent conversations, newest first:

```
╭ resume a conversation ──────────────────────────────────────────────╮
│ enter resumes, esc closes                                           │
│ > claude-fleet    make the limit progress bar bigger, add resume   2m│
│   billing-api     add a date filter to the orders view             5h│
│   pdf-tools       Batch print manager — build spec                 2d│
╰─────────────────────────────────────────────────────────────────────╯
```

`Enter` starts a new session in that conversation's directory with
`--resume <id>` — Claude Code loads the full history and keeps writing to the
same transcript. That is the only thing fleet does here: `claude` handles all
the rest, including the case where that conversation happens to be running
somewhere (it starts a copy and says so).

A conversation's name is its **first real user message**: no
`<system-reminder>`, no slash-command names, no tool results and no subagent
lines (`isSidechain`). Rows without such a message do not reach the list at all
— a session opened and closed without a word has nothing to resume.

Where the id comes from: the file name
`~/.claude/projects/<dir-with-dashes>/<id>.jsonl` is exactly what `--resume`
takes. The directory in that name is the working path with every non-alphanumeric
character replaced by `-`. `claude-fleet --history` prints the same list without
starting the panel — for when the question is "why is that conversation not
there".

## Live changes

Two different things, because they cost different amounts.

**Config — no restart, sessions live on.** Colours, the words on cards, the
text of the `understand project` prompt and the timings live in
`~/.claude/fleet.toml` (overridden by `CLAUDE_FLEET_CONFIG`). The file is
written on the first start and belongs to you from then on — fleet never
overwrites it. Its timestamp is checked four times a second, so a save shows up in the
next redraw; the panel says `config reloaded`.

Broken TOML **does not wipe the palette**: the previous values stand and the
status bar shows `config rejected: <error>`. A config is edited in place, and
half a line mid-keystroke has no business blanking the screen. An unknown key
is an error too — a typo that parsed would look like a setting with no effect.

**Code — a restart; processes die, conversations do not.** `r` in the list
restarts fleet into a new build. When sessions are alive, fleet asks first.
The processes themselves cannot be saved: a child under a ConPTY dies with the
process that created that pseudoconsole (`--orphan-probe` shows this), and a
`HPCON` is not transferable between processes — there is nothing to hand over.
Surviving processes would mean moving the PTY into a separate host process;
that is not here.

What does come back is the **conversation**. Before exiting, fleet appends the
transcript id of every live session to a restore file — the `sessionId` the
registry records next to that session's pid, so several sessions in one
directory each get their own conversation back — and on return starts
them with `claude --resume <id>` — same directory, same history, new process.
The status bar shows `3 sessions came back after the restart, with their
conversations`. When a transcript cannot be pinned down, that session comes
back empty and the counter in the message says so.

### How that works

The process you start is the **supervisor**: it copies the build output to a
temporary file and runs that copy in the same console. A copy asking for a
restart exits with code `75`, and the loop goes round again with whatever is
sitting at the build path by then. They never run at the same time, so nothing
fights over the terminal.

The copy is the point of the whole construction: **a running exe is locked on
Windows**, so a fleet started straight out of `target` breaks the next
`cargo build`. The supervisor does keep **its own** file open, though, so the
two paths have to differ:

```
claude-fleet.exe                     <- what you run (the supervisor)
target/release/claude-fleet.exe      <- what it watches and copies from
```

With that layout fleet finds the build output by itself — it takes the newer of
`target/release` and `target/debug` next to it. `CLAUDE_FLEET_BUILD` points at
it by hand. When the two are the same file, fleet says so at startup, because
no copying further down will help then.

The working loop therefore looks like this: `cargo build --release`, fleet
notices the new file within a second and prints `new build ready`, `r` moves
into the new version. Nothing has to be copied by hand.

Copies left behind by a fleet that was killed rather than closed are cleaned up
by the next start: trying to delete one is at the same time the test of whether
it is still running — Windows will not delete a running file.

## Running

```
claude-fleet [DIR]         TUI, current directory by default
claude-fleet --list        print the running sessions and exit
claude-fleet --help        help
```

Diagnostics: `--pipes` (names of the open pipes), `--selftest [ui]` (run
`claude` under a PTY and show its screen), `--selftest understand` (check that
queued text really reaches the child's prompt), `--selftest usage` (ask Claude
Code for fresh limit numbers once and report what happened), `--raw <prog> [args]` (raw
bytes from any program under a PTY), `--mouse` (whether this terminal hands
wheel events to the application at all), `--usage` (account limits as the
sidebar reads them), `--history` (conversations to resume, as the `R` list
reads them).

`--raw` answers DSR itself — otherwise the child stalls at startup and the
probe shows exactly the four bytes you are asking about (see pitfall 1).

## Building

```
cargo build --release
cargo test
```

The binary lands in `target/release/claude-fleet.exe`. For the restart loop
described under "Live changes", copy it once next to `target` and run that
copy.

On the `x86_64-pc-windows-gnu` toolchain, see "Build environment" below.

## Updates

A fleet started from a downloaded `claude-fleet.exe` asks the GitHub API for
the latest release at startup and every half hour after; `i` asks at once when
nothing newer is known yet. When there is a newer
one the sidebar shows `^ v0.2.0 available`, and `i` downloads it, puts it in
place of the exe the fleet was started from and asks for the usual restart —
sessions come back with their conversations. The exe that was running is moved
aside as `claude-fleet.old-<ms>.exe` (a running file cannot be overwritten on
Windows, only renamed) and swept on a later start.

Requests go through the `curl.exe` that ships with Windows. A fleet running out
of `target/release` or `target/debug` never updates itself: that file belongs
to cargo. To switch the check off:

```toml
[updates]
check = false
```

### Releases

Every push to `main` that changes the program is released by the `Release`
workflow: tested, built, tagged and published with `claude-fleet.exe`
attached. Pushes that only touch Markdown and repo housekeeping are skipped.

The version is the next patch after the newest `v*` tag. For a minor or major
step, set `version` in `Cargo.toml` above that tag and it is used instead. The
number is stamped into the build only; nothing is committed back.

## Account limits

Two bars sit at the bottom of the left pane: the five-hour window (`session`)
and the seven-day one (`weekly`), each with a percentage spent and the time
until it resets.

```
 LIMITS
 session                    21%   3h44m
 ██████▌░░░░░░░░░░░░░░░░░░░░░░░░
 weekly                     31%  11h34m
 █████████▉░░░░░░░░░░░░░░░░░░░░░
```

The bar runs the full width of the pane, on a line of its own under the
numbers. The fill is counted in eighths of a block (`▏▎▍▌▋▊▉█`), so movement
shows between whole cells — otherwise the bar would stand still for a quarter
of an hour at a time.

These are the same numbers `/usage` shows in Claude Code, and fleet **asks
nobody for them**: Claude Code keeps them in `~/.claude.json` under
`cachedUsageUtilization`, together with each window's `resets_at` and a
`fetchedAtMs` stamp. Fleet is a reader here exactly as it is for the session
registry.

The file is over a hundred kilobytes and almost never changes, so it is parsed
only when its mtime moves; the countdown to a reset is computed from what has
already been parsed, on every list refresh.

When a reset time has passed, the whole row goes faint and the time column says
`stale` — the percentage then describes a window that is over. With no cache at
all (before the first login, say) the footer is not drawn.

### Keeping the numbers current

Only Claude Code writes that cache, and it writes it when it feels like it — on
a quiet machine the numbers can be hours old, which is a percentage that means
nothing. So when they go stale, fleet asks for new ones the only way that is
documented: it drives a session the way a person would.

A hidden `claude` is spawned under a PTY exactly like any other session, `/usage`
is typed into it, and fleet waits for `fetchedAtMs` in `~/.claude.json` to move.
Then the child is killed. It takes about two seconds. `/usage` is a slash
command, so Claude Code answers it itself: `total_cost_usd` is `0`, `num_turns`
is `0` — the refresh costs no tokens.

Fleet stays a reader throughout. It does not know the usage endpoint, holds no
token, and writes nothing into the cache — the numbers are put there by Claude
Code, with absolute `resets_at` times in them, as always.

Two clocks decide when to ask:

| situation | asked again after |
|---|---|
| a session of ours has been working since the last fetch | 6 minutes |
| nothing here has run since then | 30 minutes |

Nothing running means nothing of ours spending, and the long clock is there only
because usage can move elsewhere — another machine, claude.ai. `U` asks straight
away.

While the hidden session is alive the footer heading says `refreshing`, and that
session is kept off the list: it registers itself like any other, and showing it
would be the panel reporting its own bookkeeping as somebody's work.

An attempt that changes nothing is normal. Claude Code answers `/usage` from
numbers of its own for a few minutes before it asks upstream again, so a cache
that is already recent sits still — which is why the short clock is six minutes
and not one. Nothing is reported when that happens; the next attempt is five
minutes out anyway.

`claude-fleet --selftest usage` runs one attempt and prints what it did, with
the hidden session's screen when nothing moved.

When nobody has refreshed the numbers for more than twenty minutes, the heading
says `1h ago` instead of pretending they are current.

The percentage follows the usage: green up to 50%, yellow up to 85%, red above
that. The bar itself is the accent colour — its length says what the colour
would say, so it does not say it twice. By default it takes `theme.accent` and
`theme.accent_dim` from the config, which means changing one colour moves the
whole chrome along with the bar; `theme.bar` and `theme.bar_empty` give it
colours of its own when that is what you want:

```toml
[theme]
accent     = "#D97757"   # the bar and every other accent
accent_dim = "#8A4C36"   # the empty part of the bar
# bar       = "#6EA87A"  # the bar only, independent of the accent
# bar_empty = "#2A3A2E"
```

## Scrolling

The wheel has two possible recipients, and the child decides which one it is.

Claude Code usually renders in the normal buffer: history leaves through the
top of the screen and lands in the emulator's scrollback, so the wheel moves
our own view (`set_scrollback`) and the pane border shows `^12`.

But when Claude Code switches to the **alternate screen** (`ESC[?1049h`) and
turns on its own mouse tracking (`ESC[?1000h`…`ESC[?1006h`), both halves of
that mechanism disappear at once: the alternate `vt100` grid is created with
zero scrollback, so there is nothing to scroll, and the history is inside the
child anyway, not with us. The only thing that works then is passing the wheel
on — the way a real terminal does, as an SGR report `ESC[<64;col;rowM`.

So fleet looks at the emulator's `mouse_protocol_mode()`: a child that asked
for the mouse gets a wheel notice on the PTY (coordinates relative to the
inside of the pane, counting from 1); a child that did not ask stays with our
scrollback. That notice goes through `write_passthrough` rather than the usual
`write_input`, because the wheel is not a key press and has no business pulling
the view back to the end.

The wheel over the session list moves the selection, not the terminal.

If nothing scrolls in any mode, the question is one level earlier: does the
terminal fleet is running in hand wheel events to applications at all?
`claude-fleet --mouse` answers that.

## Two pitfalls this code answers

**1. ConPTY blocks without an answer to DSR.** Right after startup, ConPTY
sends `ESC[6n` (a cursor position request) and **waits** for the terminal's
answer. An emulator that does not answer hangs every child at startup — the
process looks dead and has produced exactly 4 bytes. The `dsr` module scans the
child's output and sends back `ESC[<row>;<col>R`, `ESC[0n` and answers to
Device Attributes. That is why the PTY writer is shared (`Arc<Mutex<…>>`): both
the UI thread and the reading thread answer on it.

**2. A named pipe's name contains a backslash.** The pipe namespace is flat and
`\` is an ordinary character in it. `\\.\pipe\LOCAL\cc-msg-abc` appears as the
single name `LOCAL\cc-msg-abc`, so trimming to the last path segment never
matches. Only the `\\.\pipe\` prefix may be stripped.

## Build environment

The `x86_64-pc-windows-msvc` toolchain (with the VS Build Tools) needs nothing
special.

On `x86_64-pc-windows-gnu`, an old 32-bit `dlltool` earlier in `PATH` (a
leftover `C:\MinGW\bin`, typically) wins over the 64-bit one and the build
fails inside `windows-sys`:

```
dlltool could not create import library ... Invalid bfd target
```

`build.cmd` is a thin wrapper around `cargo` that puts a 64-bit mingw toolchain
ahead of it in `PATH` for the duration of the build only. It defaults to the
WinLibs package installed by winget; set `MINGW64` to point it elsewhere:

```
set MINGW64=C:\path\to\mingw64\bin
build.cmd build --release
```

Permanent fixes: move the old MinGW behind the 64-bit one in the system `PATH`,
or switch to the msvc toolchain.

## What is not here

- **Sending messages to foreign sessions.** It would need the `cc-msg`
  protocol, which is undocumented. For our own sessions it is unnecessary —
  writing to the PTY's stdin does exactly the same thing.
- **Resurrecting a session process.** The conversation comes back through
  `claude --resume`, but the process is new — screen, scrollback and tool state
  start from zero.
- **git worktree integration.** Two sessions in one working tree mix up each
  other's state; fleet does not police that.

## License

MIT — see [LICENSE](LICENSE).
