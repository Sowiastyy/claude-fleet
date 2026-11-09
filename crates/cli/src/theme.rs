//! Colour palette, kept in one place so panes and chrome stay consistent.
//!
//! The values themselves live in the config file and can change while the
//! fleet runs, so these are accessors rather than constants. Each one is a
//! read lock and a `Copy` out of it; a frame takes a few hundred of those and
//! never notices.

use ratatui::style::Color;

use crate::config;

pub fn accent() -> Color {
    config::theme().accent
}

pub fn accent_dim() -> Color {
    config::theme().accent_dim
}

pub fn text() -> Color {
    config::theme().text
}

pub fn muted() -> Color {
    config::theme().muted
}

pub fn faint() -> Color {
    config::theme().faint
}

pub fn busy() -> Color {
    config::theme().busy
}

pub fn idle() -> Color {
    config::theme().idle
}

/// A session stopped on a question of its own: a permission prompt, a dialog,
/// anything waiting on a keypress that only the user can supply.
pub fn ask() -> Color {
    config::theme().ask
}

pub fn dead() -> Color {
    config::theme().dead
}

pub fn surface() -> Color {
    config::theme().surface
}

/// The filled part of a limit bar. `theme.bar` in the config when it is set,
/// the accent when it is not.
pub fn bar() -> Color {
    config::theme().bar
}

/// The unfilled part of the same bar, dim enough to read as empty and bright
/// enough not to vanish into the background.
pub fn bar_empty() -> Color {
    config::theme().bar_empty
}
