//! What the fleet's two front ends share: the terminal app (`claude-fleet`)
//! and the desktop GUI.
//!
//! Everything here reads or writes Claude Code's local state, or runs `git` —
//! nothing draws anything.

pub mod accounts;
pub mod claude;
pub mod commitmsg;
pub mod git;
pub mod history;
pub mod registry;
pub mod remote;
pub mod repos;
pub mod update;
pub mod usage;
