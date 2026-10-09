//! Inline slash-command menu (pure logic).
//!
//! Holds the list of commands, the current filter, fuzzy match scoring, and
//! selection state. No rendering, no I/O — a reducer-friendly value type.
//!
//! `is_open_for` keeps completion within single-line command/subcommand
//! names. Arguments belong to the composer, not the menu. The caller
//! constructs the menu when opened and drops it when closed.

pub(crate) mod menu;
pub(crate) mod popup;

pub(crate) use menu::{SlashItem, SlashMenu, is_open_for, score_token};

#[cfg(test)]
mod tests;
