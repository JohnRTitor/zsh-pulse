//! In-memory index of zsh command history with sub-microsecond queries.
//!
//! # Architecture
//!
//! - **Zero disk scans on the hot path**: The history file is parsed into memory
//!   lazily or in the background; keystrokes query the in-memory index in
//!   microseconds.
//! - **Supports both Zsh formats**:
//!   - Plain format: one command per line (with `\` line continuations).
//!   - Extended format: `: <timestamp>:<duration>;<command>` (multiline supported).
//! - **Fast prefix & fuzzy lookups**:
//!   - Prefix search (`find_most_recent_prefix`): $O(\text{entries})$ scan in reverse,
//!     returning the most recent matching command without allocations.
//!   - Fuzzy search (`search_fuzzy`): Deduplicated candidates scored with `ranking::rank_top_k`.

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A single recorded history entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// Unix timestamp if recorded (extended format), or `None` (plain format).
    pub timestamp: Option<u64>,
    /// The raw command bytes.
    pub command: Vec<u8>,
}

/// An indexed collection of shell history entries.
#[derive(Debug, Default, Clone)]
pub struct HistoryIndex {
    /// Entries in chronological order (oldest to newest).
    entries: Vec<HistoryEntry>,
    /// Unique command strings in reverse chronological order (newest first).
    /// Used for fast fuzzy ranking and deduplicated prefix searching.
    unique_reverse: Vec<Vec<u8>>,
    /// File path where history is loaded from.
    path: Option<PathBuf>,
    /// Last file size observed.
    last_size: u64,
    /// Last file modification time observed.
    last_mtime: Option<SystemTime>,
}

impl HistoryIndex {
    /// Create an empty history index.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            unique_reverse: Vec::new(),
            path: None,
            last_size: 0,
            last_mtime: None,
        }
    }

    /// Number of history entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the history is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Parse history from in-memory bytes.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut index = Self::new();
        index.append_bytes(bytes);
        index.rebuild_unique_reverse();
        index
    }

    /// Load history from a file path.
    pub fn load_from_file(path: impl AsRef<Path>) -> io::Result<Self> {
        let path_buf = path.as_ref().to_path_buf();
        let mut file = File::open(&path_buf)?;
        let metadata = file.metadata()?;
        let size = metadata.len();
        let mtime = metadata.modified().ok();

        let mut bytes = Vec::with_capacity(size.min(16 * 1024 * 1024) as usize);
        file.read_to_end(&mut bytes)?;

        let mut index = Self::from_bytes(&bytes);
        index.path = Some(path_buf);
        index.last_size = size;
        index.last_mtime = mtime;
        Ok(index)
    }

    /// Refresh the index if the history file has changed on disk.
    ///
    /// Returns `true` if new entries were loaded.
    pub fn refresh(&mut self) -> io::Result<bool> {
        let Some(path) = &self.path else {
            return Ok(false);
        };

        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };

        let new_size = metadata.len();
        let new_mtime = metadata.modified().ok();

        if new_size == self.last_size && new_mtime == self.last_mtime {
            return Ok(false);
        }

        // If the file shrank or was replaced, reload from beginning.
        if new_size < self.last_size {
            let mut fresh = Self::load_from_file(path)?;
            std::mem::swap(self, &mut fresh);
            return Ok(true);
        }

        // Otherwise append only the new bytes.
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(self.last_size))?;
        let mut new_bytes = Vec::new();
        file.read_to_end(&mut new_bytes)?;

        if !new_bytes.is_empty() {
            self.append_bytes(&new_bytes);
            self.rebuild_unique_reverse();
        }

        self.last_size = new_size;
        self.last_mtime = new_mtime;
        Ok(true)
    }

    /// Append parsed bytes into `entries`.
    fn append_bytes(&mut self, bytes: &[u8]) {
        let mut i = 0;
        let len = bytes.len();

        while i < len {
            // Find end of line
            let mut line_end = i;
            while line_end < len && bytes[line_end] != b'\n' {
                line_end += 1;
            }

            let mut line = &bytes[i..line_end];
            if line.ends_with(b"\r") {
                line = &line[..line.len() - 1];
            }

            if line.is_empty() {
                i = line_end + 1;
                continue;
            }

            // Check for extended history format: `: <timestamp>:<duration>;<cmd>`
            if line.starts_with(b": ") {
                if let Some((ts, cmd)) = parse_extended_header(line) {
                    let mut cmd_vec = cmd.to_vec();
                    // Handle multi-line commands in extended history:
                    // subsequent lines that do NOT start with `: ` belong to this command.
                    let mut next_start = line_end + 1;
                    while next_start < len {
                        let mut next_end = next_start;
                        while next_end < len && bytes[next_end] != b'\n' {
                            next_end += 1;
                        }
                        let next_line = &bytes[next_start..next_end];
                        if next_line.starts_with(b": ") {
                            break;
                        }
                        cmd_vec.push(b'\n');
                        let trimmed = if next_line.ends_with(b"\r") {
                            &next_line[..next_line.len() - 1]
                        } else {
                            next_line
                        };
                        cmd_vec.extend_from_slice(trimmed);
                        next_start = next_end + 1;
                        line_end = next_end;
                    }

                    clean_and_push_entry(&mut self.entries, Some(ts), cmd_vec);
                    i = line_end + 1;
                    continue;
                }
            }

            // Plain format: check for backslash continuation
            let mut cmd_vec = line.to_vec();
            while cmd_vec.ends_with(b"\\") && line_end + 1 < len {
                cmd_vec.pop(); // remove trailing backslash
                cmd_vec.push(b'\n');
                let next_start = line_end + 1;
                let mut next_end = next_start;
                while next_end < len && bytes[next_end] != b'\n' {
                    next_end += 1;
                }
                let next_line = &bytes[next_start..next_end];
                let trimmed = if next_line.ends_with(b"\r") {
                    &next_line[..next_line.len() - 1]
                } else {
                    next_line
                };
                cmd_vec.extend_from_slice(trimmed);
                line_end = next_end;
            }

            clean_and_push_entry(&mut self.entries, None, cmd_vec);
            i = line_end + 1;
        }
    }

    /// Rebuild deduplicated reverse command cache.
    fn rebuild_unique_reverse(&mut self) {
        let mut seen = HashSet::new();
        self.unique_reverse.clear();
        self.unique_reverse.reserve(self.entries.len().min(4096));

        for entry in self.entries.iter().rev() {
            if seen.insert(entry.command.as_slice()) {
                self.unique_reverse.push(entry.command.clone());
            }
        }
    }

    /// Find the most recent command that starts with `prefix`.
    ///
    /// Skips commands that are exactly equal to `prefix`.
    /// Operates in sub-microsecond time with zero allocations.
    pub fn find_most_recent_prefix(&self, prefix: &[u8]) -> Option<&[u8]> {
        if prefix.is_empty() {
            return None;
        }

        for entry in self.entries.iter().rev() {
            let cmd = entry.command.as_slice();
            if cmd.len() > prefix.len() && cmd.starts_with(prefix) {
                return Some(cmd);
            }
        }
        None
    }

    /// Search for commands matching `prefix`, newest first, deduplicated.
    pub fn search_prefix(&self, prefix: &[u8], limit: usize) -> Vec<&[u8]> {
        if limit == 0 {
            return Vec::new();
        }

        let mut results = Vec::with_capacity(limit);
        for cmd in &self.unique_reverse {
            let slice = cmd.as_slice();
            if prefix.is_empty() || (slice.starts_with(prefix) && slice != prefix) {
                results.push(slice);
                if results.len() == limit {
                    break;
                }
            }
        }
        results
    }

    /// Fuzzy search commands using `ranking::rank_top_k`.
    ///
    /// Returns deduplicated commands ranked by relevance.
    pub fn search_fuzzy(&self, pattern: &[u8], limit: usize) -> Vec<&[u8]> {
        if limit == 0 || self.unique_reverse.is_empty() {
            return Vec::new();
        }

        if pattern.is_empty() {
            return self
                .unique_reverse
                .iter()
                .take(limit)
                .map(|v| v.as_slice())
                .collect();
        }

        let ranked = ranking::rank_top_k(
            self.unique_reverse.len(),
            |idx| self.unique_reverse[idx].as_slice(),
            pattern,
            limit,
        );

        ranked
            .into_iter()
            .map(|sc| self.unique_reverse[sc.index].as_slice())
            .collect()
    }
}

/// Parse `: <timestamp>:<duration>;<command>`
fn parse_extended_header(line: &[u8]) -> Option<(u64, &[u8])> {
    // Must start with `: `
    let rem = line.strip_prefix(b": ")?;
    // Find ':' separating timestamp and duration
    let colon_idx = rem.iter().position(|&b| b == b':')?;
    let ts_str = std::str::from_utf8(&rem[..colon_idx]).ok()?;
    let ts: u64 = ts_str.trim().parse().ok()?;

    // Find ';' separating duration and command
    let semi_idx = rem[colon_idx + 1..].iter().position(|&b| b == b';')? + colon_idx + 1;
    let cmd = &rem[semi_idx + 1..];
    Some((ts, cmd))
}

fn clean_and_push_entry(entries: &mut Vec<HistoryEntry>, timestamp: Option<u64>, mut cmd: Vec<u8>) {
    // Trim trailing whitespace or newlines
    while cmd.last().is_some_and(|&b| b == b'\r' || b == b'\n' || b == b' ' || b == b'\t') {
        cmd.pop();
    }
    if !cmd.is_empty() {
        entries.push(HistoryEntry { timestamp, command: cmd });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_history_lines() {
        let data = b"cargo build\nls -la\ncargo test\n";
        let history = HistoryIndex::from_bytes(data);
        assert_eq!(history.len(), 3);
        assert_eq!(history.find_most_recent_prefix(b"cargo"), Some(&b"cargo test"[..]));
        assert_eq!(history.find_most_recent_prefix(b"cargo test"), None); // equal is skipped
    }

    #[test]
    fn parse_plain_history_with_continuations() {
        let data = b"git commit \\\n-m \"initial commit\"\nls\n";
        let history = HistoryIndex::from_bytes(data);
        assert_eq!(history.len(), 2);
        assert_eq!(history.entries[0].command, b"git commit \n-m \"initial commit\"");
    }

    #[test]
    fn parse_extended_history_lines() {
        let data = b": 1700000000:0;git status\n: 1700000005:2;git checkout -b feature\n";
        let history = HistoryIndex::from_bytes(data);
        assert_eq!(history.len(), 2);
        assert_eq!(history.entries[0].timestamp, Some(1700000000));
        assert_eq!(history.entries[0].command, b"git status");
        assert_eq!(history.entries[1].timestamp, Some(1700000005));
        assert_eq!(history.entries[1].command, b"git checkout -b feature");
    }

    #[test]
    fn parse_extended_history_multiline() {
        let data = b": 1700000010:0;echo 'hello\nworld'\n: 1700000020:0;ls\n";
        let history = HistoryIndex::from_bytes(data);
        assert_eq!(history.len(), 2);
        assert_eq!(history.entries[0].command, b"echo 'hello\nworld'");
        assert_eq!(history.entries[1].command, b"ls");
    }

    #[test]
    fn deduplicated_prefix_search() {
        let data = b"cargo check\ncargo test\ncargo check\ncargo build\n";
        let history = HistoryIndex::from_bytes(data);
        let results = history.search_prefix(b"cargo", 10);
        // Newest unique first: build, check, test
        assert_eq!(results, vec![&b"cargo build"[..], &b"cargo check"[..], &b"cargo test"[..]]);
    }

    #[test]
    fn fuzzy_search_history() {
        let data = b"git clone https://...\ngit commit -m 'fix'\ncargo test\ngit checkout master\n";
        let history = HistoryIndex::from_bytes(data);
        let matches = history.search_fuzzy(b"checkout", 2);
        assert!(!matches.is_empty());
        assert_eq!(matches[0], b"git checkout master");
    }
}
