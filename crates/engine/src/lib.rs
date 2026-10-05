//! Engine state shared between the zsh module and the Rust core.
//!
//! This crate contains no `unsafe` and no zsh types: it is the seam where the
//! `zsh-module` boundary hands in plain Rust values. That keeps the interesting
//! logic testable without a shell.

pub mod state;

pub use state::{EngineState, EngineStatus, LineSnapshot};