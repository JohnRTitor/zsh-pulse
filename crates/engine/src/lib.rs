//! Engine state shared between the zsh module and the Rust core.
//!
//! This crate contains no `unsafe` and no zsh types: it is the seam where the
//! `zsh-module` boundary hands in plain Rust values. That keeps the interesting
//! logic testable without a shell.

#[cfg(not(any(
    feature = "highlight",
    feature = "autocomplete",
    feature = "history",
    feature = "history-navigation"
)))]
compile_error!("At least one feature must be enabled: 'highlight', 'autocomplete', or 'history'.");

pub mod state;
pub mod worker;

pub use state::{EngineState, EngineStatus, LineSnapshot};
