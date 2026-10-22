//! Turning styled spans into zsh's `region_highlight` entries.
//!
//! # The format
//!
//! Each element of `region_highlight` is a string of the form
//!
//! ```text
//! <start> <end> <highlight-spec>
//! ```
//!
//! where `start` and `end` are offsets **in the same units as `CURSOR`** —
//! that is, *characters*, not bytes (`zsh-src/Doc/Zsh/zle.yo:980-981`) — and
//! the region covers `[start, end)`. The highlight specification is the same
//! syntax as a `zle_highlight` context (`zsh-src/Src/prompt.c:2031`):
//!
//! | Form | Meaning |
//! |---|---|
//! | `bold`, `standout`, `underline`, `faint`, `italic` | attributes |
//! | `fg=<colour>`, `bg=<colour>` | colours |
//! | `fg=<colour>,bold` | both |
//!
//! The colour names are `black`, `red`, `green`, `yellow`, `blue`,
//! `magenta`, `cyan` and `white` (`zsh-src/Src/prompt.c:1885`).
//!
//! # The `memo=` field
//!
//! Every entry we publish carries `memo=shell-pulse`. The field exists so
//! that plugins can tell *whose* entries they are looking at
//! (`zsh-src/Doc/Zsh/zle.yo:986-998`), and it costs eleven bytes.

use highlight::{char_offset, HighlightSpan, StyleId};

/// The `memo=` token that identifies our entries.
pub const MEMO: &str = "memo=shell-pulse";

/// The `zle_highlight` attribute for each style.
///
/// `None` means "leave this alone": an ordinary argument is shown exactly as
/// the user typed it, which is also what zsh would do without us.
pub fn attribute(style: StyleId) -> Option<&'static str> {
    match style {
        StyleId::Command => Some("fg=cyan,bold"),
        StyleId::Assignment => Some("fg=yellow"),
        StyleId::SingleQuoted => Some("fg=green"),
        StyleId::DoubleQuoted => Some("fg=green"),
        StyleId::Escape => Some("fg=magenta"),
        StyleId::Comment => Some("fg=blue"),
        StyleId::Operator => Some("fg=red"),
        StyleId::Redirect => Some("fg=red"),
        StyleId::Substitution => Some("fg=magenta"),
        StyleId::ParamExpansion => Some("fg=magenta"),
        StyleId::Glob => Some("fg=yellow"),
        StyleId::Heredoc => Some("fg=green"),
        StyleId::Argument => None,
    }
}

/// The reusable buffer the `region_highlight` entries are built in.
///
/// The rows are kept NUL-separated in one byte buffer, so a keystroke does
/// not allocate once per highlighted region — which on the interactive
/// thread is exactly what must not happen.
#[derive(Debug, Default)]
pub struct Entries {
    /// The rows, each NUL-terminated, back to back.
    buf: Vec<u8>,
    /// How many rows the last build produced.
    count: usize,
}

impl Entries {
    /// Replace the contents with the entries for `spans`.
    ///
    /// `offsets` is the byte-to-character map from
    /// [`highlight::char_offsets`]. `None` means the line is pure ASCII, in
    /// which case byte offsets *are* character offsets and the map is not
    /// needed.
    ///
    /// At most [`MAX_ENTRIES`] regions are produced. A line of thousands of
    /// quoted words would otherwise make zsh parse thousands of region
    /// specifications on every redraw, which is far more work than the
    /// highlighting is worth. The limit is reported by the return value, so
    /// the caller can say that the highlighting was truncated.
    pub fn build(&mut self, spans: &[HighlightSpan], offsets: Option<&[u32]>) -> bool {
        self.buf.clear();
        self.count = 0;
        let mut truncated = false;
        for span in spans {
            let Some(attr) = attribute(span.style) else { continue };
            if self.count >= MAX_ENTRIES {
                truncated = true;
                break;
            }
            let start = match offsets {
                Some(map) => char_offset(map, span.start),
                None => span.start,
            };
            let end = match offsets {
                Some(map) => char_offset(map, span.end),
                None => span.end,
            };
            if end <= start {
                // An empty region would be rejected by zsh's parser.
                continue;
            }
            write_number(&mut self.buf, start);
            self.buf.push(b' ');
            write_number(&mut self.buf, end);
            self.buf.push(b' ');
            self.buf.extend_from_slice(attr.as_bytes());
            self.buf.push(b' ');
            self.buf.extend_from_slice(MEMO.as_bytes());
            self.buf.push(0);
            self.count += 1;
        }
        truncated
    }

    /// An empty buffer.
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Empty the buffer, keeping the allocation.
    pub fn clear(&mut self) {
        self.buf.clear();
        self.count = 0;
    }

    /// The rows, as NUL-terminated C strings.
    ///
    /// The returned slices borrow the buffer, so the buffer must not be
    /// modified while they are alive.
    pub fn rows(&self) -> Vec<&std::ffi::CStr> {
        self.buf
            .split_inclusive(|b| *b == 0)
            .filter(|row| !row.is_empty())
            .map(|row| {
                // Safety: every row ends with a NUL and contains no other.
                unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(row) }
            })
            .collect()
    }

    /// How many rows the buffer holds.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// The largest number of regions published in one push.
///
/// zsh parses every region specification on every redraw
/// (`zsh-src/Src/Zle/zle_refresh.c:513-586`), so the number of regions is
/// the real cost — not the length of the line.
pub const MAX_ENTRIES: usize = 256;

/// Write a decimal number into a buffer.
///
/// `format!` would allocate; this does not, and the buffer is reused.
fn write_number(out: &mut Vec<u8>, mut n: u32) {
    let mut digits = [0u8; 10];
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[i..]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use highlight::StyleId;

    #[test]
    fn an_argument_is_not_highlighted() {
        assert_eq!(attribute(StyleId::Argument), None);
        assert!(attribute(StyleId::Command).is_some());
    }

    #[test]
    fn entries_use_character_offsets() {
        let text = "echo 'héllo'".as_bytes();
        let offsets = highlight::char_offsets(text);
        let spans = [HighlightSpan::new(5, 12, StyleId::SingleQuoted)];
        let mut entries = Entries::new();
        entries.build(&spans, Some(&offsets));
        let rows = entries.rows();
        assert_eq!(rows.len(), 1);
        // `'héllo'` starts at character 5 and is six characters long.
        assert_eq!(rows[0].to_bytes(), b"5 11 fg=green memo=shell-pulse");
    }

    #[test]
    fn an_ascii_line_needs_no_offset_map() {
        let spans = [HighlightSpan::new(0, 4, StyleId::Command)];
        let mut entries = Entries::new();
        entries.build(&spans, None);
        assert_eq!(entries.rows()[0].to_bytes(), b"0 4 fg=cyan,bold memo=shell-pulse");
    }

    #[test]
    fn spans_without_an_attribute_are_skipped() {
        let offsets = highlight::char_offsets(b"echo hi");
        let spans = [
            HighlightSpan::new(0, 4, StyleId::Command),
            HighlightSpan::new(5, 7, StyleId::Argument),
        ];
        let mut entries = Entries::new();
        entries.build(&spans, Some(&offsets));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.rows()[0].to_bytes(), b"0 4 fg=cyan,bold memo=shell-pulse");
    }

    #[test]
    fn the_output_buffer_is_reused() {
        let offsets = highlight::char_offsets(b"echo hi");
        let mut entries = Entries::new();
        entries.build(&[HighlightSpan::new(0, 4, StyleId::Command)], Some(&offsets));
        assert_eq!(entries.len(), 1);
        entries.build(&[], Some(&offsets));
        assert!(entries.is_empty(), "the buffer must be cleared, not appended to");
    }

    #[test]
    fn an_empty_region_is_not_emitted() {
        let offsets = highlight::char_offsets(b"echo");
        let mut entries = Entries::new();
        entries.build(&[HighlightSpan::new(2, 2, StyleId::Command)], Some(&offsets));
        assert!(entries.is_empty());
    }

    #[test]
    fn the_number_of_entries_is_capped_and_the_truncation_is_reported() {
        let offsets = highlight::char_offsets(b"echo");
        let spans: Vec<HighlightSpan> = (0..MAX_ENTRIES + 10)
            .map(|_| HighlightSpan::new(0, 4, StyleId::Command))
            .collect();
        let mut entries = Entries::new();
        let truncated = entries.build(&spans, Some(&offsets));
        assert!(truncated, "the truncation must be reported");
        assert_eq!(entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn every_row_is_nul_terminated_and_holds_no_interior_nul() {
        let offsets = highlight::char_offsets(b"echo hi");
        let spans = [
            HighlightSpan::new(0, 4, StyleId::Command),
            HighlightSpan::new(5, 7, StyleId::Glob),
        ];
        let mut entries = Entries::new();
        entries.build(&spans, Some(&offsets));
        for row in entries.rows() {
            // `CStr` guarantees the NUL; what is checked here is that the
            // row holds no *interior* NUL, which would truncate it.
            assert!(!row.to_bytes().contains(&0), "no interior NUL");
            assert!(row.to_bytes().ends_with(b"memo=shell-pulse"));
        }
    }
}
