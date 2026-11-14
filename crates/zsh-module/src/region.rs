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
//! Every entry we publish carries `memo=zsh-pulse`. The field exists so
//! that plugins can tell *whose* entries they are looking at
//! (`zsh-src/Doc/Zsh/zle.yo:986-998`), and it costs eleven bytes.

#![forbid(unsafe_code)]

use highlight::{HighlightSpan, StyleId, char_offset};

/// The `memo=` token that identifies our entries.
pub const MEMO: &str = "memo=zsh-pulse";

/// Whether an entry is one of ours.
///
/// The `memo=` field exists so that plugins can tell *whose* entries they
/// are looking at (`zsh-src/Doc/Zsh/zle.yo:986-998`); this is how we find
/// our own previous entries again, so that we replace them instead of
/// accumulating them.
///
/// The match is a substring, exactly as `zsh-syntax-highlighting` does it
/// (`research/upstream/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh:170`).
pub fn is_our_entry(entry: &str) -> bool {
    entry.contains(MEMO)
}

/// Merge our entries with the ones another plugin left behind.
///
/// `existing` is what zsh currently holds; `ours` is what we want to publish.
/// The result is everything that is not ours, followed by ours — ours last,
/// because zsh resolves overlapping regions in favour of the later entry
/// (`zsh-src/Doc/Zsh/zle.yo:1019-1021`).
///
/// Without this the module would replace the whole array on every keystroke
/// and another plugin's highlights would disappear. The reference
/// implementation, `zsh-syntax-highlighting`, does exactly the same thing
/// (`research/upstream/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh:168-175`).
pub fn merge_entries(existing: &[String], ours: &[std::ffi::CString]) -> Vec<std::ffi::CString> {
    let mut merged: Vec<std::ffi::CString> = existing
        .iter()
        .filter(|e| !is_our_entry(e))
        .filter_map(|e| std::ffi::CString::new(e.as_str()).ok())
        .collect();
    merged.extend_from_slice(ours);
    merged
}

/// The `zle_highlight` attribute for each style.
///
/// `None` means "leave this alone": an ordinary argument is shown exactly as
/// the user typed it, which is also what zsh would do without us.
///
/// The defaults aim to match zsh-patina's default theme as closely as the
/// current `StyleId` set allows (hard-coded, no theme/config foundation).
/// Mappings are constrained by the styles the highlighter distinguishes
/// today.
/// TODO: will do finer-grained token kinds.
pub fn attribute(style: StyleId) -> Option<&'static str> {
    match style {
        // callable-known (patina `dynamic.callable`): cyan.
        StyleId::Command => Some("fg=cyan"),
        // callable-missing (patina `dynamic.callable.missing`): red.
        // Bold is kept for visibility; patina specifies colour, not weight.
        StyleId::UnknownCommand => Some("fg=red,bold"),
        // keyword (patina `keyword`): blue.
        StyleId::ReservedWord => Some("fg=blue"),
        // global alias: no dedicated patina key; keep a distinct highlight.
        StyleId::GlobalAlias => Some("bg=blue"),
        // options/flags: no dedicated patina key; keep cyan.
        StyleId::Option => Some("fg=cyan"),
        // dynamic.path (patina): underline. Directory keeps the underline;
        // without a distinct "path type" style it cannot be told apart yet.
        StyleId::Path => Some("underline"),
        StyleId::PathToDir => Some("underline"),
        // storage / variable assignment (patina `storage`): blue.
        StyleId::Assignment => Some("fg=blue"),
        // string (patina `string`): green.
        StyleId::SingleQuoted | StyleId::DoubleQuoted | StyleId::Heredoc => Some("fg=green"),
        // constant.character.escape (patina): yellow.
        StyleId::Escape => Some("fg=yellow"),
        // comment (patina): #a0a0a0. zsh attributes only support the 8
        // named colours, so this is the closest available mapping.
        StyleId::Comment => Some("fg=white"),
        // operators/punctuation: no exact patina key in this set; keep red.
        StyleId::Operator | StyleId::Redirect => Some("fg=red"),
        // command substitution / grouping punctuation: patina resets command
        // substitution parens to terminal defaults; with a single style we
        // leave the construct uncoloured rather than miscolour it.
        StyleId::Substitution => None,
        // variable/parameter expansion (patina `variable.other` /
        // `punctuation.definition.variable`): yellow.
        StyleId::ParamExpansion => Some("fg=yellow"),
        // globs: no dedicated patina key; keep a visible style.
        StyleId::Glob => Some("fg=blue,bold"),
        // bracket pair highlight at cursor: patina `punctuation.section` is
        // yellow; standout keeps it readable against the cursor context.
        StyleId::PairedBracket => Some("fg=yellow,bold"),
        // unmatched bracket: patina marks errors red.
        StyleId::BracketError => Some("fg=red,bold"),
        // A recognized subcommand (adapter-provided): mark as callable/prominent.
        StyleId::Subcommand => Some("fg=cyan"),
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
        self.build_into(spans, offsets)
    }

    /// Write our own entries into the buffer, which must already be empty.
    fn build_into(&mut self, spans: &[HighlightSpan], offsets: Option<&[u32]>) -> bool {
        let mut truncated = false;
        for span in spans {
            let Some(attr) = attribute(span.style) else {
                continue;
            };
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

    /// Build the entries, keeping the ones another plugin published.
    ///
    /// `existing` is what zsh currently holds. Our own previous entries are
    /// recognised by their `memo=` and are replaced; everything else is kept
    /// and ours are appended, because zsh resolves overlapping regions in
    /// favour of the later entry
    /// (`zsh-src/Doc/Zsh/zle.yo:1019-1021`).
    ///
    /// The foreign entries are written into the same buffer as ours, so that
    /// publishing still allocates nothing per region.
    ///
    /// Returns the rows, borrowing this buffer.
    pub fn build_merged(
        &mut self,
        existing: &[String],
        spans: &[HighlightSpan],
        offsets: Option<&[u32]>,
    ) -> (Vec<&std::ffi::CStr>, bool) {
        self.buf.clear();
        self.count = 0;
        for entry in existing {
            if is_our_entry(entry) {
                // Ours from last time: replaced by the new ones below.
                continue;
            }
            // An entry with an interior NUL would truncate the row, so it is
            // dropped rather than published half-formed.
            if entry.as_bytes().contains(&0) {
                continue;
            }
            self.buf.extend_from_slice(entry.as_bytes());
            self.buf.push(0);
            self.count += 1;
        }
        let truncated = self.build_into(spans, offsets);
        (self.rows(), truncated)
    }

    /// The rows, as NUL-terminated C strings.
    ///
    /// The returned slices borrow the buffer, so the buffer must not be
    /// modified while they are alive.
    pub fn rows(&self) -> Vec<&std::ffi::CStr> {
        self.buf
            .split_inclusive(|b| *b == 0)
            .filter(|row| !row.is_empty())
            .filter_map(|row| std::ffi::CStr::from_bytes_with_nul(row).ok())
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
///
/// **Measured**, not guessed. `bench/bench_zpty.zsh --regions N` types a
/// line with N regions and measures the per-keystroke latency; the marginal
/// cost is 0.3–0.9 µs per region depending on the count. At 32 regions the
/// added latency is about 10 µs — the budget this project set for a
/// keystroke. The module's own share is far smaller (about 12 ns per
/// region), but the latency the *user* waits for is zsh's.
pub const MAX_ENTRIES: usize = 32;

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
    use {super::*, highlight::StyleId};

    #[test]
    fn an_argument_is_not_highlighted() {
        assert_eq!(attribute(StyleId::Argument), None);
        assert!(attribute(StyleId::Command).is_some());
    }

    #[test]
    fn styles_match_patina_default_intent_where_representable() {
        // Patina defaults (see /tmp/opencode/zsh-patina/themes/patina.toml):
        // callable-known cyan, callable-missing red, keyword/storage blue,
        // string green, escape yellow, comment #a0a0a0 (approximated),
        // parameter/variable yellow, dynamic.path underline.
        assert_eq!(attribute(StyleId::Command), Some("fg=cyan"));
        assert_eq!(attribute(StyleId::UnknownCommand), Some("fg=red,bold"));
        assert_eq!(attribute(StyleId::ReservedWord), Some("fg=blue"));
        assert_eq!(attribute(StyleId::Assignment), Some("fg=blue"));
        assert_eq!(attribute(StyleId::SingleQuoted), Some("fg=green"));
        assert_eq!(attribute(StyleId::DoubleQuoted), Some("fg=green"));
        assert_eq!(attribute(StyleId::Heredoc), Some("fg=green"));
        assert_eq!(attribute(StyleId::Escape), Some("fg=yellow"));
        assert_eq!(attribute(StyleId::Comment), Some("fg=white"));
        assert_eq!(attribute(StyleId::ParamExpansion), Some("fg=yellow"));
        assert_eq!(attribute(StyleId::Path), Some("underline"));
        assert_eq!(attribute(StyleId::PathToDir), Some("underline"));
        assert_eq!(attribute(StyleId::PairedBracket), Some("fg=yellow,bold"));
        assert_eq!(attribute(StyleId::BracketError), Some("fg=red,bold"));
        // Patina resets command substitution parens to terminal defaults.
        assert_eq!(attribute(StyleId::Substitution), None);
    }

    #[test]
    fn every_attribute_is_one_zsh_can_parse() {
        // zsh's attribute parser (`match_highlight`,
        // `research/upstream/zsh-src/Src/prompt.c:2031`) reads `fg=`, then
        // the colour, then — after a comma — the attributes from the
        // `highlights[]` table (`research/upstream/zsh-src/Src/prompt.c:1896`).
        //
        // The check here is the shape of that grammar: a colour, or a
        // colour and a comma and an attribute. A style that does not fit
        // would be rejected by zsh, and the region would silently do
        // nothing.
        const COLOURS: [&str; 8] = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        // The `highlights[]` table of `research/upstream/zsh-src/Src/prompt.c:1896`.
        const ATTRIBUTES: [&str; 6] = ["reset", "bold", "faint", "standout", "underline", "italic"];
        for style in [
            StyleId::Command,
            StyleId::UnknownCommand,
            StyleId::ReservedWord,
            StyleId::GlobalAlias,
            StyleId::Option,
            StyleId::Path,
            StyleId::PathToDir,
            StyleId::Assignment,
            StyleId::SingleQuoted,
            StyleId::DoubleQuoted,
            StyleId::Escape,
            StyleId::Comment,
            StyleId::Operator,
            StyleId::Redirect,
            StyleId::Substitution,
            StyleId::ParamExpansion,
            StyleId::Glob,
            StyleId::Heredoc,
            StyleId::PairedBracket,
            StyleId::BracketError,
            StyleId::Subcommand,
        ] {
            let Some(attr) = attribute(style) else {
                continue;
            };
            for part in attr.split(',') {
                let is_colour = part
                    .strip_prefix("fg=")
                    .or_else(|| part.strip_prefix("bg="))
                    .is_some_and(|name| COLOURS.contains(&name));
                let is_attribute = ATTRIBUTES.contains(&part);
                assert!(
                    is_colour || is_attribute,
                    "{style:?} has the token {part:?}, which zsh does not know"
                );
            }
        }
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
        assert_eq!(rows[0].to_bytes(), b"5 11 fg=green memo=zsh-pulse");
    }

    #[test]
    fn an_ascii_line_needs_no_offset_map() {
        let spans = [HighlightSpan::new(0, 4, StyleId::Command)];
        let mut entries = Entries::new();
        entries.build(&spans, None);
        assert_eq!(entries.rows()[0].to_bytes(), b"0 4 fg=cyan memo=zsh-pulse");
    }

    #[test]
    fn spans_without_an_attribute_are_skipped() {
        let offsets = highlight::char_offsets(b"echo hi");
        let spans = [
            HighlightSpan::new(0, 4, StyleId::Command),
            HighlightSpan::new(5, 7, StyleId::Argument),
            // Patina resets command substitution parens; unstyled spans are skipped.
            HighlightSpan::new(0, 1, StyleId::Substitution),
        ];
        let mut entries = Entries::new();
        entries.build(&spans, Some(&offsets));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.rows()[0].to_bytes(), b"0 4 fg=cyan memo=zsh-pulse");
    }

    #[test]
    fn the_output_buffer_is_reused() {
        let offsets = highlight::char_offsets(b"echo hi");
        let mut entries = Entries::new();
        entries.build(
            &[HighlightSpan::new(0, 4, StyleId::Command)],
            Some(&offsets),
        );
        assert_eq!(entries.len(), 1);
        entries.build(&[], Some(&offsets));
        assert!(
            entries.is_empty(),
            "the buffer must be cleared, not appended to"
        );
    }

    #[test]
    fn an_empty_region_is_not_emitted() {
        let offsets = highlight::char_offsets(b"echo");
        let mut entries = Entries::new();
        entries.build(
            &[HighlightSpan::new(2, 2, StyleId::Command)],
            Some(&offsets),
        );
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
            assert!(row.to_bytes().ends_with(b"memo=zsh-pulse"));
        }
    }
}
