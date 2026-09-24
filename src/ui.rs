//! All rendering. The pane is a `tui-term` widget over the session's vt100
//! screen; everything else is chrome drawn around it.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use tui_term::widget::{Cursor, PseudoTerminal};

use crate::{
    app::{App, BranchRow, GitHit, GitJob, Mode, NewSessionForm, SpawnKind},
    bigbrother::{Level, Scope},
    commitmsg, config, editor, git,
    gitview::{GitView, Row},
    ide::PromptKind,
    msgedit, splash,
    syntax::{self, Tok},
    theme, usage,
};

pub const SIDEBAR_WIDTH: u16 = 36;

/// The git panel on the right. Wide enough for a hash, most of a commit
/// subject and its age.
pub const GIT_WIDTH: u16 = 44;

/// The narrowest either side column may be dragged to. Below this the cards
/// and the git rows stop saying anything.
pub const SIDEBAR_MIN: u16 = 28;
pub const GIT_MIN: u16 = 32;

/// The widths in use, which the mouse can drag away from the defaults above.
/// Global rather than on `App` because every draw function sizes its rows by
/// them, and threading two numbers through all of them buys nothing.
static SIDEBAR_W: AtomicU16 = AtomicU16::new(SIDEBAR_WIDTH);
static GIT_W: AtomicU16 = AtomicU16::new(GIT_WIDTH);

pub fn sidebar_width() -> u16 {
    SIDEBAR_W.load(Ordering::Relaxed)
}

pub fn git_width() -> u16 {
    GIT_W.load(Ordering::Relaxed)
}

/// Set the sidebar's width, kept wide enough to read and narrow enough to
/// leave the pane (and the git panel, when it is up) their room.
pub fn set_sidebar_width(w: u16, full: Rect, show_git: bool) {
    let others = if show_git && git_fits(full) {
        PANE_MIN_WITH_GIT + git_width()
    } else {
        PANE_MIN
    };
    let max = full.width.saturating_sub(others).max(SIDEBAR_MIN);
    SIDEBAR_W.store(w.clamp(SIDEBAR_MIN, max), Ordering::Relaxed);
}

/// Set the git panel's width. It never grows past the point where it would no
/// longer fit, or dragging it wider would make it disappear.
pub fn set_git_width(w: u16, full: Rect) {
    let max = full
        .width
        .saturating_sub(sidebar_width() + PANE_MIN_WITH_GIT)
        .max(GIT_MIN);
    GIT_W.store(w.clamp(GIT_MIN, max), Ordering::Relaxed);
}

/// The widths as they were last left, read back at start-up.
pub fn load_widths() {
    let Some(raw) = widths_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return;
    };
    let mut it = raw.split_whitespace().map(str::parse::<u16>);
    if let Some(Ok(w)) = it.next() {
        SIDEBAR_W.store(w.max(SIDEBAR_MIN), Ordering::Relaxed);
    }
    if let Some(Ok(w)) = it.next() {
        GIT_W.store(w.max(GIT_MIN), Ordering::Relaxed);
    }
}

/// Remember the widths for the next start. Losing them is no great harm, so a
/// failed write is not reported.
pub fn save_widths() {
    if let Some(p) = widths_path() {
        let _ = std::fs::write(
            p,
            format!(
                "{} {}
",
                sidebar_width(),
                git_width()
            ),
        );
    }
}

fn widths_path() -> Option<std::path::PathBuf> {
    Some(dirs::home_dir()?.join(".claude").join("fleet-layout"))
}

/// The narrowest the terminal pane may get before the git panel steps aside
/// for it: Claude Code's own layout starts breaking up below about this.
const PANE_MIN_WITH_GIT: u16 = 70;
/// The narrowest the pane may get with the git panel hidden.
const PANE_MIN: u16 = 20;

/// Columns kept clear either side of a limit bar, so it never runs into the
/// sidebar's border.
const USAGE_GUTTER: usize = 1;
/// Eighth-blocks, narrowest first. A bar this wide still moves a whole cell
/// only every few percent; the partial cell is what keeps a slow window
/// visibly moving between them.
const EIGHTHS: [char; 7] = [
    '\u{258f}', '\u{258e}', '\u{258d}', '\u{258c}', '\u{258b}', '\u{258a}', '\u{2589}',
];
/// Past this, the cached limits are old enough that the footer says so instead
/// of quietly showing them as current.
const USAGE_STALE: Duration = Duration::from_secs(20 * 60);

pub fn draw(f: &mut Frame, app: &mut App) {
    let [body, status] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
    let (sidebar, pane, git) = columns(body, app.show_git);

    // Rebuilt by whatever draws a clickable part this frame; a hidden panel
    // leaves nothing to click.
    app.git_hits.clear();
    draw_sidebar(f, app, sidebar);
    if app.mode == Mode::Ide {
        draw_ide(f, app, pane);
    } else if app.mode.on_git() && app.git_view.preview_open {
        draw_git_preview(f, app, pane);
    } else {
        draw_pane(f, app, pane);
    }
    if let Some(git) = git {
        draw_git(f, app, git);
    }
    draw_status(f, app, status);

    match app.mode {
        Mode::NewSession => draw_new_session(f, app),
        Mode::Help => draw_help(f),
        Mode::ConfirmKill => draw_confirm_kill(f, app),
        Mode::ConfirmRestart => draw_confirm_restart(f, app),
        Mode::ConfirmMkdir => {
            // The form stays visible underneath: the question is about the path
            // still standing in it.
            draw_new_session(f, app);
            draw_confirm_mkdir(f, app);
        }
        Mode::Resume => draw_resume(f, app),
        Mode::Branch => draw_branches(f, app),
        Mode::Tag => draw_tag(f, app),
        Mode::BigBrother => draw_big_brother(f, app),
        Mode::Reports => draw_reports(f, app),
        Mode::PushFailed => draw_push_failed(f, app),
        _ => {}
    }
}

/// The pane rectangle for a given terminal size, so the event loop can resize
/// PTYs without waiting for a render.
pub fn pane_area(full: Rect, show_git: bool) -> Rect {
    let [body, _] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(full);
    columns(body, show_git).1
}

/// Whether a terminal this size has room for the git panel next to a pane
/// still worth typing into.
pub fn git_fits(full: Rect) -> bool {
    full.width >= sidebar_width() + PANE_MIN_WITH_GIT + git_width()
}

/// Sidebar, pane, and the git panel when it is wanted and fits.
fn columns(body: Rect, show_git: bool) -> (Rect, Rect, Option<Rect>) {
    if show_git && git_fits(body) {
        let [sidebar, pane, git] = Layout::horizontal([
            Constraint::Length(sidebar_width()),
            Constraint::Min(PANE_MIN),
            Constraint::Length(git_width()),
        ])
        .areas(body);
        return (sidebar, pane, Some(git));
    }
    let [sidebar, pane] = Layout::horizontal([
        Constraint::Length(sidebar_width()),
        Constraint::Min(PANE_MIN),
    ])
    .areas(body);
    (sidebar, pane, None)
}

/// Inner size of the terminal pane, which is what a PTY must be resized to.
pub fn pane_inner(area: Rect) -> (u16, u16) {
    let inner = pane_inner_rect(area);
    (inner.height.max(1), inner.width.max(1))
}

/// The pane's inner rectangle, borders excluded. A mouse event carries screen
/// coordinates, and a child that tracks the mouse wants them pane-relative.
pub fn pane_inner_rect(area: Rect) -> Rect {
    Block::bordered().inner(area)
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    // The list has the keyboard unless a session or the git panel took it.
    // Exactly one column is lit at a time, so it is plain where keys go.
    let focused = !matches!(app.mode, Mode::Focus | Mode::Git | Mode::Commit | Mode::Ide);
    let mut items: Vec<ListItem> = Vec::new();

    for (i, s) in app.sessions.iter().enumerate() {
        let entry = app.entry_for(i);
        let (glyph, glyph_color, state_text) = if !s.is_alive() {
            // Finished cards expire on their own; show how long they have left.
            let ttl = config::finished_ttl();
            let left = s
                .finished_for()
                .map(|d| ttl.saturating_sub(d))
                .unwrap_or(ttl);
            ("x", theme::dead(), fmt_countdown(left))
        } else if s.shell {
            // Never registers, so it has no busy or idle to tell.
            ("$", theme::muted(), "shell".to_string())
        } else {
            let labels = config::labels();
            match entry.map(|e| e.status.as_str()) {
                Some("busy") => ("*", theme::busy(), labels.busy),
                Some("idle") => ("o", theme::idle(), labels.idle),
                // Stopped on a dialog or a permission prompt: nothing is
                // running and nothing will, until someone answers it.
                Some("waiting") => ("?", theme::ask(), labels.waiting),
                // Spawned, but has not written its registry entry yet.
                _ => ("-", theme::muted(), labels.starting),
            }
        };

        // A Big Brother stands out from the sessions it watches.
        let (glyph, glyph_color) = if s.watch.is_some() && s.is_alive() {
            ("@", theme::accent())
        } else {
            (glyph, glyph_color)
        };
        let name = match (&s.watch, entry) {
            (Some(_), _) | (None, None) => s.label.clone(),
            (None, Some(e)) => e.name.clone(),
        };
        let name = truncate(
            &name,
            usize::from(sidebar_width()).saturating_sub(20).max(8),
        );
        let hotkey = if i < 9 {
            format!("F{}", i + 1)
        } else {
            String::new()
        };
        let used = 4 + name.chars().count() + hotkey.chars().count();
        let pad = usize::from(sidebar_width()).saturating_sub(used + 2);

        let name_style = if i == app.selected {
            Style::default()
                .fg(theme::text())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::text())
        };

        items.push(ListItem::new(vec![
            Line::from(vec![
                Span::raw(" "),
                Span::styled(glyph, Style::default().fg(glyph_color)),
                Span::raw(" "),
                Span::styled(name, name_style),
                Span::raw(" ".repeat(pad)),
                Span::styled(hotkey, Style::default().fg(theme::faint())),
            ]),
            Line::from(vec![
                Span::raw("   "),
                match (&s.watch, s.group) {
                    (None, Some(g)) => Span::styled(
                        format!("[{g}] "),
                        Style::default().fg(group_color(g)).bold(),
                    ),
                    _ => Span::raw(""),
                },
                match &s.watch {
                    Some(w) => Span::styled(
                        match w.scope {
                            Scope::All => "watching all".to_string(),
                            Scope::Group(g) => format!("watching [{g}]"),
                        },
                        Style::default().fg(theme::accent()),
                    ),
                    None => Span::styled(
                        truncate(&s.cwd_label(), 12),
                        Style::default().fg(theme::muted()),
                    ),
                },
                Span::raw(" "),
                Span::styled(state_text, Style::default().fg(glyph_color)),
                Span::raw(" "),
                Span::styled(
                    fmt_uptime(s.started.elapsed()),
                    Style::default().fg(theme::faint()),
                ),
            ]),
        ]));
    }

    // The next free function key is a button as much as the cards above it are,
    // so it gets a row of its own rather than living only in the help.
    if !app.sessions.is_empty() && app.sessions.len() < 9 {
        items.push(ListItem::new(Line::from(vec![
            Span::styled(" + ", Style::default().fg(theme::accent())),
            Span::styled("new session", Style::default().fg(theme::muted())),
            Span::raw(
                " ".repeat(
                    usize::from(sidebar_width()).saturating_sub(3 + "new session".len() + 4),
                ),
            ),
            Span::styled(
                format!("F{}", app.sessions.len() + 1),
                Style::default().fg(theme::faint()),
            ),
        ])));
    }

    if app.unread_reports > 0 {
        let color = match app.unread_level {
            Some(Level::Alarm) => theme::dead(),
            Some(Level::Warn) => theme::busy(),
            _ => theme::ask(),
        };
        let plural = if app.unread_reports == 1 { "" } else { "s" };
        let label = format!("{} new report{plural}", app.unread_reports);
        items.push(ListItem::new(Line::from(vec![
            Span::styled(" @ ", Style::default().fg(color)),
            Span::styled(label.clone(), Style::default().fg(color).bold()),
            Span::raw(" ".repeat(
                usize::from(sidebar_width()).saturating_sub(3 + label.chars().count() + 3),
            )),
            Span::styled("A", Style::default().fg(color)),
        ])));
    }

    if let Some(rel) = app.release.as_ref().filter(|_| !app.update_ready) {
        let (label, key) = if app.update_busy() {
            (format!("{} downloading", rel.tag), "")
        } else {
            (format!("{} available", rel.tag), "i")
        };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(" ^ ", Style::default().fg(theme::ask())),
            Span::styled(label.clone(), Style::default().fg(theme::ask()).bold()),
            Span::raw(" ".repeat(
                usize::from(sidebar_width()).saturating_sub(3 + label.chars().count() + 3),
            )),
            Span::styled(key, Style::default().fg(theme::ask())),
        ])));
    }

    if app.update_ready {
        items.push(ListItem::new(Line::from(vec![
            Span::styled(" ^ ", Style::default().fg(theme::ask())),
            Span::styled("new build", Style::default().fg(theme::ask()).bold()),
            Span::raw(
                " ".repeat(usize::from(sidebar_width()).saturating_sub(3 + "new build".len() + 3)),
            ),
            Span::styled("r", Style::default().fg(theme::ask())),
        ])));
    }

    let foreign = app.foreign();
    if !foreign.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            " --- UNREACHABLE",
            Style::default().fg(theme::faint()),
        ))));
        for e in foreign {
            let color = match e.status.as_str() {
                "busy" => theme::busy(),
                "waiting" => theme::ask(),
                _ => theme::idle(),
            };
            let state = if e.is_waiting() && !e.waiting_for.is_empty() {
                format!("{}: {}", config::labels().waiting, e.waiting_for)
            } else if e.is_waiting() {
                config::labels().waiting
            } else {
                e.status.clone()
            };
            items.push(ListItem::new(vec![
                Line::from(vec![
                    Span::styled(" ! ", Style::default().fg(theme::faint())),
                    Span::styled(truncate(&e.name, 20), Style::default().fg(theme::muted())),
                ]),
                Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        truncate(&e.cwd_label(), 12),
                        Style::default().fg(theme::faint()),
                    ),
                    Span::raw(" "),
                    Span::styled(truncate(&state, 12), Style::default().fg(color).dim()),
                ]),
            ]));
        }
    }

    if items.is_empty() {
        items.push(ListItem::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  no sessions",
                Style::default().fg(theme::muted()),
            )),
            Line::from(Span::styled(
                "  press n",
                Style::default().fg(theme::faint()),
            )),
        ]));
    }

    // The accent border marks which half of the screen has the keyboard.
    let border_color = if focused {
        theme::accent()
    } else {
        theme::faint()
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Line::from(vec![
            Span::styled(" CLAUDE ", Style::default().fg(theme::accent()).bold()),
            Span::styled("FLEET ", Style::default().fg(theme::muted())),
        ]))
        .title_bottom(Line::from(Span::styled(
            format!(" {} sessions ", app.sessions.len()),
            Style::default().fg(theme::faint()),
        )));

    let list = List::new(items).highlight_style(
        Style::default()
            .bg(theme::surface())
            .fg(theme::text())
            .add_modifier(Modifier::BOLD),
    );

    let mut state = ListState::default();
    if !app.sessions.is_empty() {
        state.select(Some(app.selected));
    }

    // The limits are a footer, not a card, so they take the bottom rows and the
    // list takes what is left. That means drawing the frame first and putting
    // two widgets inside it, rather than letting the list own the block.
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut limits = usage_lines(app, inner.width as usize);
    // A sidebar too short for both keeps the sessions: the limits are the half
    // one can also read with `/usage`.
    let mut reserved = (limits.len() as u16).min(inner.height.saturating_sub(4));
    if reserved < limits.len() as u16 {
        // Each window is a heading and the bar under it. Half a window reads as
        // a bug, so what does not fit whole is dropped whole.
        reserved = if reserved >= 3 {
            1 + (reserved - 1) / 2 * 2
        } else {
            0
        };
        limits.truncate(reserved as usize);
    }
    let [list_area, limits_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(reserved)]).areas(inner);

    f.render_stateful_widget(list, list_area, &mut state);
    if reserved > 0 {
        f.render_widget(Paragraph::new(limits), limits_area);
    }
}

/// The account's rate-limit windows as the rows at the foot of the sidebar:
/// how much of each window is spent, and how long until it resets.
///
/// Nothing is drawn when Claude Code has never cached them. An empty footer
/// beats dashes standing in for numbers nobody has.
fn usage_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let Some(u) = app.usage.current.as_ref() else {
        return Vec::new();
    };
    if u.session.is_none() && u.weekly.is_none() {
        return Vec::new();
    }

    // Claude Code refreshes that cache; fleet only reads it — but it does ask
    // for a refresh when the numbers go stale, and that ask is worth showing:
    // the age stops moving for a moment and then jumps.
    let stale = if app.usage_refreshing() {
        Some("refreshing ".to_string())
    } else {
        (u.fetched_ago > USAGE_STALE).then(|| format!("{} ago ", fmt_uptime(u.fetched_ago)))
    };
    let title = " LIMITS";
    let used = title.chars().count() + stale.as_ref().map_or(0, |s| s.chars().count());
    let mut head = vec![
        Span::styled(title, Style::default().fg(theme::muted())),
        Span::raw(" ".repeat(width.saturating_sub(used))),
    ];
    if let Some(stale) = stale {
        head.push(Span::styled(stale, Style::default().fg(theme::faint())));
    }

    let mut lines = vec![Line::from(head)];
    for (label, window) in [("session", &u.session), ("weekly", &u.weekly)] {
        if let Some(w) = window {
            lines.extend(usage_rows(label, w, width));
        }
    }
    lines
}

/// One window: what it is and how much of it is left, then a bar across the
/// whole sidebar under it.
///
/// Two lines rather than one, because a bar squeezed in beside the numbers had
/// five cells to say everything in — a third spent and half spent drew the
/// same.
fn usage_rows(label: &str, w: &usage::Window, width: usize) -> Vec<Line<'static>> {
    // A window whose reset time has passed describes a window that is over, so
    // its percentage belongs to that one too. The rows go faint rather than
    // pretending the number is about the window running now.
    //
    // The bar itself is the accent and always the accent: it says how much is
    // spent by its length, so a colour that changed with the number would be
    // saying the same thing twice. The percentage keeps the green-to-red
    // warning, which is the part a glance reads.
    let (label_color, value_color, bar_color, empty_color) = if w.expired {
        (
            theme::faint(),
            theme::faint(),
            theme::faint(),
            theme::surface(),
        )
    } else {
        (
            theme::muted(),
            usage_color(w.pct),
            theme::bar(),
            theme::bar_empty(),
        )
    };

    let head = format!(" {label}");
    let pct = format!("{:>3}%", w.pct);
    let reset = format!("{:>6} ", fmt_reset(w));
    let used = head.chars().count() + pct.chars().count() + reset.chars().count() + 1;

    let (filled, empty) = usage_bar(w.pct, width.saturating_sub(USAGE_GUTTER * 2));

    vec![
        Line::from(vec![
            Span::styled(head, Style::default().fg(label_color)),
            Span::raw(" ".repeat(width.saturating_sub(used))),
            Span::styled(pct, Style::default().fg(value_color).bold()),
            Span::raw(" "),
            Span::styled(reset, Style::default().fg(theme::faint())),
        ]),
        Line::from(vec![
            Span::raw(" ".repeat(USAGE_GUTTER)),
            Span::styled(filled, Style::default().fg(bar_color)),
            Span::styled(empty, Style::default().fg(empty_color)),
            Span::raw(" ".repeat(USAGE_GUTTER)),
        ]),
    ]
}

/// The filled and unfilled halves of a bar `width` cells wide.
///
/// The fill is measured in eighths so the last cell can be a partial block: a
/// whole cell is three percent or so at this width, and a bar that only moved
/// in whole cells would sit still for a quarter of an hour at a time.
fn usage_bar(pct: u8, width: usize) -> (String, String) {
    let eighths = usize::from(pct.min(100)) * width * 8 / 100;
    let whole = (eighths / 8).min(width);
    let rest = eighths % 8;

    let mut filled = "\u{2588}".repeat(whole);
    if whole < width && rest > 0 {
        filled.push(EIGHTHS[rest - 1]);
    }
    let empty = "\u{2591}".repeat(width - filled.chars().count());
    (filled, empty)
}

fn usage_color(pct: u8) -> Color {
    match pct {
        0..=49 => theme::idle(),
        50..=84 => theme::busy(),
        _ => theme::dead(),
    }
}

/// How long until a window resets, in the widest unit that still says
/// something: `2h14m`, `47m`, `6d3h`.
fn fmt_reset(w: &usage::Window) -> String {
    if w.expired {
        return "stale".to_string();
    }
    let Some(d) = w.resets_in else {
        return "-".to_string();
    };
    let mins = d.as_secs() / 60;
    match mins {
        0..=59 => format!("{mins}m"),
        60..=1439 => format!("{}h{:02}m", mins / 60, mins % 60),
        _ => format!("{}d{}h", mins / 1440, (mins % 1440) / 60),
    }
}

/// The pane with nothing to show: the fleet's name turning above the keys that
/// start something. A pane too small for the letters keeps only the keys.
fn draw_empty_pane(f: &mut Frame, area: Rect) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::faint()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let key = |k: &'static str, what: &'static str| {
        Line::from(vec![
            Span::styled(k, Style::default().fg(theme::accent()).bold()),
            Span::styled(what, Style::default().fg(theme::muted())),
        ])
    };
    let keys = vec![
        key("n", "  new session in a directory you pick"),
        key("?", "  keyboard shortcuts"),
        key("q", "  quit"),
    ];
    let [art, hint] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(keys.len() as u16 + 1),
    ])
    .areas(inner);

    let Some(cells) = splash::render(art.width, art.height, splash::angle()) else {
        let mut lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                "No sessions running.",
                Style::default().fg(theme::muted()),
            )),
            Line::from(""),
        ];
        lines.extend(keys);
        f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
        return;
    };
    let buf = f.buffer_mut();
    for (i, cell) in cells.into_iter().enumerate() {
        let Some((ch, light)) = cell else { continue };
        let x = art.x + (i % art.width as usize) as u16;
        let y = art.y + (i / art.width as usize) as u16;
        let style = if light > 0.66 {
            Style::default().fg(theme::accent()).bold()
        } else if light > 0.33 {
            Style::default().fg(theme::accent())
        } else {
            Style::default().fg(theme::accent_dim())
        };
        buf[(x, y)].set_char(ch).set_style(style);
    }
    f.render_widget(Paragraph::new(keys).alignment(Alignment::Center), hint);
}

fn draw_pane(f: &mut Frame, app: &App, area: Rect) {
    let focused = matches!(app.mode, Mode::Focus);

    let Some(session) = app.selected_session() else {
        draw_empty_pane(f, area);
        return;
    };

    let mode_tag = if !session.is_alive() {
        Span::styled(" FINISHED ", Style::default().fg(theme::dead()).bold())
    } else if focused {
        // Naming the way out in the title means it is on screen even when the
        // status bar is showing a transient message.
        Span::styled(
            " FOCUS — F10 leaves ",
            Style::default()
                .bg(theme::accent())
                .fg(theme::surface())
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" VIEW ONLY ", Style::default().fg(theme::muted()))
    };

    let mut title = vec![
        Span::styled(
            format!(" {} ", session.label),
            Style::default().fg(theme::text()).bold(),
        ),
        mode_tag,
    ];
    // A session stopped on a question is the one thing worth saying on the
    // frame: the pane shows the dialog, but the title says what it wants.
    if let Some(entry) = app.entry_for(app.selected)
        && entry.is_waiting()
    {
        let what = if entry.waiting_for.is_empty() {
            " QUESTION ".to_string()
        } else {
            format!(" QUESTION — {} ", entry.waiting_for)
        };
        title.push(Span::styled(
            what,
            Style::default()
                .bg(theme::ask())
                .fg(theme::surface())
                .add_modifier(Modifier::BOLD),
        ));
    }
    if session.prompt_pending() {
        title.push(Span::styled(
            " understand project… ",
            Style::default().fg(theme::ask()),
        ));
    }
    if session.scrollback > 0 {
        title.push(Span::styled(
            format!(" ^{} ", session.scrollback),
            Style::default().fg(theme::busy()),
        ));
    }
    let queued = session.queued_input();
    if queued > 0 {
        title.push(Span::styled(
            format!(" pasting {} ", fmt_bytes(queued)),
            Style::default().fg(theme::busy()).bold(),
        ));
    }

    let border_color = if focused {
        theme::accent()
    } else {
        theme::faint()
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Line::from(title))
        .title_bottom(Line::from(Span::styled(
            format!(" {} ", shorten_path(&session.cwd, 44)),
            Style::default().fg(theme::faint()),
        )))
        .title_bottom(
            Line::from(Span::styled(
                format!(" {}x{} ", session.cols, session.rows),
                Style::default().fg(theme::faint()),
            ))
            .right_aligned(),
        );

    let Ok(parser) = session.parser.read() else {
        return;
    };
    let screen = parser.screen();

    // Only the pane taking keystrokes should show a cursor.
    let cursor = Cursor::default()
        .visibility(focused && session.is_alive() && !screen.hide_cursor())
        .style(Style::default().fg(theme::accent()));

    let term = PseudoTerminal::new(screen).block(block).cursor(cursor);
    f.render_widget(term, area);

    if let Some(sel) = app.selection.filter(|s| s.session == app.selected) {
        let inner = pane_inner_rect(area);
        let marked = Style::default().add_modifier(Modifier::REVERSED);
        for row in 0..inner.height {
            for col in 0..inner.width {
                if sel.contains(row, col) {
                    f.buffer_mut()[(inner.x + col, inner.y + row)].set_style(marked);
                }
            }
        }
    }
}

/// The repository the selected session works in: branch, what is not
/// committed yet, and the history. It follows the selection, so switching
/// sessions switches repositories.
///
/// With the keyboard on it (`g`), it is a list with a cursor: commits open up
/// to show their files, and the pane beside it previews what the cursor is on.
fn draw_git(f: &mut Frame, app: &mut App, area: Rect) {
    // Browsing lights the panel; typing a message lights only the message box.
    let focused = app.mode == Mode::Git;
    let on_git = app.mode.on_git();
    let target = app.git_target();
    let since = app
        .selected_session()
        .map(|s| SystemTime::now() - s.started.elapsed());
    let App {
        git,
        git_view,
        commit_msg,
        commit_cursor,
        commit_model,
        git_job,
        mode,
        ..
    } = app;
    let state = git
        .as_ref()
        .filter(|(cwd, _)| *cwd == target)
        .map(|(_, s)| s);

    let mut title = vec![Span::styled(
        " GIT ",
        Style::default().fg(theme::accent()).bold(),
    )];
    if let Some(git::State::Repo(snap)) = state
        && let Some(name) = snap.root.file_name()
    {
        title.push(Span::styled(
            format!(
                "{} ",
                truncate(&name.to_string_lossy(), usize::from(git_width()) - 10)
            ),
            Style::default().fg(theme::muted()),
        ));
    }
    let hint = if focused {
        " enter opens · c message · esc leaves "
    } else if on_git {
        " enter commits · esc back "
    } else {
        " g browse · G hide "
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused {
            theme::accent()
        } else {
            theme::faint()
        }))
        .title(Line::from(title))
        .title_bottom(Line::from(Span::styled(
            hint,
            Style::default().fg(theme::faint()),
        )));
    let mut inner = block.inner(area);
    f.render_widget(block, area);

    // The commit box and its buttons head the panel, above the changes they
    // commit, whenever there is a repository to commit to and room left for
    // the list below.
    let mut hits = Vec::new();
    if let Some(git::State::Repo(snap)) = state {
        let msg_rows = wrap_message(commit_msg, inner.width.saturating_sub(2) as usize)
            .len()
            .clamp(1, 5) as u16;
        if inner.height >= msg_rows + 3 + 6 {
            let [message, buttons, _, list] = Layout::vertical([
                Constraint::Length(msg_rows + 2),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .areas(inner);
            inner = list;
            draw_commit_box(
                f,
                commit_msg,
                *commit_cursor,
                *mode == Mode::Commit,
                snap.changes_total,
                message,
            );
            hits.push((message, GitHit::Message));
            hits.extend(draw_git_buttons(f, *git_job, snap, commit_model, buttons));
        }
    }

    let width = inner.width as usize;
    let lines = match state {
        None => vec![
            Line::from(""),
            Line::from(Span::styled(
                " reading…",
                Style::default().fg(theme::faint()),
            )),
        ],
        Some(git::State::NoGit) => vec![
            Line::from(""),
            Line::from(Span::styled(
                " git is not on PATH",
                Style::default().fg(theme::muted()),
            )),
        ],
        Some(git::State::NotARepo) => vec![
            Line::from(""),
            Line::from(Span::styled(
                " not a git repository",
                Style::default().fg(theme::muted()),
            )),
            Line::from(Span::styled(
                format!(" {}", shorten_path(&target, width.saturating_sub(2))),
                Style::default().fg(theme::faint()),
            )),
        ],
        Some(git::State::Repo(snap)) => {
            git_lines(git_view, snap, since, width, inner.height as usize, on_git)
        }
    };
    f.render_widget(Paragraph::new(lines), inner);
    app.git_hits.extend(hits);
}

/// The message split into the rows the box shows: its own lines, each broken
/// at `width` characters. Always at least one row, so the cursor has a place.
fn wrap_message(msg: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in msg.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            rows.push(String::new());
            continue;
        }
        for chunk in chars.chunks(width) {
            rows.push(chunk.iter().collect());
        }
    }
    rows
}

/// The box the commit message is typed into. When it holds more rows than fit,
/// it scrolls to keep the cursor in view, the way a text field does.
fn draw_commit_box(
    f: &mut Frame,
    msg: &str,
    cursor: usize,
    typing: bool,
    changes: usize,
    area: Rect,
) {
    let border = if typing {
        theme::accent()
    } else {
        theme::faint()
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            format!(" message · {changes} changed "),
            Style::default().fg(if typing {
                theme::accent()
            } else {
                theme::muted()
            }),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if msg.is_empty() && !typing {
        f.render_widget(
            Paragraph::new(Span::styled(
                "c writes · m generates",
                Style::default().fg(theme::faint()),
            )),
            inner,
        );
        return;
    }
    let rows = wrap_message(msg, inner.width as usize);
    // A cursor after a full row stands at the start of the row below it.
    let (cur_row, cur_col) = msgedit::screen_pos(msg, cursor, inner.width as usize);
    let height = inner.height as usize;
    let skip = if typing {
        (cur_row + 1).saturating_sub(height)
    } else {
        rows.len().saturating_sub(height)
    };
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(skip)
        .map(|(i, r)| {
            // The subject line is what `git log --oneline` will show.
            let style = if i == 0 {
                Style::default().fg(theme::text()).bold()
            } else {
                Style::default().fg(theme::text())
            };
            Line::from(Span::styled(r.clone(), style))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
    if typing {
        let y = inner.y + (cur_row - skip) as u16;
        f.set_cursor_position((inner.x + cur_col as u16, y.min(inner.bottom() - 1)));
    }
}

/// Commit, Push and Generate, side by side. Returns where each one landed,
/// for the mouse.
fn draw_git_buttons(
    f: &mut Frame,
    job: Option<GitJob>,
    snap: &git::Snapshot,
    model: &str,
    area: Rect,
) -> Vec<(Rect, GitHit)> {
    let push = match (snap.upstream.is_some(), snap.ahead, snap.behind) {
        (false, _, _) => "Push ↑new".to_string(),
        (true, 0, 0) => "Push".to_string(),
        (true, a, 0) => format!("Push ↑{a}"),
        (true, 0, b) => format!("Push ↓{b}"),
        (true, a, b) => format!("Push ↑{a}↓{b}"),
    };
    let pull = match snap.behind {
        0 => "Pull".to_string(),
        b => format!("Pull ↓{b}"),
    };
    let buttons = [
        (GitHit::Commit, Some(GitJob::Commit), "Commit".to_string()),
        (GitHit::Push, Some(GitJob::Push), push),
        (GitHit::Pull, Some(GitJob::Pull), pull),
        (
            GitHit::Generate,
            Some(GitJob::Generate),
            "✦ Generate".to_string(),
        ),
        // Which model Generate asks; pressing it moves to the next one.
        (
            GitHit::Model,
            None,
            format!("{} ▾", commitmsg::short_name(model)),
        ),
    ];
    let mut hits = Vec::new();
    let mut spans = Vec::new();
    let mut x = area.x;
    for (hit, kind, label) in buttons {
        // A commit that has its message written first is still the Commit
        // button's doing.
        let job_kind = job.map(|j| match j {
            GitJob::GenerateCommit => GitJob::Commit,
            j => j,
        });
        let running = kind.is_some() && job_kind == kind;
        let text = match job {
            Some(job) if running => format!(" {} ", job.doing()),
            _ => format!(" {label} "),
        };
        let style = if running {
            Style::default()
                .bg(theme::busy())
                .fg(theme::surface())
                .bold()
        } else if job.is_some() {
            Style::default().bg(theme::surface()).fg(theme::faint())
        } else if hit == GitHit::Commit {
            Style::default()
                .bg(theme::accent())
                .fg(theme::surface())
                .bold()
        } else if hit == GitHit::Model {
            Style::default().bg(theme::surface()).fg(theme::muted())
        } else {
            Style::default()
                .bg(theme::surface())
                .fg(theme::text())
                .bold()
        };
        let w = (Span::raw(text.as_str()).width() as u16).min(area.right().saturating_sub(x));
        if w == 0 {
            break;
        }
        hits.push((Rect::new(x, area.y, w, 1), hit));
        spans.push(Span::styled(text, style));
        spans.push(Span::raw(" "));
        x = x.saturating_add(w + 1);
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    hits
}

/// The panel's rows for a repository, fitted to `height`.
///
/// At rest the changes get up to a third of the panel and the history the
/// rest. Focused, everything is listed and the list scrolls with the cursor.
fn git_lines(
    view: &mut GitView,
    snap: &git::Snapshot,
    since: Option<SystemTime>,
    width: usize,
    height: usize,
    focused: bool,
) -> Vec<Line<'static>> {
    let mut lines = vec![branch_line(snap, width), Line::from("")];
    let room = height.saturating_sub(lines.len());

    let limit = (!focused).then(|| (height / 3).max(3));
    let rows = view.rows(snap, limit);
    let cursor = focused.then(|| view.cursor_index(&rows));

    view.list_scroll = match cursor {
        Some(c) => {
            let top = view.list_scroll.min(rows.len().saturating_sub(room));
            if c < top {
                c
            } else if c >= top + room {
                c + 1 - room
            } else {
                top
            }
        }
        None => 0,
    };

    let now = SystemTime::now();
    for (i, row) in rows.iter().enumerate().skip(view.list_scroll).take(room) {
        let line = row_line(row, snap, view, since, now, width);
        if Some(i) == cursor {
            let pad = width.saturating_sub(line.width());
            let mut spans = line.spans;
            spans.push(Span::raw(" ".repeat(pad)));
            lines.push(
                Line::from(spans).style(
                    Style::default()
                        .bg(theme::surface())
                        .add_modifier(Modifier::BOLD),
                ),
            );
        } else {
            lines.push(line);
        }
    }
    lines
}

fn row_line(
    row: &Row,
    snap: &git::Snapshot,
    view: &GitView,
    since: Option<SystemTime>,
    now: SystemTime,
    width: usize,
) -> Line<'static> {
    match row {
        Row::Changes => changes_head(snap, view.changes_open, width),
        Row::Change(path) => snap
            .changes
            .iter()
            .find(|c| &c.path == path)
            .map(|c| change_line(c, width))
            .unwrap_or_default(),
        Row::More(n) => Line::from(Span::styled(
            format!("     +{n} more"),
            Style::default().fg(theme::faint()),
        )),
        Row::Gap => Line::from(""),
        Row::History => fit(
            vec![
                Span::raw(" "),
                Span::styled(
                    open_mark(view.history_open),
                    Style::default().fg(theme::faint()),
                ),
                Span::styled(" HISTORY", Style::default().fg(theme::muted())),
            ],
            String::new(),
            Style::default(),
            vec![Span::styled(
                format!("{} ", snap.log.len()),
                Style::default().fg(theme::faint()),
            )],
            width,
            false,
        ),
        Row::Commit(hash) => snap
            .log
            .iter()
            .enumerate()
            .find(|(_, c)| &c.hash == hash)
            .map(|(i, c)| commit_line(c, i, snap, view.expanded.contains(hash), since, now, width))
            .unwrap_or_default(),
        Row::CommitFile(hash, path) => {
            let stat = view
                .details
                .get(hash)
                .and_then(|d| d.files.iter().find(|f| &f.path == path));
            let mut right = stat.map_or_else(Vec::new, |s| stat_spans(s.added, s.removed));
            right.push(Span::raw(" "));
            fit(
                vec![Span::raw("      ")],
                path.clone(),
                Style::default().fg(theme::text()),
                right,
                width,
                true,
            )
        }
        Row::Loading(_) => Line::from(Span::styled(
            "      loading…",
            Style::default().fg(theme::faint()),
        )),
    }
}

/// A row with a fixed start, a middle cut to whatever is left, and a fixed
/// end pushed to the right edge.
fn fit(
    prefix: Vec<Span<'static>>,
    text: String,
    text_style: Style,
    right: Vec<Span<'static>>,
    width: usize,
    cut_left: bool,
) -> Line<'static> {
    let pw: usize = prefix.iter().map(Span::width).sum();
    let rw: usize = right.iter().map(Span::width).sum();
    let room = width.saturating_sub(pw + rw + 1);
    let text = if cut_left {
        truncate_left(&text, room)
    } else {
        truncate(&text, room)
    };
    let pad = width.saturating_sub(pw + rw + text.chars().count());
    let mut spans = prefix;
    spans.push(Span::styled(text, text_style));
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(right);
    Line::from(spans)
}

fn open_mark(open: bool) -> &'static str {
    if open { "\u{25be}" } else { "\u{25b8}" }
}

/// `+12 −3`, green and red; `bin` for a file git would not count.
fn stat_spans(added: Option<u32>, removed: Option<u32>) -> Vec<Span<'static>> {
    match (added, removed) {
        (Some(a), Some(r)) => {
            let mut spans = Vec::new();
            if a > 0 || r == 0 {
                spans.push(Span::styled(
                    format!("+{a}"),
                    Style::default().fg(theme::idle()),
                ));
            }
            if r > 0 {
                if !spans.is_empty() {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(
                    format!("\u{2212}{r}"),
                    Style::default().fg(theme::dead()),
                ));
            }
            spans
        }
        _ => vec![Span::styled("bin", Style::default().fg(theme::faint()))],
    }
}

/// Five cells split between added and removed, the way GitHub draws it: the
/// share of each, not the size.
fn share_bar(added: u32, removed: u32) -> Vec<Span<'static>> {
    const CELLS: u32 = 5;
    let total = added + removed;
    if total == 0 {
        return vec![Span::styled(
            "\u{25a0}".repeat(CELLS as usize),
            Style::default().fg(theme::faint()),
        )];
    }
    let green = ((added * CELLS + total / 2) / total).min(CELLS);
    vec![
        Span::styled(
            "\u{25a0}".repeat(green as usize),
            Style::default().fg(theme::idle()),
        ),
        Span::styled(
            "\u{25a0}".repeat((CELLS - green) as usize),
            Style::default().fg(theme::dead()),
        ),
    ]
}

/// The heading over the changes: how many files, and how many lines in and
/// out, with the share bar.
fn changes_head(snap: &git::Snapshot, open: bool, width: usize) -> Line<'static> {
    let prefix = vec![
        Span::raw(" "),
        Span::styled(open_mark(open), Style::default().fg(theme::faint())),
        Span::styled(" CHANGES ", Style::default().fg(theme::muted())),
    ];
    if snap.changes_total == 0 {
        return fit(
            prefix,
            String::new(),
            Style::default(),
            vec![Span::styled("clean ", Style::default().fg(theme::idle()))],
            width,
            false,
        );
    }
    let mut right = stat_spans(Some(snap.added), Some(snap.removed));
    right.push(Span::raw(" "));
    right.extend(share_bar(snap.added, snap.removed));
    right.push(Span::raw(" "));
    fit(
        prefix,
        snap.changes_total.to_string(),
        Style::default().fg(theme::text()).bold(),
        right,
        width,
        false,
    )
}

/// The branch, and how it stands against its upstream.
fn branch_line(snap: &git::Snapshot, width: usize) -> Line<'static> {
    let branch = match (&snap.branch, snap.log.first()) {
        (Some(b), _) => b.clone(),
        (None, Some(c)) => format!("detached at {}", c.hash),
        (None, None) => "detached".to_string(),
    };
    let mut sync = String::new();
    if snap.ahead > 0 {
        sync.push_str(&format!("\u{2191}{} ", snap.ahead));
    }
    if snap.behind > 0 {
        sync.push_str(&format!("\u{2193}{} ", snap.behind));
    }
    if snap.upstream.is_none() {
        sync.push_str("no upstream ");
    } else if sync.is_empty() {
        sync.push_str("in sync ");
    }
    let sync_color = if snap.behind > 0 {
        theme::dead()
    } else if snap.ahead > 0 {
        theme::ask()
    } else {
        theme::faint()
    };
    fit(
        vec![Span::styled(
            " \u{2387} ",
            Style::default().fg(theme::accent()),
        )],
        branch,
        Style::default().fg(theme::text()).bold(),
        vec![Span::styled(sync, Style::default().fg(sync_color))],
        width,
        false,
    )
}

/// One changed file: its porcelain code coloured by what happened to it, the
/// path cut from the left since the file name is the end worth keeping, and
/// its lines in and out.
fn change_line(c: &git::Change, width: usize) -> Line<'static> {
    let color = match c.code.as_str() {
        "??" => theme::faint(),
        code if code.contains('U') => theme::ask(),
        code if code.contains('D') => theme::dead(),
        _ if c.staged() => theme::idle(),
        _ => theme::busy(),
    };
    let mut right = if c.added.is_none() && c.untracked() {
        vec![Span::styled("new", Style::default().fg(theme::faint()))]
    } else {
        stat_spans(c.added, c.removed)
    };
    right.push(Span::raw(" "));
    fit(
        vec![
            Span::raw("   "),
            Span::styled(c.code.clone(), Style::default().fg(color).bold()),
            Span::raw(" "),
        ],
        c.path.clone(),
        Style::default().fg(theme::text()),
        right,
        width,
        true,
    )
}

/// One commit: open or closed, a mark for the ones made during the session,
/// the hash (in the warning colour while it is not pushed yet), the subject,
/// who made it and how long ago.
fn commit_line(
    c: &git::Commit,
    index: usize,
    snap: &git::Snapshot,
    open: bool,
    since: Option<SystemTime>,
    now: SystemTime,
    width: usize,
) -> Line<'static> {
    let at = UNIX_EPOCH + Duration::from_secs(c.time);
    let fresh = since.is_some_and(|t| at >= t);
    // The log starts at HEAD, so the first `ahead` of it are what the
    // upstream has not got.
    let unpushed = snap.upstream.is_some() && index < snap.ahead as usize;
    let hash_color = if unpushed {
        theme::ask()
    } else {
        theme::accent_dim()
    };
    let author = c.author.split_whitespace().next().unwrap_or("");
    fit(
        vec![
            Span::raw(" "),
            Span::styled(open_mark(open), Style::default().fg(theme::faint())),
            Span::styled(
                if fresh { "\u{2022}" } else { " " },
                Style::default().fg(theme::accent()),
            ),
            Span::styled(c.hash.clone(), Style::default().fg(hash_color)),
            Span::raw(" "),
        ],
        c.subject.clone(),
        Style::default().fg(theme::text()),
        vec![
            Span::styled(truncate(author, 9), Style::default().fg(theme::muted())),
            Span::raw(" "),
            Span::styled(
                format!("{:>3}", fmt_age(now, at)),
                Style::default().fg(theme::faint()),
            ),
            Span::raw(" "),
        ],
        width,
        false,
    )
}

/// The pane while the keyboard is on the git panel: the diff of the file under
/// the cursor, the whole of the commit under it, or a summary of the changes.
fn draw_git_preview(f: &mut Frame, app: &mut App, area: Rect) {
    let target = app.git_target();
    let App { git, git_view, .. } = app;
    let snap = match git.as_ref().filter(|(cwd, _)| *cwd == target) {
        Some((_, git::State::Repo(snap))) => Some(snap),
        _ => None,
    };

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::faint()));
    let inner = block.inner(area);
    let width = inner.width as usize;

    let (title, lines) = match snap {
        None => (
            " git ".to_string(),
            vec![Line::from(Span::styled(
                " nothing to show",
                Style::default().fg(theme::faint()),
            ))],
        ),
        Some(snap) => preview_content(git_view, snap, width),
    };

    let height = inner.height as usize;
    let top = git_view
        .preview_scroll
        .min(lines.len().saturating_sub(height));
    git_view.preview_scroll = top;
    let position = if lines.len() > height {
        format!(
            " {}-{} of {} · pgup/pgdn ",
            top + 1,
            (top + height).min(lines.len()),
            lines.len()
        )
    } else {
        String::new()
    };
    let block = block
        .title(Line::from(Span::styled(
            title,
            Style::default().fg(theme::text()).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(position, Style::default().fg(theme::faint()))).right_aligned(),
        );
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    let visible: Vec<Line> = lines.into_iter().skip(top).take(height).collect();
    f.render_widget(Paragraph::new(visible), inner);
}

/// The editor on the pane: the file tree on the left, the open file on the
/// right with its tabs on the frame, and a prompt on its last row when one is
/// open.
fn draw_ide(f: &mut Frame, app: &mut App, area: Rect) {
    // Files git counts as changed, by a normalised path, so the tree can
    // mark them. Folders holding one get a dot.
    let target = app.git_target();
    let mut marks: HashMap<String, (char, Color)> = HashMap::new();
    let mut dirty_dirs: HashSet<String> = HashSet::new();
    if let Some((cwd, git::State::Repo(snap))) = &app.git
        && *cwd == target
    {
        for c in &snap.changes {
            let path = snap.root.join(&c.path);
            marks.insert(norm_path(&path), change_mark(&c.code));
            let mut p = path.parent();
            while let Some(dir) = p {
                if !dir.starts_with(&snap.root) || !dirty_dirs.insert(norm_path(dir)) {
                    break;
                }
                p = dir.parent();
            }
        }
    }

    let ide = &mut app.ide;
    f.render_widget(Clear, area);
    let tree_w = (area.width / 4).clamp(area.width.min(22), 40);
    let [tree_area, edit_area] =
        Layout::horizontal([Constraint::Length(tree_w), Constraint::Min(1)]).areas(area);
    ide.areas.all = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(2),
        ..area
    };
    ide.areas.tabs.clear();

    // ---- the tree
    let tree_keys = !ide.editing || ide.buffers.is_empty();
    let root_name = ide.tree.root.file_name().map_or_else(
        || ide.tree.root.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if tree_keys {
            theme::accent()
        } else {
            theme::faint()
        }))
        .title(Line::from(vec![
            Span::styled(" FILES ", Style::default().fg(theme::accent()).bold()),
            Span::styled(
                format!(
                    "{} ",
                    truncate(&root_name, tree_w.saturating_sub(10) as usize)
                ),
                Style::default().fg(theme::muted()),
            ),
        ]));
    let inner = block.inner(tree_area);
    f.render_widget(block, tree_area);
    ide.areas.tree = inner;

    let rows = ide.tree.rows();
    let cursor = ide.tree.cursor_index(&rows);
    let height = inner.height as usize;
    if ide.tree.follow {
        if cursor < ide.tree.scroll {
            ide.tree.scroll = cursor;
        } else if cursor >= ide.tree.scroll + height {
            ide.tree.scroll = cursor + 1 - height;
        }
    }
    ide.tree.scroll = ide.tree.scroll.min(rows.len().saturating_sub(height));
    let width = inner.width as usize;
    let open: HashMap<String, bool> = ide
        .buffers
        .iter()
        .map(|b| (norm_path(&b.path), b.dirty()))
        .collect();
    let lines: Vec<Line> = if rows.is_empty() {
        vec![Line::from(Span::styled(
            " empty — a makes a file",
            Style::default().fg(theme::faint()),
        ))]
    } else {
        rows.iter()
            .enumerate()
            .skip(ide.tree.scroll)
            .take(height)
            .map(|(i, r)| {
                let key = norm_path(&r.path);
                let mut name_style = if r.dir {
                    Style::default().fg(theme::text()).bold()
                } else {
                    Style::default().fg(theme::text())
                };
                let mut right = String::new();
                let mut right_style = Style::default().fg(theme::faint());
                if let Some((m, color)) = marks.get(&key) {
                    name_style = name_style.fg(*color);
                    right = m.to_string();
                    right_style = Style::default().fg(*color).bold();
                } else if r.dir && dirty_dirs.contains(&key) {
                    right = "•".to_string();
                    right_style = Style::default().fg(theme::busy());
                }
                if let Some(unsaved) = open.get(&key) {
                    name_style = name_style.add_modifier(Modifier::UNDERLINED);
                    if *unsaved {
                        right = "●".to_string();
                        right_style = Style::default().fg(theme::accent()).bold();
                    }
                }
                let lead = format!(
                    " {}{}",
                    "  ".repeat(r.depth),
                    if r.dir { open_mark(r.open) } else { " " }
                );
                let line = fit(
                    vec![Span::styled(
                        format!("{lead} "),
                        Style::default().fg(theme::faint()),
                    )],
                    r.name.clone(),
                    name_style,
                    vec![Span::styled(format!("{right} "), right_style)],
                    width,
                    false,
                );
                if i == cursor {
                    let bg = if tree_keys {
                        theme::surface()
                    } else {
                        Color::Reset
                    };
                    line.style(Style::default().bg(bg).add_modifier(Modifier::BOLD))
                } else {
                    line
                }
            })
            .collect()
    };
    f.render_widget(Paragraph::new(lines), inner);

    // ---- the editor
    let text_keys = !tree_keys && ide.prompt.is_none();
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if text_keys {
            theme::accent()
        } else {
            theme::faint()
        }));

    // Tabs on the top border, scrolled so the active one is always there.
    let room = edit_area.width.saturating_sub(4) as usize;
    let labels: Vec<String> = ide
        .buffers
        .iter()
        .map(|b| format!(" {}{} ", b.name(), if b.dirty() { " ●" } else { "" }))
        .collect();
    let widths: Vec<usize> = labels
        .iter()
        .map(|l| Span::raw(l.as_str()).width() + 1)
        .collect();
    let mut first = 0;
    while first < ide.active && widths[first..=ide.active].iter().sum::<usize>() > room {
        first += 1;
    }
    let mut spans = vec![Span::raw(" ")];
    let mut x = edit_area.x + 2;
    let mut used = 0;
    for (i, label) in labels.iter().enumerate().skip(first) {
        if used + widths[i] > room {
            break;
        }
        let style = if i == ide.active && text_keys {
            Style::default()
                .bg(theme::accent())
                .fg(theme::surface())
                .bold()
        } else if i == ide.active {
            Style::default().fg(theme::accent()).bold()
        } else {
            Style::default().fg(theme::muted())
        };
        let w = widths[i] as u16 - 1;
        ide.areas.tabs.push((Rect::new(x, edit_area.y, w, 1), i));
        spans.push(Span::styled(label.clone(), style));
        spans.push(Span::raw(" "));
        x += w + 1;
        used += widths[i];
    }
    if ide.buffers.is_empty() {
        spans.push(Span::styled(
            " EDITOR ",
            Style::default().fg(theme::accent()).bold(),
        ));
    }
    block = block.title(Line::from(spans));

    if let Some(b) = ide.buffers.get(ide.active) {
        let shown = b
            .path
            .strip_prefix(&ide.tree.root)
            .unwrap_or(&b.path)
            .display()
            .to_string();
        let mut left = vec![Span::styled(
            format!(
                " {} ",
                truncate_left(&shown, (edit_area.width / 2) as usize)
            ),
            Style::default().fg(theme::muted()),
        )];
        if b.gone {
            left.push(Span::styled(
                " deleted on disk ",
                Style::default().fg(theme::dead()).bold(),
            ));
        } else if b.changed_on_disk {
            left.push(Span::styled(
                " changed on disk ",
                Style::default().fg(theme::ask()).bold(),
            ));
        }
        let indent = if b.indent == "\t" {
            "tabs".to_string()
        } else {
            format!("spaces {}", b.indent.len())
        };
        let right = format!(
            " ln {}/{}, col {} · {} · {} · {} ",
            b.cursor.line + 1,
            b.lines.len(),
            b.cursor.col + 1,
            b.lang.name,
            if b.crlf { "CRLF" } else { "LF" },
            indent,
        );
        block = block.title_bottom(Line::from(left)).title_bottom(
            Line::from(Span::styled(right, Style::default().fg(theme::faint()))).right_aligned(),
        );
    }

    let inner = block.inner(edit_area);
    f.render_widget(block, edit_area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let prompt_rows = u16::from(ide.prompt.is_some());
    let text_h = inner.height.saturating_sub(prompt_rows);
    let text_rect = Rect {
        height: text_h,
        ..inner
    };

    if ide.buffers.is_empty() {
        ide.areas.text = Rect::default();
        ide.areas.gutter = Rect::default();
        let hint = |k: &'static str, v: &'static str| {
            Line::from(vec![
                Span::styled(k, Style::default().fg(theme::accent()).bold()),
                Span::styled(v, Style::default().fg(theme::muted())),
            ])
        };
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                "No file open.",
                Style::default().fg(theme::muted()),
            )),
            Line::from(""),
            hint("enter", "  opens the file under the cursor in the tree"),
            hint("ctrl+p", "  finds a file by name"),
            hint("a", "  makes a new file (end with / for a folder)"),
            hint("esc", "  gives the pane back to the session"),
        ];
        f.render_widget(
            Paragraph::new(lines).alignment(Alignment::Center),
            text_rect,
        );
    } else {
        let query = ide.query.clone();
        let b = &mut ide.buffers[ide.active];
        let digits = b.lines.len().to_string().len();
        let gutter_w = (digits + 2) as u16;
        let gutter = Rect {
            width: gutter_w.min(text_rect.width),
            ..text_rect
        };
        let text = Rect {
            x: text_rect.x + gutter.width,
            width: text_rect.width.saturating_sub(gutter.width),
            ..text_rect
        };
        ide.areas.gutter = gutter;
        ide.areas.text = text;
        if b.follow {
            b.scroll_to_cursor(text.height as usize, text.width as usize);
        }
        b.scroll = b.scroll.min(b.lines.len().saturating_sub(1));

        let sel = b.selection();
        let qlen = query.chars().count();
        let mut in_block = syntax::block_open_before(&b.lines, b.scroll, b.lang);
        let mut gutter_lines = Vec::new();
        let mut text_lines = Vec::new();
        for row in 0..text.height as usize {
            let li = b.scroll + row;
            let Some(line) = b.lines.get(li) else {
                gutter_lines.push(Line::from(""));
                text_lines.push(Line::from(""));
                continue;
            };
            let here = li == b.cursor.line;
            gutter_lines.push(Line::from(Span::styled(
                format!(" {:>digits$} ", li + 1),
                if here {
                    Style::default().fg(theme::text()).bold()
                } else {
                    Style::default().fg(theme::faint())
                },
            )));
            let (toks, next) = syntax::highlight(line, b.lang, in_block);
            in_block = next;
            let hits = b.matches_in_line(li, &query);
            text_lines.push(code_line(
                line,
                &toks,
                li,
                sel,
                &hits,
                qlen,
                b.hscroll,
                text.width as usize,
            ));
        }
        f.render_widget(Paragraph::new(gutter_lines), gutter);
        f.render_widget(Paragraph::new(text_lines), text);

        if text_keys && b.cursor.line >= b.scroll && b.cursor.line < b.scroll + text.height as usize
        {
            let dc = editor::display_col(&b.lines[b.cursor.line], b.cursor.col);
            if dc >= b.hscroll && dc < b.hscroll + text.width as usize {
                f.set_cursor_position((
                    text.x + (dc - b.hscroll) as u16,
                    text.y + (b.cursor.line - b.scroll) as u16,
                ));
            }
        }
    }

    // ---- the prompt
    if let Some(p) = &ide.prompt {
        let row = Rect {
            y: inner.bottom() - 1,
            height: 1,
            ..inner
        };
        let label = format!(" {} ", p.label());
        let mut spans = vec![Span::styled(
            label.clone(),
            Style::default()
                .bg(theme::surface())
                .fg(theme::accent())
                .bold(),
        )];
        if !p.is_question() {
            spans.push(Span::styled(
                format!(" {}", p.input),
                Style::default().fg(theme::text()),
            ));
        }
        f.render_widget(Clear, row);
        f.render_widget(Paragraph::new(Line::from(spans)), row);
        if !p.is_question() {
            let x =
                row.x + (Span::raw(label.as_str()).width() + 1 + p.input.chars().count()) as u16;
            f.set_cursor_position((x.min(row.right().saturating_sub(1)), row.y));
        }

        if p.kind == PromptKind::Open {
            let matches = p.matches();
            let n = matches.len().max(1) as u16;
            let list = Rect {
                y: row.y.saturating_sub(n).max(inner.y),
                height: n.min(row.y.saturating_sub(inner.y)),
                ..inner
            };
            f.render_widget(Clear, list);
            let lines: Vec<Line> = if matches.is_empty() {
                vec![Line::from(Span::styled(
                    if p.files.is_empty() {
                        " no files here"
                    } else {
                        " nothing matches"
                    },
                    Style::default().fg(theme::faint()),
                ))]
            } else {
                matches
                    .iter()
                    .enumerate()
                    .map(|(i, m)| {
                        let line = Line::from(Span::styled(
                            format!(
                                " {}",
                                truncate_left(m, (inner.width as usize).saturating_sub(2))
                            ),
                            Style::default().fg(theme::text()),
                        ));
                        if i == p.pick {
                            line.style(
                                Style::default()
                                    .bg(theme::surface())
                                    .fg(theme::accent())
                                    .bold(),
                            )
                        } else {
                            line
                        }
                    })
                    .collect()
            };
            f.render_widget(Paragraph::new(lines), list);
        }
    }
}

/// A path as a map key: one separator, one case, so git's `C:/x/y` and the
/// file system's `C:\x\y` meet.
fn norm_path(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/").to_lowercase()
}

/// The letter the tree shows for a file git counts as changed, and its colour.
fn change_mark(code: &str) -> (char, Color) {
    let mut chars = code.chars();
    let x = chars.next().unwrap_or(' ');
    let y = chars.next().unwrap_or(' ');
    match (x, y) {
        ('?', _) => ('U', theme::idle()),
        ('A', _) | (_, 'A') => ('A', theme::idle()),
        ('D', _) | (_, 'D') => ('D', theme::dead()),
        ('R', _) => ('R', theme::busy()),
        ('U', _) | (_, 'U') => ('!', theme::dead()),
        _ => ('M', theme::busy()),
    }
}

fn tok_style(t: Tok) -> Style {
    match t {
        Tok::Plain => Style::default().fg(theme::text()),
        Tok::Keyword => Style::default().fg(theme::accent()).bold(),
        Tok::Str => Style::default().fg(theme::idle()),
        Tok::Comment => Style::default()
            .fg(theme::faint())
            .add_modifier(Modifier::ITALIC),
        Tok::Number => Style::default().fg(theme::busy()),
        Tok::Type => Style::default().fg(theme::ask()),
        Tok::Call => Style::default().fg(theme::text()).bold(),
        Tok::Heading => Style::default().fg(theme::accent()).bold(),
    }
}

/// One line of code as the editor shows it: coloured, with the selection and
/// search matches marked, tabs expanded, cut to the columns in view.
#[allow(clippy::too_many_arguments)]
fn code_line(
    line: &str,
    toks: &[Tok],
    li: usize,
    sel: Option<(editor::Pos, editor::Pos)>,
    hits: &[usize],
    qlen: usize,
    hscroll: usize,
    width: usize,
) -> Line<'static> {
    let selected = |col: usize| {
        sel.is_some_and(|(s, e)| {
            let p = editor::Pos::new(li, col);
            p >= s && p < e
        })
    };
    let marked = Style::default().bg(theme::accent_dim()).fg(theme::text());
    let found = Style::default().bg(theme::surface());
    let end = hscroll + width;
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_style = Style::default();
    let mut push = |text: &str, style: Style, spans: &mut Vec<Span<'static>>, run: &mut String| {
        if style != run_style && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(run), run_style));
        }
        run_style = style;
        run.push_str(text);
    };
    let mut w = 0;
    for (ci, c) in line.chars().enumerate() {
        if w >= end {
            break;
        }
        let cw = editor::char_width(c, w);
        let mut style = tok_style(toks.get(ci).copied().unwrap_or(Tok::Plain));
        if hits.iter().any(|&h| ci >= h && ci < h + qlen) {
            style = style.patch(found);
        }
        if selected(ci) {
            style = style.patch(marked);
        }
        let start = w;
        w += cw;
        if w <= hscroll {
            continue;
        }
        // A wide character cut by an edge shows as blanks, not half a glyph.
        let cut = start < hscroll || w > end;
        let visible = w.min(end) - start.max(hscroll);
        let text = if c == '\t' || cut {
            " ".repeat(visible)
        } else if c.is_control() {
            "·".to_string()
        } else {
            c.to_string()
        };
        push(&text, style, &mut spans, &mut run);
    }
    // The line break itself is selected: one marked cell past the end.
    let len = line.chars().count();
    if sel.is_some_and(|(s, e)| {
        let p = editor::Pos::new(li, len);
        p >= s && p < e
    }) && w >= hscroll
        && w < end
    {
        push(" ", marked, &mut spans, &mut run);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, run_style));
    }
    Line::from(spans)
}

/// Title and lines for the preview, by the row under the cursor.
fn preview_content(
    view: &GitView,
    snap: &git::Snapshot,
    width: usize,
) -> (String, Vec<Line<'static>>) {
    let loading = || {
        vec![Line::from(Span::styled(
            " loading…",
            Style::default().fg(theme::faint()),
        ))]
    };
    match view.cursor.as_ref() {
        Some(Row::Change(path)) | Some(Row::CommitFile(_, path)) => {
            let lines = view
                .wanted_diff(snap)
                .and_then(|src| view.diff_for(&src).map(|l| diff_lines(l, width)))
                .unwrap_or_else(loading);
            let title = match view.cursor.as_ref() {
                Some(Row::CommitFile(hash, _)) => format!(" {path} @ {hash} "),
                _ => format!(" {path} "),
            };
            (title, lines)
        }
        Some(Row::Commit(hash)) => {
            let lines = view
                .details
                .get(hash)
                .map(|d| commit_preview(hash, d, width))
                .unwrap_or_else(loading);
            (format!(" commit {hash} "), lines)
        }
        Some(Row::History) => {
            let mut lines = vec![Line::from("")];
            let mut authors: Vec<(&str, usize)> = Vec::new();
            for c in &snap.log {
                match authors.iter_mut().find(|(a, _)| *a == c.author) {
                    Some((_, n)) => *n += 1,
                    None => authors.push((&c.author, 1)),
                }
            }
            lines.push(Line::from(Span::styled(
                format!(" the last {} commits, by author:", snap.log.len()),
                Style::default().fg(theme::muted()),
            )));
            lines.push(Line::from(""));
            for (a, n) in authors {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {n:>4}  "),
                        Style::default().fg(theme::accent()).bold(),
                    ),
                    Span::styled(a.to_string(), Style::default().fg(theme::text())),
                ]));
            }
            (" history ".to_string(), lines)
        }
        _ => {
            let mut lines = vec![Line::from("")];
            if snap.changes_total == 0 {
                lines.push(Line::from(Span::styled(
                    " nothing uncommitted — the work tree matches HEAD",
                    Style::default().fg(theme::muted()),
                )));
            } else {
                lines.push(summary_line(snap.changes_total, snap.added, snap.removed));
                lines.push(Line::from(""));
                lines.extend(stat_graph(
                    snap.changes
                        .iter()
                        .map(|c| (c.path.as_str(), c.added, c.removed)),
                    width,
                ));
            }
            (" uncommitted changes ".to_string(), lines)
        }
    }
}

/// `3 files changed, +120 −33` and the share bar.
fn summary_line(files: usize, added: u32, removed: u32) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!(
            " {files} file{} changed  ",
            if files == 1 { "" } else { "s" }
        ),
        Style::default().fg(theme::text()).bold(),
    )];
    spans.extend(stat_spans(Some(added), Some(removed)));
    spans.push(Span::raw("  "));
    spans.extend(share_bar(added, removed));
    Line::from(spans)
}

/// Every file with a bar of `+` and `-` scaled to the busiest one, the way
/// `git diff --stat` draws it.
fn stat_graph<'a>(
    files: impl Iterator<Item = (&'a str, Option<u32>, Option<u32>)>,
    width: usize,
) -> Vec<Line<'static>> {
    let files: Vec<_> = files.collect();
    let most = files
        .iter()
        .map(|(_, a, r)| a.unwrap_or(0) + r.unwrap_or(0))
        .max()
        .unwrap_or(0)
        .max(1);
    let bar = (width / 3).clamp(5, 40) as u32;
    let path_room = width.saturating_sub(bar as usize + 12).max(8);
    files
        .into_iter()
        .map(|(path, a, r)| {
            let mut spans = vec![Span::styled(
                format!(" {:<path_room$} ", truncate_left(path, path_room)),
                Style::default().fg(theme::text()),
            )];
            match (a, r) {
                (Some(a), Some(r)) => {
                    let total = a + r;
                    // At least one cell for any change, so a one-line edit
                    // next to a rewrite is still visible.
                    let cells = if total == 0 {
                        0
                    } else {
                        (total * bar / most).max(1)
                    };
                    let plus = (a * cells + total / 2).checked_div(total).unwrap_or(0);
                    spans.push(Span::styled(
                        format!("{total:>5} "),
                        Style::default().fg(theme::muted()),
                    ));
                    spans.push(Span::styled(
                        "+".repeat(plus as usize),
                        Style::default().fg(theme::idle()),
                    ));
                    spans.push(Span::styled(
                        "-".repeat((cells - plus) as usize),
                        Style::default().fg(theme::dead()),
                    ));
                }
                _ => spans.push(Span::styled("  bin", Style::default().fg(theme::faint()))),
            }
            Line::from(spans)
        })
        .collect()
}

/// A commit in full: who, when, the whole message, and what it touched.
fn commit_preview(hash: &str, d: &git::CommitDetail, width: usize) -> Vec<Line<'static>> {
    let label = |k: &str| Span::styled(format!(" {k:<8}"), Style::default().fg(theme::muted()));
    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            label("commit"),
            Span::styled(
                hash.to_string(),
                Style::default().fg(theme::accent()).bold(),
            ),
        ]),
        Line::from(vec![
            label("author"),
            Span::styled(d.author.clone(), Style::default().fg(theme::text()).bold()),
            Span::styled(
                format!(" <{}>", d.email),
                Style::default().fg(theme::faint()),
            ),
        ]),
        Line::from(vec![
            label("date"),
            Span::styled(d.date.clone(), Style::default().fg(theme::text())),
        ]),
        Line::from(""),
    ];
    for l in d.message.lines() {
        lines.push(Line::from(Span::styled(
            format!(
                "   {}",
                truncate(&l.replace('\t', "    "), width.saturating_sub(4))
            ),
            Style::default().fg(theme::text()),
        )));
    }
    lines.push(Line::from(""));
    let added = d.files.iter().filter_map(|f| f.added).sum();
    let removed = d.files.iter().filter_map(|f| f.removed).sum();
    lines.push(summary_line(d.files.len(), added, removed));
    lines.push(Line::from(""));
    lines.extend(stat_graph(
        d.files
            .iter()
            .map(|f| (f.path.as_str(), f.added, f.removed)),
        width,
    ));
    lines
}

/// A unified diff, coloured the way a terminal `git diff` is.
fn diff_lines(lines: &[String], width: usize) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|l| {
            let style = if l.starts_with("+++") || l.starts_with("---") {
                Style::default().fg(theme::muted()).bold()
            } else if l.starts_with('+') {
                Style::default().fg(theme::idle())
            } else if l.starts_with('-') {
                Style::default().fg(theme::dead())
            } else if l.starts_with("@@") {
                Style::default().fg(theme::accent())
            } else if l.starts_with("diff ")
                || l.starts_with("index ")
                || l.starts_with("new file")
                || l.starts_with("deleted file")
                || l.starts_with("similarity")
                || l.starts_with("rename ")
                || l.starts_with('(')
            {
                Style::default().fg(theme::faint())
            } else {
                Style::default().fg(theme::text())
            };
            Line::from(Span::styled(
                truncate(&format!(" {}", l.replace('\t', "    ")), width),
                style,
            ))
        })
        .collect()
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    if let Some((msg, _)) = &app.status {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(" > ", Style::default().fg(theme::accent())),
            Span::styled(msg.clone(), Style::default().fg(theme::text())),
        ]));
        f.render_widget(p, area);
        return;
    }

    let hints: Vec<(&str, &str)> = match app.mode {
        Mode::Nav if app.update_ready => vec![
            ("r", "RESTART INTO THE NEW BUILD"),
            ("up/dn", "select"),
            ("enter", "focus"),
            ("n", "new"),
            ("u", "understand project"),
            ("q", "quit"),
        ],
        Mode::Nav if app.release.is_some() && !app.update_busy() => vec![
            ("i", "INSTALL THE NEW RELEASE"),
            ("up/dn", "select"),
            ("enter", "focus"),
            ("n", "new"),
            ("u", "understand project"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Mode::Nav => vec![
            ("up/dn", "select"),
            ("enter", "focus"),
            ("n", "new"),
            ("e", "editor"),
            ("s", "shell"),
            ("R", "resume a conversation"),
            ("u", "understand project"),
            ("g", "git"),
            ("b", "branch"),
            ("t", "group"),
            ("B", "big brother"),
            ("A", "reports"),
            ("x", "kill"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Mode::Focus => vec![
            ("F10", "LEAVE FOCUS"),
            ("alt+g", "git"),
            ("alt+e", "editor"),
            ("F1-F9", "session"),
            ("F11", "new"),
            ("F12", "help"),
        ],
        Mode::NewSession => vec![
            ("enter", "start"),
            ("up/dn", "pick"),
            ("right", "enter dir"),
            ("left", "parent"),
            ("ctrl+r", "local/remote/teleport"),
            ("esc", "cancel"),
        ],
        Mode::Help => vec![("any key", "close")],
        Mode::ConfirmKill => vec![("y", "yes"), ("n/esc", "no")],
        Mode::ConfirmMkdir => vec![("y/enter", "create it"), ("n/esc", "back to the path")],
        Mode::ConfirmRestart => vec![("y", "restart"), ("n/esc", "leave it")],
        Mode::Resume => vec![
            ("enter", "resume"),
            ("up/dn", "pick a conversation"),
            ("esc", "close"),
        ],
        Mode::Git => vec![
            ("esc", "back"),
            ("alt+g", "back to the session"),
            ("up/dn", "move"),
            ("enter", "show/fold"),
            ("left", "close"),
            ("pgup/pgdn", "scroll the preview"),
            ("c", "message"),
            ("m", "generate it"),
            ("M", "next model"),
            ("p", "push"),
            ("P", "pull"),
            ("f", "fetch"),
            ("b", "switch branch"),
            ("G", "hide the panel"),
        ],
        Mode::Commit => vec![
            ("enter", "commit"),
            ("shift+enter", "new line"),
            ("ctrl+g", "generate"),
            ("ctrl+o", "next model"),
            ("ctrl+p", "push"),
            ("ctrl+u", "clear"),
            ("esc", "back to the list"),
        ],
        Mode::Branch => vec![
            ("type", "filter"),
            ("up/dn", "pick"),
            ("enter", "switch"),
            ("esc", "close"),
        ],
        Mode::Tag => vec![
            ("a-z", "put the session in that group"),
            ("-", "no group"),
            ("esc", "cancel"),
        ],
        Mode::BigBrother => vec![
            ("a-z", "watch that group"),
            ("*", "watch all"),
            ("enter", "the selected session's group"),
            ("esc", "cancel"),
        ],
        Mode::Reports => vec![("up/dn", "scroll"), ("any key", "close")],
        Mode::PushFailed => vec![
            ("f/enter", "Claude fixes it"),
            ("r", "push again"),
            ("up/dn", "scroll"),
            ("esc", "close"),
        ],
        Mode::Ide if app.ide.prompt.as_ref().is_some_and(|p| p.is_question()) => {
            vec![("the letter", "answers"), ("esc", "cancel")]
        }
        Mode::Ide if app.ide.prompt.is_some() => vec![
            ("enter", "go"),
            ("up/dn", "pick"),
            ("ctrl+u", "clear"),
            ("esc", "cancel"),
        ],
        Mode::Ide if app.ide.editing && !app.ide.buffers.is_empty() => vec![
            ("ctrl+s", "save"),
            ("ctrl+z/y", "undo/redo"),
            ("ctrl+f", "find"),
            ("ctrl+h", "replace"),
            ("ctrl+g", "line"),
            ("ctrl+p", "open"),
            ("ctrl+/", "comment"),
            ("ctrl+d", "duplicate"),
            ("alt+up/dn", "move line"),
            ("ctrl+w", "close"),
            ("alt+left/right", "tabs"),
            ("esc", "tree"),
            ("alt+e", "session"),
        ],
        Mode::Ide => vec![
            ("enter", "open"),
            ("up/dn", "move"),
            ("left/right", "fold"),
            ("a", "new"),
            ("r", "rename"),
            ("d", "delete"),
            ("ctrl+p", "find a file"),
            ("y", "copy path"),
            ("tab", "to the file"),
            ("esc", "leave"),
        ],
        Mode::Understand => vec![
            ("F1-F9", "that session"),
            ("u", "new session"),
            ("n", "new, pick a directory"),
            ("enter", "the selected one"),
            ("esc", "cancel"),
        ],
    };

    let mut spans = vec![Span::raw(" ")];
    for (i, (k, v)) in hints.iter().enumerate() {
        // In Focus the escape hatch is the one thing that must not be missed,
        // so it gets the inverted chip rather than the usual accent text.
        let key_style = if app.mode == Mode::Focus && i == 0 {
            Style::default()
                .bg(theme::accent())
                .fg(theme::surface())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD)
        };
        spans.push(Span::styled(
            if app.mode == Mode::Focus && i == 0 {
                format!(" {k} ")
            } else {
                (*k).to_string()
            },
            key_style,
        ));
        if !v.is_empty() {
            spans.push(Span::styled(
                format!(" {v}"),
                Style::default().fg(theme::muted()),
            ));
        }
        spans.push(Span::raw("   "));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_new_session(f: &mut Frame, app: &App) {
    let Some(form) = &app.form else { return };
    if form.kind == SpawnKind::Remote {
        return draw_remote_form(f, app, form);
    }

    let subdir_rows = if form.subdirs.is_empty() {
        0
    } else {
        form.subdirs.len() as u16 + 2
    };
    let height = (form.recent.len() as u16).min(12) + subdir_rows + 9;
    let area = centered(70, height, f.area());
    f.render_widget(Clear, area);

    let mut lines = kind_lines(form);
    lines.extend([
        Line::from(Span::styled(
            " working directory:",
            Style::default().fg(theme::muted()),
        )),
        Line::from(vec![
            Span::styled(" > ", Style::default().fg(theme::accent())),
            Span::styled(
                form.input.clone(),
                if form.cursor == 0 {
                    Style::default()
                        .fg(theme::text())
                        .add_modifier(Modifier::UNDERLINED)
                } else {
                    Style::default().fg(theme::muted())
                },
            ),
            Span::styled(
                if form.cursor == 0 { "_" } else { "" },
                Style::default().fg(theme::accent()),
            ),
        ]),
    ]);

    if !form.subdirs.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " subfolders:  (right/tab enter · left up)",
            Style::default().fg(theme::muted()),
        )));
        for (i, p) in form.subdirs.iter().enumerate() {
            let selected = form.cursor == i + 1;
            let label = p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string());
            lines.push(Line::from(vec![
                Span::styled(
                    if selected { " > " } else { "   " },
                    Style::default().fg(theme::accent()),
                ),
                Span::styled(
                    format!("{}{}", truncate(&label, 60), std::path::MAIN_SEPARATOR),
                    if selected {
                        Style::default().fg(theme::text()).bold()
                    } else {
                        Style::default().fg(theme::text())
                    },
                ),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " recent projects:",
        Style::default().fg(theme::muted()),
    )));

    for (i, p) in form.recent.iter().take(12).enumerate() {
        let selected = form.cursor == form.subdirs.len() + i + 1;
        let label = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string());
        lines.push(Line::from(vec![
            Span::styled(
                if selected { " > " } else { "   " },
                Style::default().fg(theme::accent()),
            ),
            Span::styled(
                format!("{:<22}", truncate(&label, 22)),
                if selected {
                    Style::default().fg(theme::text()).bold()
                } else {
                    Style::default().fg(theme::text())
                },
            ),
            Span::styled(shorten_path(p, 36), Style::default().fg(theme::faint())),
        ]));
    }

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::accent()))
        .title(Line::from(Span::styled(
            " new session ",
            Style::default().fg(theme::accent()).bold(),
        )));

    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// The run-kind switch at the top of the new-session dialog, and what the
/// picked kind does.
fn kind_lines(form: &NewSessionForm) -> Vec<Line<'static>> {
    let mut kinds = vec![Span::styled(" run:  ", Style::default().fg(theme::muted()))];
    for k in SpawnKind::ALL {
        kinds.push(if k == form.kind {
            Span::styled(
                format!("[{}]", k.name()),
                Style::default().fg(theme::accent()).bold(),
            )
        } else {
            Span::styled(
                format!(" {} ", k.name()),
                Style::default().fg(theme::faint()),
            )
        });
        kinds.push(Span::raw(" "));
    }
    kinds.push(Span::styled(
        " ctrl+r switches",
        Style::default().fg(theme::faint()),
    ));
    let hint = match form.kind {
        SpawnKind::Local => " claude on this machine",
        SpawnKind::Remote => " claude --cloud: new session on claude.ai/code for the repository",
        SpawnKind::Teleport => " claude --teleport: lists every remote session, pulls one here",
    };
    vec![
        Line::from(kinds),
        Line::from(Span::styled(hint, Style::default().fg(theme::faint()))),
        Line::from(""),
    ]
}

/// How many repositories the remote form shows at once.
const REPO_ROWS: usize = 14;

/// The new-session dialog in remote mode: the repositories the account can
/// reach, filtered by what was typed.
fn draw_remote_form(f: &mut Frame, app: &App, form: &NewSessionForm) {
    let repos = app.filtered_repos();
    let area = centered(76, REPO_ROWS as u16 + 9, f.area());
    f.render_widget(Clear, area);

    let mut lines = kind_lines(form);
    lines.push(Line::from(vec![
        Span::styled(" repository: ", Style::default().fg(theme::muted())),
        Span::styled(
            form.repo_filter.clone(),
            Style::default()
                .fg(theme::text())
                .add_modifier(Modifier::UNDERLINED),
        ),
        Span::styled("_", Style::default().fg(theme::accent())),
    ]));

    let status = if let Some(name) = &app.cloning {
        Some(format!(" preparing {name}…"))
    } else if app.repos_loading() && app.repos.is_none() {
        Some(" fetching repositories from GitHub…".to_string())
    } else {
        app.repos
            .as_ref()
            .and_then(|l| l.note.clone())
            .map(|n| format!(" {n}"))
    };
    lines.push(Line::from(Span::styled(
        status.unwrap_or_default(),
        Style::default().fg(theme::faint()),
    )));

    if repos.is_empty() && !app.repos_loading() {
        lines.push(Line::from(Span::styled(
            " no repository — enter uses the directory from the local form",
            Style::default().fg(theme::muted()),
        )));
    }
    // The window follows the cursor, so a long list scrolls instead of cutting off.
    let start = form.repo_cursor.saturating_sub(REPO_ROWS - 1);
    for (i, r) in repos.iter().enumerate().skip(start).take(REPO_ROWS) {
        let selected = i == form.repo_cursor;
        let where_ = match &r.local {
            Some(p) => shorten_path(p, 30),
            None => "clone on start".to_string(),
        };
        lines.push(Line::from(vec![
            Span::styled(
                if selected { " > " } else { "   " },
                Style::default().fg(theme::accent()),
            ),
            Span::styled(
                format!("{:<38}", truncate(&r.full_name, 38)),
                if selected {
                    Style::default().fg(theme::text()).bold()
                } else {
                    Style::default().fg(theme::text())
                },
            ),
            Span::styled(
                if r.private { "private " } else { "        " },
                Style::default().fg(theme::muted()),
            ),
            Span::styled(where_, Style::default().fg(theme::faint())),
        ]));
    }

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::accent()))
        .title(Line::from(Span::styled(
            " new remote session ",
            Style::default().fg(theme::accent()).bold(),
        )));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// The list of past conversations: where each was held, what it was asked
/// for, and how long ago it was last alive.
fn draw_resume(f: &mut Frame, app: &App) {
    let Some(picker) = &app.resume else { return };

    let area = centered(92, (picker.items.len() as u16).min(20) + 4, f.area());
    f.render_widget(Clear, area);

    // The summary takes whatever the fixed columns leave, so a narrow terminal
    // shortens the prompt rather than pushing the age off the edge.
    let inner = area.width.saturating_sub(2) as usize;
    let summary_width = inner.saturating_sub(3 + 18 + 1 + 4 + 1);

    let mut lines = vec![Line::from(Span::styled(
        " enter resumes, esc closes",
        Style::default().fg(theme::faint()),
    ))];

    let now = SystemTime::now();
    for (i, c) in picker.items.iter().enumerate().take(20) {
        let selected = i == picker.cursor;
        lines.push(Line::from(vec![
            Span::styled(
                if selected { " > " } else { "   " },
                Style::default().fg(theme::accent()),
            ),
            Span::styled(
                format!("{:<18}", truncate(&c.cwd_label(), 17)),
                Style::default().fg(theme::muted()),
            ),
            Span::styled(
                format!("{:<summary_width$}", truncate(&c.summary, summary_width)),
                if selected {
                    Style::default().fg(theme::text()).bold()
                } else {
                    Style::default().fg(theme::text())
                },
            ),
            Span::styled(
                format!(" {:>4}", fmt_age(now, c.modified)),
                Style::default().fg(theme::faint()),
            ),
        ]));
    }

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::accent()))
        .title(Line::from(Span::styled(
            " resume a conversation ",
            Style::default().fg(theme::accent()).bold(),
        )));

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_branches(f: &mut Frame, app: &App) {
    let Some(picker) = &app.branches else { return };
    let rows = picker.rows();
    const SHOWN: usize = 16;

    let area = centered(60, (rows.len().clamp(1, SHOWN) as u16) + 4, f.area());
    f.render_widget(Clear, area);
    let width = area.width.saturating_sub(2) as usize;
    let name_width = width.saturating_sub(3 + 5 + 1);

    let mut lines = vec![Line::from(vec![
        Span::styled(" > ", Style::default().fg(theme::accent())),
        Span::styled(
            picker.filter.clone(),
            Style::default().fg(theme::text()).bold(),
        ),
        Span::styled("_", Style::default().fg(theme::faint())),
    ])];
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "   no branches",
            Style::default().fg(theme::muted()),
        )));
    }

    // The window scrolls with the cursor once the list outgrows it.
    let start = picker.cursor.saturating_sub(SHOWN - 1);
    let now = SystemTime::now();
    for (i, row) in rows.iter().enumerate().skip(start).take(SHOWN) {
        let selected = i == picker.cursor;
        let marker = Span::styled(
            if selected { " > " } else { "   " },
            Style::default().fg(theme::accent()),
        );
        let line = match row {
            BranchRow::Branch(b) => {
                let color = if b.current {
                    theme::accent()
                } else if b.remote {
                    theme::muted()
                } else {
                    theme::text()
                };
                let mut style = Style::default().fg(color);
                if selected {
                    style = style.bold();
                }
                let label = if b.current {
                    format!("{} *", b.name)
                } else {
                    b.name.clone()
                };
                let then = UNIX_EPOCH + Duration::from_secs(b.time);
                Line::from(vec![
                    marker,
                    Span::styled(
                        format!("{:<name_width$}", truncate(&label, name_width)),
                        style,
                    ),
                    Span::styled(
                        format!(" {:>4}", fmt_age(now, then)),
                        Style::default().fg(theme::faint()),
                    ),
                ])
            }
            BranchRow::Create(name) => Line::from(vec![
                marker,
                Span::styled("+ new branch ", Style::default().fg(theme::accent())),
                Span::styled(
                    truncate(name, name_width.saturating_sub(13)),
                    Style::default().fg(theme::text()).bold(),
                ),
            ]),
        };
        lines.push(line);
    }

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::accent()))
        .title(Line::from(Span::styled(
            " switch branch ",
            Style::default().fg(theme::accent()).bold(),
        )))
        .title_bottom(Line::from(Span::styled(
            " type to filter, enter switches, esc closes ",
            Style::default().fg(theme::faint()),
        )));

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_confirm_kill(f: &mut Frame, app: &App) {
    let label = app
        .selected_session()
        .map(|s| s.label.clone())
        .unwrap_or_default();
    let area = centered(46, 5, f.area());
    f.render_widget(Clear, area);

    let p = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  kill session ", Style::default().fg(theme::text())),
            Span::styled(label, Style::default().fg(theme::accent()).bold()),
            Span::styled("?  ", Style::default().fg(theme::text())),
        ]),
        Line::from(Span::styled(
            "  y = yes      n / esc = no",
            Style::default().fg(theme::muted()),
        )),
    ])
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::dead())),
    );
    f.render_widget(p, area);
}

fn draw_confirm_restart(f: &mut Frame, app: &App) {
    let live = app.sessions.iter().filter(|s| s.is_alive()).count();
    let area = centered(62, 8, f.area());
    f.render_widget(Clear, area);

    let p = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "  restart the fleet into the new build",
            Style::default().fg(theme::text()),
        )),
        Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(format!("{live}"), Style::default().fg(theme::dead()).bold()),
            Span::styled(
                " sessions should be recovered using --resume flag",
                Style::default().fg(theme::muted()),
            ),
        ]),
        Line::from(Span::styled(
            "  if agent is working update may interrupt it",
            Style::default().fg(theme::muted()),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  y = restart      n / esc = leave it",
            Style::default().fg(theme::muted()),
        )),
    ])
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::ask())),
    );
    f.render_widget(p, area);
}

fn draw_confirm_mkdir(f: &mut Frame, app: &App) {
    let Some(path) = &app.pending_mkdir else {
        return;
    };
    let area = centered(64, 7, f.area());
    f.render_widget(Clear, area);

    let p = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "  directory does not exist:",
            Style::default().fg(theme::text()),
        )),
        Line::from(Span::styled(
            format!("  {}", shorten_path(path, 58)),
            Style::default().fg(theme::accent()).bold(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  create it?   y / enter = yes      n / esc = no",
            Style::default().fg(theme::muted()),
        )),
    ])
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::accent())),
    );
    f.render_widget(p, area);
}

fn draw_help(f: &mut Frame) {
    let rows: &[(&str, &str)] = &[
        ("", "-- NAVIGATION --"),
        ("up/down, j/k", "select a session"),
        ("enter / tab", "enter the session (focus)"),
        ("F1 .. F9", "jump to a session (always works)"),
        ("", "the first free F starts a new session"),
        ("n", "new session"),
        ("s", "a command shell where the selected session works"),
        ("", "(cmd.exe from %COMSPEC%, or $SHELL; not restored)"),
        ("u", "understand project (see below)"),
        ("x", "kill the selected session"),
        ("w", "close a finished session's card now"),
        ("", "(finished ones go by themselves after a minute)"),
        ("r", "restart into a new build (when a newer exe exists)"),
        (
            "",
            "(sessions come back with their conversations, --resume)",
        ),
        ("i", "install a newer release from GitHub, then restart"),
        (
            "",
            "(checked every five minutes, or now when none is known)",
        ),
        ("R", "resume an old conversation (transcript list)"),
        ("g", "browse the git panel (see below)"),
        ("G", "show or hide the git panel (hidden at start)"),
        ("b", "switch branch (type a new name to create one)"),
        ("alt+g", "git panel from anywhere, again to go back"),
        ("alt+shift+g", "show or hide the git panel from anywhere"),
        ("click", "a card selects it, the pane focuses it,"),
        ("", "the git panel browses it"),
        ("drag a border", "resize the sidebar or the git panel"),
        ("[ / ]", "sidebar narrower / wider"),
        ("{ / }", "git panel narrower / wider"),
        ("", "(the widths are kept for the next start)"),
        ("U", "refresh the account limits now"),
        ("", "(a hidden /usage session, no tokens)"),
        ("q", "quit"),
        ("", ""),
        ("", "-- FOCUS --"),
        ("F10", "LEAVE FOCUS"),
        ("everything else", "goes to Claude, including Ctrl+anything"),
        ("mouse wheel", "scroll the history"),
        ("", ""),
        ("", "-- ALWAYS WORKS --"),
        ("F1 .. F9", "jump to a session"),
        ("", "F on the first free slot = new session"),
        ("", "(in the selected session's directory, no dialog)"),
        ("F11", "new session"),
        ("F12", "this help"),
        ("", ""),
        ("", "-- LIVE CONFIG --"),
        ("", "~/.claude/fleet.toml — colours, labels, timings"),
        ("", "a save shows up at once, sessions live on"),
        ("", ""),
        ("", "-- UNDERSTAND PROJECT --"),
        ("u", "arms it, then the target:"),
        ("", "u F1..F9 = that session (free slot = a new one)"),
        ("", "u u = new session, no directory dialog"),
        ("", "u n = new session with the directory dialog"),
        ("", "u enter = the selected session"),
        ("F1..F9, then u", "the same thing, the other way round"),
        ("", "(a lone u counts for 2 s after entering a session)"),
        ("", "the text lands in the prompt, enter sends it"),
        ("", ""),
        ("", "-- GIT PANEL (g) --"),
        ("", "the selected session's repository: branch, changes"),
        ("", "with lines +added -removed, history with authors"),
        ("up/down, j/k", "move; the pane previews the diff or commit"),
        ("enter / space", "open or close a commit or a section"),
        ("right / left", "open / close (left at the top leaves)"),
        ("pgup/pgdn, J/K", "scroll the preview (the wheel works too)"),
        ("", "\u{2022} = committed while the session ran"),
        ("", "yellow hash = not pushed yet"),
        ("b", "switch branch; a remote one is checked out"),
        ("", "tracking it, a new name starts a branch at HEAD"),
        ("c", "type the commit message (or click the box)"),
        ("m / ctrl+g", "have claude-haiku-4-5 write the message"),
        ("enter", "commit: the staged files, or all if none are"),
        ("", "an empty message is written first, then committed"),
        ("shift+enter", "a new line in the message"),
        ("p / ctrl+p", "push; a branch without upstream gets origin"),
        ("", "rejected as behind: pulls with rebase, pushes again"),
        ("P", "pull: fetch, rebase onto upstream, autostash"),
        ("f", "fetch, so the ↓ count is current"),
        ("esc / g", "back to the list"),
        ("", ""),
        ("", "-- EDITOR (e, alt+e) --"),
        ("", "a file tree and tabs of open files, in the pane;"),
        ("", "the session's directory, git changes marked"),
        ("tree: enter", "open a file or a folder (left/right fold)"),
        (
            "tree: a / r / d",
            "new file (dir/ = folder), rename, delete",
        ),
        ("ctrl+s / ctrl+w", "save / close the tab"),
        ("ctrl+z / ctrl+y", "undo / redo"),
        ("ctrl+f", "find; enter next, shift+enter previous"),
        ("ctrl+h / ctrl+r", "replace all"),
        ("ctrl+g", "go to line"),
        ("ctrl+p", "open a file by name"),
        (
            "ctrl+c / x / v",
            "copy, cut, paste (the line if none selected)",
        ),
        ("ctrl+d / ctrl+k", "duplicate / delete the line"),
        ("alt+up / alt+down", "move the line"),
        ("ctrl+/", "comment the lines in or out"),
        ("esc", "text -> tree -> back to the list"),
        ("", "files changed on disk reload; unsaved edits stay"),
        ("", ""),
        ("", "-- BIG BROTHER --"),
        (
            "t, then a-z",
            "put the selected session in a group (- = none)",
        ),
        ("B, then a-z", "start a BIG BROTHER over that group"),
        (
            "B, then *",
            "... over every session (enter = selected's group)",
        ),
        ("", "a claude session that watches the others, reads"),
        ("", "their screens and transcripts, and reports what"),
        ("", "looks wrong; it can send, clear, kill and spawn"),
        ("", "sessions in its scope when you tell it to"),
        ("A", "the reports it filed (warn/alarm ring a bell)"),
        ("", ""),
        ("", "-- FOREIGN SESSIONS --"),
        ("!", "started outside fleet, view only"),
        ("", "their PTY belongs to another terminal"),
    ];

    // Wide enough for the longest description next to its key column, so
    // nothing wraps and the height below stays one row per entry.
    let area = centered(80, rows.len() as u16 + 2, f.area());
    f.render_widget(Clear, area);

    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, v)| {
            if k.is_empty() {
                Line::from(Span::styled(
                    format!("  {v}"),
                    Style::default().fg(theme::accent_dim()).bold(),
                ))
            } else {
                Line::from(vec![
                    Span::styled(
                        format!("  {k:<20}"),
                        Style::default().fg(theme::accent()).bold(),
                    ),
                    Span::styled(*v, Style::default().fg(theme::text())),
                ])
            }
        })
        .collect();

    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::accent()))
                .title(Line::from(Span::styled(
                    " shortcuts ",
                    Style::default().fg(theme::accent()).bold(),
                ))),
        ),
        area,
    );
}

/// A colour per group letter, so a group reads as one at a glance.
fn group_color(g: char) -> Color {
    let palette = [
        theme::accent(),
        theme::ask(),
        theme::idle(),
        theme::busy(),
        theme::dead(),
    ];
    palette[(g as usize).wrapping_sub('a' as usize) % palette.len()]
}

fn draw_tag(f: &mut Frame, app: &App) {
    let Some(s) = app.selected_session() else {
        return;
    };
    let groups = app.groups();
    let in_use = if groups.is_empty() {
        "no groups yet".to_string()
    } else {
        groups
            .iter()
            .map(|(g, n)| format!("{g}:{n}"))
            .collect::<Vec<_>>()
            .join("  ")
    };
    let area = centered(56, 7, f.area());
    f.render_widget(Clear, area);
    let p = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  group for ", Style::default().fg(theme::text())),
            Span::styled(s.label.clone(), Style::default().fg(theme::accent()).bold()),
        ]),
        Line::from(Span::styled(
            format!("  in use: {in_use}"),
            Style::default().fg(theme::muted()),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  a-z = that group    - = none    esc = cancel",
            Style::default().fg(theme::muted()),
        )),
    ])
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::accent())),
    );
    f.render_widget(p, area);
}

fn draw_big_brother(f: &mut Frame, app: &App) {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  a claude session that watches the others and reports",
            Style::default().fg(theme::text()),
        )),
        Line::from(Span::styled(
            "  what looks wrong. What should it watch?",
            Style::default().fg(theme::text()),
        )),
        Line::from(""),
    ];
    for (g, n) in app.groups() {
        let plural = if n == 1 { "" } else { "s" };
        lines.push(Line::from(vec![
            Span::styled(
                format!("   {g}  "),
                Style::default().fg(group_color(g)).bold(),
            ),
            Span::styled(
                format!("group {g} — {n} session{plural}"),
                Style::default().fg(theme::muted()),
            ),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("   *  ", Style::default().fg(theme::accent()).bold()),
        Span::styled("every session", Style::default().fg(theme::muted())),
    ]));
    lines.push(Line::from(""));
    let default = match app.default_scope() {
        Scope::All => "all".to_string(),
        Scope::Group(g) => format!("group {g}"),
    };
    lines.push(Line::from(Span::styled(
        format!("  a-z / * = pick    enter = {default}    esc = cancel"),
        Style::default().fg(theme::muted()),
    )));
    let area = centered(62, lines.len() as u16 + 2, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::accent()))
                .title(Line::from(Span::styled(
                    " BIG BROTHER ",
                    Style::default().fg(theme::accent()).bold(),
                ))),
        ),
        area,
    );
}

fn draw_reports(f: &mut Frame, app: &App) {
    let full = f.area();
    let width = full.width.saturating_sub(8).clamp(40, 110);
    let height = full.height.saturating_sub(4).max(6);
    let area = centered(width, height, full);
    f.render_widget(Clear, area);
    let text_w = usize::from(width).saturating_sub(7).max(10);

    // Newest first; the scroll steps back into older ones.
    let mut lines: Vec<Line> = Vec::new();
    for r in app.reports.iter().rev().skip(app.reports_scroll) {
        let color = match r.level {
            Level::Alarm => theme::dead(),
            Level::Warn => theme::busy(),
            Level::Info => theme::idle(),
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {:<5} ", r.level.name()),
                Style::default().fg(color).bold(),
            ),
            Span::styled(r.from.clone(), Style::default().fg(theme::accent())),
            Span::styled(
                format!("  {} ago", fmt_uptime(r.at.elapsed())),
                Style::default().fg(theme::faint()),
            ),
        ]));
        for chunk in wrap_message(&r.text, text_w) {
            lines.push(Line::from(Span::styled(
                format!("   {chunk}"),
                Style::default().fg(theme::text()),
            )));
        }
        lines.push(Line::from(""));
    }
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::accent()))
                .title(Line::from(Span::styled(
                    format!(" BIG BROTHER reports ({}) ", app.reports.len()),
                    Style::default().fg(theme::accent()).bold(),
                ))),
        ),
        area,
    );
}

/// A push that failed: the kind of failure, git's words, the branch against
/// its remote, and the buttons for what to do about it.
fn draw_push_failed(f: &mut Frame, app: &mut App) {
    let Some(failed) = app.push_failed.as_mut() else {
        return;
    };
    let full = f.area();
    let width = full.width.saturating_sub(8).clamp(40, 110);
    let height = full.height.saturating_sub(4).max(10);
    let area = centered(width, height, full);
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::dead()))
        .title(Line::from(Span::styled(
            " push failed ",
            Style::default().fg(theme::dead()).bold(),
        )));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [head, body, buttons] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let text_w = usize::from(inner.width).saturating_sub(2).max(10);

    let kind = failed.failure.kind;
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(" error type  ", Style::default().fg(theme::muted())),
                Span::styled(
                    format!(" {} ", kind.name()),
                    Style::default()
                        .bg(theme::dead())
                        .fg(theme::surface())
                        .bold(),
                ),
            ]),
            Line::from(Span::styled(
                format!(" {}", truncate(&failed.failure.reason, text_w)),
                Style::default().fg(theme::text()),
            )),
            Line::from(""),
        ]),
        head,
    );

    let section = |title: &str| {
        Line::from(Span::styled(
            format!(" {title}"),
            Style::default().fg(theme::accent()).bold(),
        ))
    };
    let mut lines = vec![section("git said")];
    for row in wrap_message(&failed.failure.output, text_w) {
        lines.push(Line::from(Span::styled(
            format!("  {row}"),
            Style::default().fg(theme::text()),
        )));
    }
    if !failed.failure.log.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("git log"));
        for row in wrap_message(&failed.failure.log, text_w) {
            lines.push(Line::from(Span::styled(
                format!("  {row}"),
                Style::default().fg(theme::muted()),
            )));
        }
    }
    let max_scroll = lines.len().saturating_sub(usize::from(body.height));
    failed.scroll = failed.scroll.min(max_scroll);
    let scroll = u16::try_from(failed.scroll).unwrap_or(u16::MAX);
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), body);

    let choices = [
        (
            GitHit::FixPush,
            " ✦ f  let Claude fix it ",
            Style::default()
                .bg(theme::accent())
                .fg(theme::surface())
                .bold(),
        ),
        (
            GitHit::RetryPush,
            " r  push again ",
            Style::default()
                .bg(theme::surface())
                .fg(theme::text())
                .bold(),
        ),
        (
            GitHit::CloseDialog,
            " esc  close ",
            Style::default().bg(theme::surface()).fg(theme::muted()),
        ),
    ];
    let mut spans = vec![Span::raw(" ")];
    let mut x = buttons.x + 1;
    for (hit, label, style) in choices {
        let w = (Span::raw(label).width() as u16).min(buttons.right().saturating_sub(x));
        if w == 0 {
            break;
        }
        app.git_hits.push((Rect::new(x, buttons.y, w, 1), hit));
        spans.push(Span::styled(label, style));
        spans.push(Span::raw("  "));
        x = x.saturating_add(w + 2);
    }
    f.render_widget(Paragraph::new(Line::from(spans)), buttons);
}

fn centered(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!(
        "{}~",
        s.chars().take(max.saturating_sub(1)).collect::<String>()
    )
}

fn shorten_path(p: &std::path::Path, max: usize) -> String {
    truncate_left(&p.display().to_string(), max)
}

/// `truncate` from the other end: the tail is kept, since that is where a
/// path's file name is.
fn truncate_left(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    let tail: String = s.chars().skip(len - max.saturating_sub(1)).collect();
    format!("~{tail}")
}

/// Time left before a finished card is dropped, e.g. `gone in 42s`.
fn fmt_countdown(d: Duration) -> String {
    format!("gone in {}s", d.as_secs())
}

fn fmt_bytes(n: usize) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{} KB", n / 1024),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

/// How long ago something happened, in one unit: `12m`, `5h`, `3d`.
///
/// A transcript's age is read against the other rows rather than for itself,
/// so the widest unit that still separates them is the useful one.
fn fmt_age(now: SystemTime, then: SystemTime) -> String {
    let secs = now.duration_since(then).unwrap_or_default().as_secs();
    match secs {
        0..=59 => "now".to_string(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

pub fn fmt_uptime(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_editor_draws_the_tree_the_tabs_and_the_code() {
        use ratatui::{Terminal, backend::TestBackend};

        let dir = std::env::temp_dir().join(format!("fleet-ui-ide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {\n\tlet x = 1;\n}\n").unwrap();

        let mut app = App::new(dir.clone());
        app.open_ide();
        assert!(app.ide.open(&dir.join("src/main.rs")));
        app.ide.tree.reveal(&dir.join("src/main.rs"));
        app.ide.buffers[0].select_all();

        let mut term = Terminal::new(TestBackend::new(140, 30)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let screen: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(screen.contains("FILES"), "{screen}");
        assert!(screen.contains(" main.rs "), "{screen}");
        assert!(screen.contains("fn main() {"), "{screen}");
        // The tab is expanded, not printed.
        assert!(screen.contains("    let x = 1;"), "{screen}");
        assert!(screen.contains("ln 4/4"), "{screen}");
        // The mouse knows where the text went.
        assert!(app.ide.areas.text.width > 0);
        assert!(!app.ide.areas.tabs.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_fleet_turns_its_name_above_the_keys() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut app = App::new(std::env::temp_dir());
        let mut term = Terminal::new(TestBackend::new(150, 40)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let pane = pane_inner_rect(pane_area(buf.area, app.show_git));
        let shaded = (pane.y..pane.bottom())
            .flat_map(|y| (pane.x..pane.right()).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let s = buf[(x, y)].symbol();
                s.len() == 1 && ".,-~:;=!*#$@".contains(s)
            })
            .count();
        assert!(shaded > 200, "{shaded}");
        let screen: String = (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_string())
            .collect();
        assert!(screen.contains("new session in a directory you pick"));
    }

    fn window(pct: u8, secs: u64, expired: bool) -> usage::Window {
        usage::Window {
            pct,
            resets_in: (!expired).then(|| Duration::from_secs(secs)),
            expired,
        }
    }

    #[test]
    fn a_limit_row_fits_the_sidebar() {
        // The sidebar is a fixed width and the rows have no room to wrap, so
        // the widest thing they can hold has to still fit between the borders.
        let inner = usize::from(SIDEBAR_WIDTH) - 2;
        for w in [
            window(0, 59 * 60, false),
            window(100, 6 * 86_400 + 3 * 3600, false),
            window(87, 0, true),
        ] {
            for label in ["session", "weekly"] {
                for row in usage_rows(label, &w, inner) {
                    assert!(
                        row.width() <= inner,
                        "{label} {w:?} takes {} of {inner}",
                        row.width()
                    );
                }
            }
        }
    }

    #[test]
    fn git_rows_fit_the_panel_and_its_height() {
        let snap = git::Snapshot {
            root: "C:/work/repo".into(),
            branch: Some("feature/a-branch-name-far-longer-than-the-panel".into()),
            ahead: 12,
            behind: 3,
            upstream: Some("origin/x".into()),
            changes: (0..50)
                .map(|i| git::Change {
                    code: " M".into(),
                    path: format!("src/some/deeply/nested/directory/file_{i}.rs"),
                    added: Some(123_456),
                    removed: (i % 2 == 0).then_some(98_765),
                })
                .collect(),
            changes_total: 50,
            added: 4_000_000,
            removed: 3_000_000,
            log: (0..60)
                .map(|i| git::Commit {
                    hash: "41c45ad".into(),
                    subject: "a subject line that goes on well past the panel's edge"
                        .repeat(i % 3 + 1),
                    time: 1_700_000_000,
                    author: "Bartholomew Longname".into(),
                })
                .collect(),
        };
        let width = usize::from(GIT_WIDTH) - 2;
        let height = 40;
        for focused in [false, true] {
            let mut view = GitView::new();
            let lines = git_lines(
                &mut view,
                &snap,
                Some(SystemTime::now()),
                width,
                height,
                focused,
            );
            assert!(lines.len() <= height, "{} rows for {height}", lines.len());
            for l in &lines {
                assert!(l.width() <= width, "a row takes {} of {width}", l.width());
            }
        }
    }

    #[test]
    fn a_commit_message_wraps_at_the_box_and_keeps_its_blank_lines() {
        assert_eq!(wrap_message("", 10), vec![String::new()]);
        assert_eq!(wrap_message("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(wrap_message("subject\n\nbody", 20), ["subject", "", "body"]);
    }

    #[test]
    fn the_share_bar_is_always_five_cells() {
        for (a, r) in [(0, 0), (1, 0), (0, 1), (1, 1000), (999, 1), (3, 3)] {
            let w: usize = share_bar(a, r).iter().map(Span::width).sum();
            assert_eq!(w, 5, "+{a} -{r}");
        }
    }

    #[test]
    fn the_bar_spans_the_sidebar_whatever_it_is_filled_to() {
        // Filled and unfilled together are the bar, so the pair always covers
        // the same columns; otherwise the two windows would not line up.
        let inner = usize::from(SIDEBAR_WIDTH) - 2 - USAGE_GUTTER * 2;
        for pct in 0..=100u8 {
            let (filled, empty) = usage_bar(pct, inner);
            assert_eq!(
                filled.chars().count() + empty.chars().count(),
                inner,
                "{pct}% has the wrong width"
            );
        }
    }

    #[test]
    fn an_age_reads_in_one_unit_and_says_now_for_the_last_minute() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 86_400);
        let ago = |secs: u64| fmt_age(now, now - Duration::from_secs(secs));
        assert_eq!(ago(5), "now");
        assert_eq!(ago(12 * 60), "12m");
        assert_eq!(ago(5 * 3600), "5h");
        assert_eq!(ago(3 * 86_400 + 3600), "3d");
        // A stamp from the future is not a negative age.
        assert_eq!(fmt_age(now, now + Duration::from_secs(60)), "now");
    }

    #[test]
    fn a_reset_reads_in_the_widest_unit_that_still_says_something() {
        assert_eq!(fmt_reset(&window(0, 47 * 60, false)), "47m");
        assert_eq!(fmt_reset(&window(0, 2 * 3600 + 14 * 60, false)), "2h14m");
        assert_eq!(fmt_reset(&window(0, 6 * 86_400 + 3 * 3600, false)), "6d3h");
        assert_eq!(fmt_reset(&window(0, 0, true)), "stale");
    }

    #[test]
    fn the_bar_only_fills_completely_at_a_full_window() {
        // A window with anything left in it must not look spent.
        let width = 30;
        assert_ne!(
            usage_bar(99, width).0,
            usage_bar(100, width).0,
            "99% looks full"
        );
        assert_eq!(usage_bar(100, width).0, "\u{2588}".repeat(width));
        assert_eq!(usage_bar(0, width).0, "");
    }

    #[test]
    fn a_partial_cell_marks_what_a_whole_cell_could_not() {
        // Two percentages landing inside the same cell still differ on screen.
        let width = 30;
        assert_ne!(usage_bar(41, width).0, usage_bar(42, width).0);
    }
}
