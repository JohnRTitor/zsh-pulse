//! Completion engine and candidate providers with bounded execution budgets.
//!
//! # Architecture
//!
//! - **Bounded Filesystem Scanning**: Hard limits on directory entries read
//!   (`MAX_DIR_SCAN`), preventing hangs in directories with tens of thousands of
//!   entries (such as `/nix/store`).
//! - **History Intelligence**: Fast in-memory queries from `crates/history`.
//! - **Fuzzy & Prefix Ranking**: Candidate ranking via `crates/ranking`.
//! - **Autosuggestion**: Computes the optimal ghost text extension (`POSTDISPLAY`)
//!   for the current line.

#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};

use history::HistoryIndex;

/// Maximum entries to inspect when prefix is empty.
pub const MAX_UNFILTERED_DIR_SCAN: usize = 128;

/// Maximum entries to inspect when prefix is filtered.
pub const MAX_FILTERED_DIR_SCAN: usize = 100_000;

/// Maximum filesystem matches to return.
pub const MAX_FS_MATCHES: usize = 64;

/// Default top-K candidate limit.
pub const DEFAULT_MAX_CANDIDATES: usize = 16;

/// The category of a completion candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    /// Normal file.
    File,
    /// Directory (ends with `/`).
    Directory,
    /// Command history entry.
    History,
    /// Shell command, builtin, or executable binary.
    Command,
}

impl CandidateKind {
    /// Returns the lowercase string identifier for this candidate category.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::History => "history",
            Self::Command => "command",
        }
    }
}

/// A completion candidate for menu rendering and insertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The string to insert into the command line buffer.
    pub value: String,
    /// The display label shown in the completion menu.
    pub display: String,
    /// Short description (e.g. "file", "dir", "history").
    pub description: String,
    /// Candidate category.
    pub kind: CandidateKind,
    /// Match score computed by ranking algorithm.
    pub score: i32,
}

/// The context of the line editor at the moment completion is requested.
#[derive(Debug, Clone)]
pub struct CompletionContext {
    /// Full buffer bytes.
    pub line: Vec<u8>,
    /// Byte offset of the cursor.
    pub cursor: usize,
    /// Start byte offset of the word under cursor.
    pub word_start: usize,
    /// End byte offset of the word under cursor.
    pub word_end: usize,
    /// The word fragment up to cursor.
    pub prefix: Vec<u8>,
    /// Whether the cursor is at command position (first word of line or after pipe/separator).
    pub is_command_position: bool,
    /// Current working directory.
    pub cwd: PathBuf,
    /// User home directory.
    pub home: PathBuf,
}

impl CompletionContext {
    /// Parse editor state into a `CompletionContext`.
    pub fn new(line: &[u8], cursor: usize, cwd: PathBuf, home: PathBuf) -> Self {
        let cursor = cursor.min(line.len());

        // Find word boundary before cursor
        let mut start = cursor;
        while start > 0 {
            let b = line[start - 1];
            if b == b' ' || b == b'\t' || b == b';' || b == b'|' || b == b'&' {
                break;
            }
            start -= 1;
        }

        // Find word boundary after cursor
        let mut end = cursor;
        while end < line.len() {
            let b = line[end];
            if b == b' ' || b == b'\t' || b == b';' || b == b'|' || b == b'&' {
                break;
            }
            end += 1;
        }

        let prefix = line[start..cursor].to_vec();

        // Determine if command position: check if preceding tokens before `start`
        // are empty, or end with `;`, `|`, `&&`, `||`, `$(`, `` ` ``.
        let mut is_command_position = true;
        let mut idx = start;
        while idx > 0 {
            let b = line[idx - 1];
            if b == b' ' || b == b'\t' {
                idx -= 1;
                continue;
            }
            is_command_position = b == b';' || b == b'|' || b == b'&' || b == b'(' || b == b'`';
            break;
        }

        Self {
            line: line.to_vec(),
            cursor,
            word_start: start,
            word_end: end,
            prefix,
            is_command_position,
            cwd,
            home,
        }
    }
}

/// The result of completion and autosuggestion generation.
#[derive(Debug, Clone, Default)]
pub struct CompletionOutcome {
    /// Ranked completion candidates.
    pub candidates: Vec<Candidate>,
    /// Inline autosuggestion suffix (ghost text) to show after cursor, if any.
    pub autosuggestion: Option<String>,
    /// Start byte offset in the buffer to replace upon selection.
    pub word_start: usize,
    /// End byte offset in the buffer to replace upon selection.
    pub word_end: usize,
}

/// Complete files and directories with strict scan bounds.
pub fn complete_filesystem(ctx: &CompletionContext) -> Vec<Candidate> {
    let prefix_str = match std::str::from_utf8(&ctx.prefix) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let (dir_path, file_prefix) = split_path_prefix(prefix_str, &ctx.cwd, &ctx.home);

    let entries = match fs::read_dir(&dir_path) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };

    let mut matches = Vec::new();
    let file_prefix_lower = file_prefix.to_ascii_lowercase();
    let max_scan = if file_prefix.is_empty() {
        MAX_UNFILTERED_DIR_SCAN
    } else {
        MAX_FILTERED_DIR_SCAN
    };

    for (scanned, entry) in entries.enumerate() {
        if scanned >= max_scan || matches.len() >= MAX_FS_MATCHES {
            break;
        }

        let Ok(entry) = entry else {
            continue;
        };

        let file_name = entry.file_name();
        let name_bytes = file_name.as_encoded_bytes();
        let name_str = match std::str::from_utf8(name_bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Skip hidden files unless prefix starts with '.'
        if name_str.starts_with('.') && !file_prefix.starts_with('.') {
            continue;
        }

        let is_match = if file_prefix.is_empty() {
            true
        } else {
            name_str.to_ascii_lowercase().starts_with(&file_prefix_lower)
        };

        if !is_match {
            continue;
        }

        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

        // Compute inserted value
        let parent_part = if prefix_str.contains('/') {
            let last_slash = prefix_str.rfind('/').unwrap();
            &prefix_str[..=last_slash]
        } else {
            ""
        };

        let (value, display, kind) = if is_dir {
            (
                format!("{parent_part}{name_str}/"),
                format!("{name_str}/"),
                CandidateKind::Directory,
            )
        } else {
            (
                format!("{parent_part}{name_str}"),
                name_str.to_string(),
                CandidateKind::File,
            )
        };

        let desc = if is_dir { "dir" } else { "file" };

        let score = if is_dir { 10 } else { 0 }
            + if name_str.starts_with(&file_prefix) { 50 } else { 20 };

        matches.push(Candidate {
            value,
            display,
            description: desc.to_string(),
            kind,
            score,
        });
    }

    matches
}

/// Common shell builtins.
pub const SHELL_BUILTINS: &[&str] = &[
    "alias", "autoload", "bg", "bindkey", "break", "builtin", "cd", "command",
    "compadd", "continue", "dirs", "echo", "eval", "exec", "exit", "export",
    "false", "fc", "fg", "functions", "getopts", "hash", "history", "jobs",
    "kill", "let", "local", "popd", "print", "printf", "pushd", "pwd", "read",
    "readonly", "return", "set", "setopt", "shift", "source", "test", "times",
    "trap", "true", "type", "typeset", "ulimit", "umask", "unalias", "unfunction",
    "unhash", "unset", "unsetopt", "wait", "which", "zmodload", "zstyle",
];

/// Complete commands, builtins, and history.
pub fn complete_commands(ctx: &CompletionContext, history: Option<&HistoryIndex>) -> Vec<Candidate> {
    let prefix_str = match std::str::from_utf8(&ctx.prefix) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let mut candidates = Vec::new();

    // 1. Builtins
    for &b in SHELL_BUILTINS {
        if b.starts_with(prefix_str) {
            candidates.push(Candidate {
                value: b.to_string(),
                display: b.to_string(),
                description: "builtin".to_string(),
                kind: CandidateKind::Command,
                score: 100,
            });
        }
    }

    // 2. History commands (if in command position)
    if let Some(hist) = history {
        let hist_matches = hist.search_prefix(ctx.prefix.as_slice(), 8);
        for m in hist_matches {
            if let Ok(m_str) = std::str::from_utf8(m) {
                // If it contains spaces, extract the first command word or entire command
                let first_word = m_str.split_whitespace().next().unwrap_or(m_str);
                if !candidates.iter().any(|c| c.value == first_word) {
                    candidates.push(Candidate {
                        value: first_word.to_string(),
                        display: first_word.to_string(),
                        description: "history".to_string(),
                        kind: CandidateKind::History,
                        score: 80,
                    });
                }
            }
        }
    }

    candidates
}

/// Compute completion candidates and inline autosuggestion for the given context.
pub fn complete(
    ctx: &CompletionContext,
    history: Option<&HistoryIndex>,
    max_candidates: usize,
) -> CompletionOutcome {
    let mut all_candidates = Vec::new();

    if ctx.is_command_position {
        all_candidates.extend(complete_commands(ctx, history));
    }

    // Filesystem completions are valid in both command (e.g. `./script.sh`) and argument positions
    all_candidates.extend(complete_filesystem(ctx));

    // Also include history lines matching prefix if in argument or general position
    if let Some(hist) = history {
        let hist_cmds = hist.search_prefix(ctx.line.as_slice(), 5);
        for h in hist_cmds {
            if let Ok(s) = std::str::from_utf8(h) {
                if !all_candidates.iter().any(|c| c.value == s) {
                    all_candidates.push(Candidate {
                        value: s.to_string(),
                        display: s.to_string(),
                        description: "history".to_string(),
                        kind: CandidateKind::History,
                        score: 70,
                    });
                }
            }
        }
    }

    // Rank candidates using top-k fuzzy / score
    let target_k = max_candidates.min(all_candidates.len());
    let ranked = if all_candidates.len() > target_k && target_k > 0 {
        all_candidates.select_nth_unstable_by(target_k - 1, |a, b| b.score.cmp(&a.score));
        all_candidates.truncate(target_k);
        all_candidates.sort_unstable_by_key(|c| std::cmp::Reverse(c.score));
        all_candidates
    } else {
        all_candidates.sort_unstable_by_key(|c| std::cmp::Reverse(c.score));
        all_candidates
    };

    // Compute autosuggestion (ghost text)
    // Priority:
    // 1. History match extending full line
    // 2. First candidate extending prefix
    let autosuggestion = compute_autosuggestion(ctx, history, &ranked);

    CompletionOutcome {
        candidates: ranked,
        autosuggestion,
        word_start: ctx.word_start,
        word_end: ctx.word_end,
    }
}

/// Compute history-only candidates for interactive history search.
pub fn complete_history(
    ctx: &CompletionContext,
    history: Option<&HistoryIndex>,
    max_candidates: usize,
) -> CompletionOutcome {
    let mut candidates = Vec::new();
    if let Some(hist) = history {
        let hist_cmds = if ctx.line.is_empty() {
            hist.search_fuzzy(b"", max_candidates)
        } else {
            let prefix_matches = hist.search_prefix(&ctx.line, max_candidates);
            if prefix_matches.len() < max_candidates {
                let fuzzy_matches = hist.search_fuzzy(&ctx.line, max_candidates);
                let mut merged = prefix_matches;
                for f in fuzzy_matches {
                    if !merged.contains(&f) {
                        merged.push(f);
                    }
                }
                merged
            } else {
                prefix_matches
            }
        };
        for h in hist_cmds.iter().take(max_candidates) {
            if let Ok(s) = std::str::from_utf8(h) {
                candidates.push(Candidate {
                    value: s.to_string(),
                    display: s.to_string(),
                    description: "history".to_string(),
                    kind: CandidateKind::History,
                    score: 100,
                });
            }
        }
    }

    CompletionOutcome {
        candidates,
        autosuggestion: None,
        word_start: 0,
        word_end: ctx.line.len(),
    }
}

/// Compute bounded filesystem path candidates only.
pub fn complete_paths(
    ctx: &CompletionContext,
    max_candidates: usize,
) -> CompletionOutcome {
    let mut candidates = complete_filesystem(ctx);
    let target_k = max_candidates.min(candidates.len());
    let ranked = if candidates.len() > target_k && target_k > 0 {
        candidates.select_nth_unstable_by(target_k - 1, |a, b| b.score.cmp(&a.score));
        candidates.truncate(target_k);
        candidates.sort_unstable_by_key(|c| std::cmp::Reverse(c.score));
        candidates
    } else {
        candidates.sort_unstable_by_key(|c| std::cmp::Reverse(c.score));
        candidates
    };

    CompletionOutcome {
        candidates: ranked,
        autosuggestion: None,
        word_start: ctx.word_start,
        word_end: ctx.word_end,
    }
}

/// Calculate the ghost text suffix to display after cursor.
fn compute_autosuggestion(
    ctx: &CompletionContext,
    history: Option<&HistoryIndex>,
    candidates: &[Candidate],
) -> Option<String> {
    // 1. Check history for a command starting with the current line
    if let Some(hist) = history {
        if !ctx.line.is_empty() {
            if let Some(full_cmd) = hist.find_most_recent_prefix(&ctx.line) {
                if let Ok(full_str) = std::str::from_utf8(full_cmd) {
                    if full_str.len() > ctx.cursor {
                        return Some(full_str[ctx.cursor..].to_string());
                    }
                }
            }
        }
    }

    // 2. If no full-line history suggestion, check best candidate matching current word
    if let Ok(prefix_str) = std::str::from_utf8(&ctx.prefix) {
        if !prefix_str.is_empty() {
            for c in candidates {
                if c.value.len() > prefix_str.len() && c.value.starts_with(prefix_str) {
                    return Some(c.value[prefix_str.len()..].to_string());
                }
            }
        }
    }

    None
}

/// Split input path into directory path to read and base filename prefix.
fn split_path_prefix(prefix_str: &str, cwd: &Path, home: &Path) -> (PathBuf, String) {
    if prefix_str.is_empty() {
        return (cwd.to_path_buf(), String::new());
    }

    // Expand ~
    let expanded = if let Some(stripped) = prefix_str.strip_prefix("~/") {
        home.join(stripped)
    } else if prefix_str == "~" {
        return (home.to_path_buf(), String::new());
    } else if prefix_str.starts_with('/') {
        PathBuf::from(prefix_str)
    } else {
        cwd.join(prefix_str)
    };

    if prefix_str.ends_with('/') {
        (expanded, String::new())
    } else {
        let parent = expanded.parent().unwrap_or(cwd).to_path_buf();
        let file_prefix = expanded
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        (parent, file_prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_completion_context_command_position() {
        let line = b"git com";
        let ctx = CompletionContext::new(line, 7, PathBuf::from("/tmp"), PathBuf::from("/home"));
        assert_eq!(ctx.word_start, 4);
        assert_eq!(ctx.word_end, 7);
        assert_eq!(ctx.prefix, b"com");
        assert!(!ctx.is_command_position);

        let line2 = b"git";
        let ctx2 = CompletionContext::new(line2, 3, PathBuf::from("/tmp"), PathBuf::from("/home"));
        assert_eq!(ctx2.word_start, 0);
        assert_eq!(ctx2.word_end, 3);
        assert_eq!(ctx2.prefix, b"git");
        assert!(ctx2.is_command_position);
    }

    #[test]
    fn complete_builtins() {
        let line = b"ech";
        let ctx = CompletionContext::new(line, 3, PathBuf::from("/tmp"), PathBuf::from("/home"));
        let commands = complete_commands(&ctx, None);
        assert!(commands.iter().any(|c| c.value == "echo"));
    }

    #[test]
    fn complete_filesystem_local_dir() {
        let cwd = std::env::current_dir().unwrap();
        let home = PathBuf::from("/home");
        let line = b"Cargo";
        let ctx = CompletionContext::new(line, 5, cwd, home);
        let candidates = complete_filesystem(&ctx);
        assert!(candidates.iter().any(|c| c.display == "Cargo.toml"));
    }

    #[test]
    fn complete_bounded_dir_does_not_panic() {
        let cwd = PathBuf::from("/nonexistent-path-abc-123");
        let home = PathBuf::from("/home");
        let line = b"foo";
        let ctx = CompletionContext::new(line, 3, cwd, home);
        let candidates = complete_filesystem(&ctx);
        assert!(candidates.is_empty());
    }

    #[test]
    fn compute_autosuggestion_from_history() {
        let history_data = b"cargo build --release\n";
        let history = HistoryIndex::from_bytes(history_data);
        let cwd = PathBuf::from("/tmp");
        let home = PathBuf::from("/home");

        let line = b"cargo b";
        let ctx = CompletionContext::new(line, 7, cwd, home);
        let outcome = complete(&ctx, Some(&history), 10);
        assert_eq!(outcome.autosuggestion, Some("uild --release".to_string()));
    }

    #[test]
    fn complete_large_directory_under_budget() {
        let store = PathBuf::from("/nix/store");
        if !store.is_dir() {
            return;
        }
        let cwd = PathBuf::from("/tmp");
        let home = PathBuf::from("/home");
        let line = b"ls /nix/store/m3y7";
        let ctx = CompletionContext::new(line, line.len(), cwd, home);
        let start = std::time::Instant::now();
        let candidates = complete_filesystem(&ctx);
        let elapsed = start.elapsed();
        // Must complete in under 100ms in debug mode (typically <10ms in release),
        // never hanging for 1.5s - 3.3s like zsh-autocomplete
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "completion on /nix/store took too long: {:?}",
            elapsed
        );
        assert!(!candidates.is_empty(), "expected match for /nix/store/m3y7");
    }

    #[test]
    fn complete_history_only_returns_history_candidates() {
        let history_data = b"git checkout main\ngit checkout -b feature\ngit status\n";
        let history = HistoryIndex::from_bytes(history_data);
        let cwd = PathBuf::from("/tmp");
        let home = PathBuf::from("/home");
        let line = b"git check";
        let ctx = CompletionContext::new(line, line.len(), cwd, home);
        let outcome = complete_history(&ctx, Some(&history), 5);
        assert!(!outcome.candidates.is_empty());
        for c in &outcome.candidates {
            assert_eq!(c.kind, CandidateKind::History);
            assert!(c.value.starts_with("git check"));
        }
    }

    #[test]
    fn complete_paths_only_returns_path_candidates() {
        let cwd = PathBuf::from("/tmp");
        let home = PathBuf::from("/home");
        let line = b"ls Cargo";
        let ctx = CompletionContext::new(line, line.len(), cwd, home);
        let outcome = complete_paths(&ctx, 10);
        for c in &outcome.candidates {
            assert!(matches!(c.kind, CandidateKind::File | CandidateKind::Directory));
        }
    }
}
