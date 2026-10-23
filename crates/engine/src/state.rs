//! Engine state and line snapshots.

use std::sync::atomic::{AtomicU64, Ordering};

/// A monotonically increasing revision for the current editor line.
///
/// Every request the engine makes carries one. A result whose revision does not
/// match the current line is discarded rather than applied, which is how stale
/// asynchronous results are prevented from corrupting the display.
#[derive(Debug, Default)]
pub struct LineSnapshot {
    revision: AtomicU64,
}

impl LineSnapshot {
    /// Create a snapshot at revision 0.
    pub const fn new() -> Self {
        Self { revision: AtomicU64::new(0) }
    }

    /// The current revision.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// Bump the revision, returning the new value.
    pub fn bump(&self) -> u64 {
        self.revision.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Whether a result tagged with `candidate` is still current.
    pub fn accepts(&self, candidate: u64) -> bool {
        candidate == self.revision()
    }
}

/// Whether the engine is running, degraded, or not started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineStatus {
    /// Not yet initialised; nothing has been requested.
    Idle,
    /// Working normally.
    Active,
    /// A panic was caught at the boundary, or zsh was missing symbols.
    Degraded,
}

/// The engine's shared state.
///
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use completion::{CompletionContext, CompletionOutcome};
use history::HistoryIndex;

/// Deliberately tiny in Stage B. It exists to prove the boundary works before
/// the lexer and completion engines are attached.
#[derive(Debug)]
pub struct EngineState {
    status: EngineStatus,
    line: LineSnapshot,
    /// How many times the highlight entry point has been invoked.
    highlight_calls: AtomicU64,
    /// How many times the completion entry point has been invoked.
    completion_calls: AtomicU64,
    /// In-memory history index.
    history: RwLock<HistoryIndex>,
}

impl Default for EngineState {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineState {
    /// Construct an idle engine.
    pub const fn new() -> Self {
        Self {
            status: EngineStatus::Idle,
            line: LineSnapshot::new(),
            highlight_calls: AtomicU64::new(0),
            completion_calls: AtomicU64::new(0),
            history: RwLock::new(HistoryIndex::new()),
        }
    }

    /// Current status.
    pub fn status(&self) -> EngineStatus {
        self.status
    }

    /// The live line revision.
    pub fn line(&self) -> &LineSnapshot {
        &self.line
    }

    /// Mark the engine active.
    pub fn activate(&mut self) {
        self.status = EngineStatus::Active;
    }

    /// Mark the engine degraded; it will no longer be trusted.
    pub fn degrade(&mut self) {
        self.status = EngineStatus::Degraded;
    }

    /// Record a highlight invocation and advance the line revision.
    pub fn record_highlight(&self) -> u64 {
        self.highlight_calls.fetch_add(1, Ordering::Relaxed);
        self.line.bump()
    }

    /// Total highlight invocations.
    pub fn highlight_calls(&self) -> u64 {
        self.highlight_calls.load(Ordering::Relaxed)
    }

    /// Total completion invocations.
    pub fn completion_calls(&self) -> u64 {
        self.completion_calls.load(Ordering::Relaxed)
    }

    /// Lazily load or refresh history index from a file path.
    pub fn ensure_history_loaded(&self, path: &Path) {
        let mut hist = self.history.write().unwrap_or_else(|e| e.into_inner());
        if hist.is_empty() {
            if let Ok(loaded) = HistoryIndex::load_from_file(path) {
                *hist = loaded;
            }
        } else {
            let _ = hist.refresh();
        }
    }

    /// Compute ghost text extension for the given line and cursor.
    pub fn autosuggest(
        &self,
        line: &[u8],
        cursor: usize,
        cwd: PathBuf,
        home: PathBuf,
    ) -> Option<String> {
        let hist_guard = self.history.read().unwrap_or_else(|e| e.into_inner());
        let ctx = CompletionContext::new(line, cursor, cwd, home);
        let outcome = completion::complete(&ctx, Some(&*hist_guard), 5);
        outcome.autosuggestion
    }

    /// Compute ranked completion candidates for the given line and cursor.
    pub fn complete(
        &self,
        line: &[u8],
        cursor: usize,
        cwd: PathBuf,
        home: PathBuf,
        max_candidates: usize,
    ) -> CompletionOutcome {
        self.completion_calls.fetch_add(1, Ordering::Relaxed);
        let hist_guard = self.history.read().unwrap_or_else(|e| e.into_inner());
        let ctx = CompletionContext::new(line, cursor, cwd, home);
        completion::complete(&ctx, Some(&*hist_guard), max_candidates)
    }

    /// Compute history-only candidates for interactive history search.
    pub fn complete_history(
        &self,
        line: &[u8],
        cursor: usize,
        cwd: PathBuf,
        home: PathBuf,
        max_candidates: usize,
    ) -> CompletionOutcome {
        self.completion_calls.fetch_add(1, Ordering::Relaxed);
        let hist_guard = self.history.read().unwrap_or_else(|e| e.into_inner());
        let ctx = CompletionContext::new(line, cursor, cwd, home);
        completion::complete_history(&ctx, Some(&*hist_guard), max_candidates)
    }

    /// Compute bounded path candidates only.
    pub fn complete_paths(
        &self,
        line: &[u8],
        cursor: usize,
        cwd: PathBuf,
        home: PathBuf,
        max_candidates: usize,
    ) -> CompletionOutcome {
        self.completion_calls.fetch_add(1, Ordering::Relaxed);
        let ctx = CompletionContext::new(line, cursor, cwd, home);
        completion::complete_paths(&ctx, max_candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_starts_at_zero_and_advances() {
        let s = LineSnapshot::new();
        assert_eq!(s.revision(), 0);
        assert_eq!(s.bump(), 1);
        assert_eq!(s.revision(), 1);
    }

    #[test]
    fn stale_revision_is_rejected() {
        let s = LineSnapshot::new();
        let issued = s.bump();
        assert!(s.accepts(issued));
        s.bump();
        assert!(!s.accepts(issued), "an out-of-date result must be refused");
    }

    #[test]
    fn revisions_are_unique_and_monotonic() {
        let s = LineSnapshot::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..10_000 {
            assert!(seen.insert(s.bump()), "revisions must never repeat");
        }
    }

    #[test]
    fn recording_a_highlight_advances_the_revision() {
        let mut e = EngineState::new();
        e.activate();
        assert_eq!(e.status(), EngineStatus::Active);
        let r = e.record_highlight();
        assert_eq!(e.line().revision(), r);
        assert_eq!(e.highlight_calls(), 1);
    }

    #[test]
    fn degraded_is_distinct_from_active() {
        let mut e = EngineState::new();
        e.activate();
        e.degrade();
        assert_eq!(e.status(), EngineStatus::Degraded);
        assert_ne!(e.status(), EngineStatus::Active);
    }
}