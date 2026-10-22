//! Styled spans over a command line.
//!
//! # What this crate does
//!
//! The [`lexer`] crate produces *tokens*; this crate turns them into
//! **styled spans**, merges adjacent spans that share a style, and reports
//! which ranges changed between two versions of a line.
//!
//! # Why spans are run-merged
//!
//! A naive conversion of tokens to spans produces one span per token. A
//! double-quoted string is three tokens (the opening quote, the text, the
//! closing quote), so `echo "hello"` would produce four spans where two
//! would do. Merging adjacent spans with the same style keeps the span list
//! proportional to the number of *visual* runs, which is what has to be
//! handed to zsh.
//!
//! # Offsets are bytes; zsh wants characters
//!
//! Spans are byte offsets into the line. zsh's `region_highlight` uses the
//! same units as `CURSOR`, which are **characters**
//! (`zsh-src/Doc/Zsh/zle.yo:980`). [`char_offsets`] converts between the two
//! without allocating.

#![forbid(unsafe_code)]

use lexer::{Token, TokenKind};

/// A style.
///
/// The set is deliberately small and lexical: it says what a piece of the
/// line *is*, never how it should look. Mapping a style onto a terminal
/// attribute is the shell adapter's job.
///
/// Some styles are *not* produced by the lexer, because deciding them needs
/// something the lexer does not have: [`StyleId::Path`] and
/// [`StyleId::PathToDir`] need the filesystem, and [`StyleId::UnknownCommand`]
/// needs the shell's own tables of commands. The caller sets those on the
/// span itself; [`StyleId::of`] only maps the kinds the lexer produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StyleId {
    /// The command name itself.
    Command,
    /// An ordinary argument.
    Argument,
    /// A command-line option: `-l`, `--amend`.
    Option,
    /// A word that names an existing file.
    Path,
    /// A word that names an existing directory.
    PathToDir,
    /// A command the shell does not know.
    UnknownCommand,
    /// A `NAME=value` assignment.
    Assignment,
    /// A single-quoted string.
    SingleQuoted,
    /// A double-quoted string.
    DoubleQuoted,
    /// A backslash escape.
    Escape,
    /// A comment.
    Comment,
    /// A control operator.
    Operator,
    /// A redirection.
    Redirect,
    /// A command substitution.
    Substitution,
    /// A parameter expansion.
    ParamExpansion,
    /// A glob metacharacter.
    Glob,
    /// A here-document body.
    Heredoc,
}

impl StyleId {
    /// The style a token of this kind is given.
    pub fn of(kind: TokenKind) -> Self {
        match kind {
            TokenKind::Command => StyleId::Command,
            TokenKind::Word => StyleId::Argument,
            TokenKind::Option => StyleId::Option,
            TokenKind::Assignment => StyleId::Assignment,
            TokenKind::SingleQuoted => StyleId::SingleQuoted,
            TokenKind::DoubleQuoted => StyleId::DoubleQuoted,
            TokenKind::Escape => StyleId::Escape,
            TokenKind::Comment => StyleId::Comment,
            TokenKind::Operator => StyleId::Operator,
            TokenKind::Redirect => StyleId::Redirect,
            TokenKind::Substitution => StyleId::Substitution,
            TokenKind::ParamExpansion => StyleId::ParamExpansion,
            TokenKind::Glob => StyleId::Glob,
            TokenKind::Heredoc => StyleId::Heredoc,
        }
    }
}

/// A styled byte range of the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighlightSpan {
    /// Inclusive start byte offset.
    pub start: u32,
    /// Exclusive end byte offset.
    pub end: u32,
    /// The style of the range.
    pub style: StyleId,
}

impl HighlightSpan {
    /// Build a span.
    pub const fn new(start: u32, end: u32, style: StyleId) -> Self {
        Self { start, end, style }
    }
}

/// A run-merged set of styled spans.
///
/// Spans are sorted, non-overlapping, and no two adjacent spans share a
/// style.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanSet {
    spans: Vec<HighlightSpan>,
}

impl SpanSet {
    /// An empty set.
    pub fn new() -> Self {
        Self { spans: Vec::new() }
    }

    /// Build a run-merged set from a token stream.
    pub fn from_tokens(tokens: &[Token]) -> Self {
        let mut set = Self::new();
        set.update_from_tokens(tokens);
        set
    }

    /// Replace the contents of this set with the spans of `tokens`.
    ///
    /// The existing allocation is reused, so a steady-state keystroke does
    /// not allocate.
    pub fn update_from_tokens(&mut self, tokens: &[Token]) {
        self.spans.clear();
        for t in tokens {
            if t.is_empty() {
                continue;
            }
            let style = StyleId::of(t.kind);
            match self.spans.last_mut() {
                // Merge with the previous run when it is adjacent and has
                // the same style.
                Some(last) if last.style == style && last.end == t.start => {
                    last.end = t.end;
                }
                _ => self.spans.push(HighlightSpan::new(t.start, t.end, style)),
            }
        }
    }

    /// Empty the set, keeping the allocation.
    pub fn clear(&mut self) {
        self.spans.clear();
    }

    /// The spans, in order.
    pub fn spans(&self) -> &[HighlightSpan] {
        &self.spans
    }

    /// How many spans there are.
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// Whether there are no spans.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// The style covering `offset`, if any.
    pub fn style_at(&self, offset: u32) -> Option<StyleId> {
        // Spans are sorted and non-overlapping, so a binary search finds the
        // only candidate.
        let idx = self.spans.partition_point(|s| s.start <= offset);
        match idx.checked_sub(1) {
            Some(i) if self.spans[i].end > offset => Some(self.spans[i].style),
            _ => None,
        }
    }
}

/// A range whose styling differs between two versions of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangedRange {
    /// Inclusive start byte offset.
    pub start: u32,
    /// Exclusive end byte offset.
    pub end: u32,
    /// The style the range has in the new version.
    pub style: StyleId,
}

/// Compare two span sets and report the ranges whose styling changed.
///
/// The result is the set of ranges that must be re-rendered. It is empty
/// exactly when the two sets agree everywhere, which is the signal the
/// caller uses to skip touching zsh at all.
///
/// # Examples
///
/// ```
/// use highlight::{diff, SpanSet};
/// use lexer::{lex, Token};
///
/// let old = lex(b"echo hi").unwrap();
/// let new = lex(b"echo bye").unwrap();
/// let a = SpanSet::from_tokens(&old.tokens);
/// let b = SpanSet::from_tokens(&new.tokens);
/// assert!(!diff(&a, &b).is_empty());
/// ```
pub fn diff(old: &SpanSet, new: &SpanSet) -> Vec<ChangedRange> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let mut pos = 0u32;
    loop {
        let so = style_at(old.spans(), i, pos);
        let sn = style_at(new.spans(), j, pos);

        // The end of the segment over which both styles are constant: the
        // first of "the current span ends" and "the next span starts",
        // whichever comes first. A span that has not started yet marks the
        // end of a gap.
        let mut end = u32::MAX;
        for spans in [old.spans(), new.spans()] {
            let idx = if std::ptr::eq(spans, old.spans()) { i } else { j };
            if let Some(s) = spans.get(idx) {
                if s.start > pos {
                    end = end.min(s.start);
                }
                end = end.min(s.end);
            }
        }
        if end == u32::MAX || end <= pos {
            break;
        }

        if so != sn {
            out.push(ChangedRange { start: pos, end, style: sn.unwrap_or(StyleId::Argument) });
        }
        pos = end;
        while old.spans().get(i).is_some_and(|s| s.end <= pos) {
            i += 1;
        }
        while new.spans().get(j).is_some_and(|s| s.end <= pos) {
            j += 1;
        }
    }
    out
}

/// The style of the span at index `idx` that covers `offset`.
fn style_at(spans: &[HighlightSpan], idx: usize, offset: u32) -> Option<StyleId> {
    let s = spans.get(idx)?;
    if s.start <= offset && offset < s.end {
        Some(s.style)
    } else {
        None
    }
}

/// Convert byte offsets into character offsets.
///
/// zsh's `region_highlight` uses the same units as `CURSOR`, which count
/// **characters**, not bytes (`zsh-src/Doc/Zsh/zle.yo:980`). A line with
/// non-ASCII text therefore needs its byte offsets converted before they
/// can be handed to zsh.
///
/// Returns one entry per byte offset: `out[i]` is the number of complete
/// characters in `text[..i]`, so a byte in the middle of a multi-byte
/// character maps to the index of that character. The result has
/// `text.len() + 1` entries.
pub fn char_offsets(text: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    char_offsets_into(&mut out, text);
    out
}

/// Compute the byte-to-character map into an existing vector.
///
/// The vector is resized, not reallocated, so a steady-state keystroke does
/// not allocate.
pub fn char_offsets_into(out: &mut Vec<u32>, text: &[u8]) {
    out.clear();
    out.resize(text.len() + 1, 0);
    let mut chars = 0u32;
    let mut i = 0usize;
    while i < text.len() {
        if text[i] & 0b1100_0000 == 0b1000_0000 {
            // A stray continuation byte: treat it as a character of its own
            // rather than mis-attributing it to the previous one.
            out[i] = chars;
            chars += 1;
            out[i + 1] = chars;
            i += 1;
            continue;
        }
        // A character starts here and runs to the next byte that does not
        // continue a UTF-8 sequence.
        let mut end = i + 1;
        while end < text.len() && text[end] & 0b1100_0000 == 0b1000_0000 {
            end += 1;
        }
        for slot in out.iter_mut().take(end).skip(i) {
            *slot = chars;
        }
        chars += 1;
        out[end] = chars;
        i = end;
    }
    out[text.len()] = chars;
}

/// The character offset of a byte offset.
pub fn char_offset(offsets: &[u32], byte: u32) -> u32 {
    offsets.get(byte as usize).copied().unwrap_or_else(|| {
        offsets.last().copied().unwrap_or(0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lexer::lex;

    fn spans_of(line: &str) -> Vec<(u32, u32, StyleId)> {
        let l = lex(line.as_bytes()).expect("scannable");
        SpanSet::from_tokens(&l.tokens)
            .spans()
            .iter()
            .map(|s| (s.start, s.end, s.style))
            .collect()
    }

    #[test]
    fn a_command_and_an_argument_get_different_styles() {
        assert_eq!(
            spans_of("echo hi"),
            vec![(0, 4, StyleId::Command), (5, 7, StyleId::Argument)]
        );
    }

    #[test]
    fn an_option_gets_its_own_style() {
        // The option is not an ordinary argument: the reference
        // implementation colours it in its own colour
        // (`research/upstream/fsh/fast-highlight:76-77`).
        assert_eq!(
            spans_of("ls -la"),
            vec![(0, 2, StyleId::Command), (3, 6, StyleId::Option)]
        );
        assert_eq!(StyleId::of(TokenKind::Option), StyleId::Option);
    }

    #[test]
    fn the_styles_the_lexer_cannot_decide_are_left_to_the_caller() {
        // Whether a word names an existing file needs the filesystem, and
        // whether the shell knows a command needs the shell's own tables, so
        // the lexer styles both as ordinary text and the caller replaces the
        // style on the span.
        assert_eq!(StyleId::of(TokenKind::Word), StyleId::Argument);
        assert_eq!(StyleId::of(TokenKind::Command), StyleId::Command);
        assert_eq!(
            spans_of("cat Cargo.toml"),
            vec![(0, 3, StyleId::Command), (4, 14, StyleId::Argument)]
        );
    }

    #[test]
    fn adjacent_spans_with_the_same_style_are_merged() {
        // `"hi"` is three tokens but one visual run.
        assert_eq!(
            spans_of("echo \"hi\""),
            vec![(0, 4, StyleId::Command), (5, 9, StyleId::DoubleQuoted)]
        );
    }

    #[test]
    fn spans_with_a_gap_between_them_are_not_merged() {
        assert_eq!(spans_of("a b").len(), 2);
    }

    #[test]
    fn the_span_is_small_enough_to_be_cheap() {
        // The plan asks for a `size_of` ceiling on the hot path: the spans
        // are rebuilt on every keystroke, so their size is part of the cost.
        assert!(
            core::mem::size_of::<HighlightSpan>() <= 16,
            "HighlightSpan grew to {} bytes",
            core::mem::size_of::<HighlightSpan>()
        );
        assert!(
            core::mem::size_of::<StyleId>() <= 4,
            "StyleId grew to {} bytes",
            core::mem::size_of::<StyleId>()
        );
    }

    #[test]
    fn the_span_set_is_never_larger_than_the_token_count() {
        let l = lex(b"echo \"hi\" 'there' $x").expect("scannable");
        let s = SpanSet::from_tokens(&l.tokens);
        assert!(s.len() <= l.tokens.len());
    }

    #[test]
    fn an_unchanged_line_has_no_changes() {
        let l = lex(b"echo hi").expect("scannable");
        let a = SpanSet::from_tokens(&l.tokens);
        let b = SpanSet::from_tokens(&l.tokens);
        assert!(diff(&a, &b).is_empty());
    }

    #[test]
    fn a_changed_argument_is_reported() {
        let old = lex(b"echo hi").expect("scannable");
        let new = lex(b"echo bye").expect("scannable");
        let a = SpanSet::from_tokens(&old.tokens);
        let b = SpanSet::from_tokens(&new.tokens);
        let changes = diff(&a, &b);
        assert!(!changes.is_empty());
        // The command did not change, so it must not be in the report.
        assert!(changes.iter().all(|c| c.start >= 5));
    }

    #[test]
    fn a_style_change_is_reported() {
        let old = lex(b"echo hi").expect("scannable");
        let new = lex(b"'echo' hi").expect("scannable");
        let a = SpanSet::from_tokens(&old.tokens);
        let b = SpanSet::from_tokens(&new.tokens);
        let changes = diff(&a, &b);
        assert!(!changes.is_empty());
        assert_eq!(changes[0].style, StyleId::SingleQuoted);
    }

    #[test]
    fn diff_reports_nothing_when_only_the_length_changed_but_not_the_style() {
        let old = lex(b"echo hi").expect("scannable");
        let new = lex(b"echo hi there").expect("scannable");
        let a = SpanSet::from_tokens(&old.tokens);
        let b = SpanSet::from_tokens(&new.tokens);
        let changes = diff(&a, &b);
        // `hi` is still an argument at the same offsets, so the only change
        // is the newly added argument.
        assert!(changes.iter().all(|c| c.start >= 7), "{changes:?}");
    }

    #[test]
    fn char_offsets_count_characters_not_bytes() {
        let text = "héllo".as_bytes();
        let offsets = char_offsets(text);
        assert_eq!(offsets[0], 0);
        assert_eq!(offsets[1], 1, "'h' is one character");
        assert_eq!(offsets[2], 1, "the first byte of 'é' does not start a new character");
        assert_eq!(offsets[3], 2, "'é' is one character");
        assert_eq!(offsets[text.len()], 5);
    }

    #[test]
    fn char_offsets_handle_ascii() {
        let offsets = char_offsets(b"abc");
        assert_eq!(offsets, vec![0, 1, 2, 3]);
    }

    #[test]
    fn char_offsets_survive_invalid_utf8() {
        // Invalid UTF-8 must not produce a map that is out of order or the
        // wrong length: the map is what converts the lexer's byte offsets
        // into zsh's character offsets, and a wrong map silently misplaces
        // every highlight after the bad byte.
        for text in [b"a\x80b".as_slice(), b"\x80abc".as_slice(), b"\xff\xfe".as_slice()] {
            let offsets = char_offsets(text);
            assert_eq!(offsets.len(), text.len() + 1, "for {text:?}");
            for pair in offsets.windows(2) {
                assert!(
                    pair[0] <= pair[1],
                    "the map must not go backwards: {offsets:?} for {text:?}"
                );
            }
            assert!(
                *offsets.last().unwrap() <= text.len() as u32,
                "the character count cannot exceed the byte count: {offsets:?}"
            );
        }
        // A stray continuation byte at the start of the text is counted as a
        // character of its own.
        assert_eq!(char_offsets(b"\x80b"), vec![0, 1, 2]);
    }

    #[test]
    fn char_offsets_of_an_empty_line() {
        assert_eq!(char_offsets(b""), vec![0]);
    }

    /// A deliberately naive diff: look at every byte offset.
    ///
    /// This is the specification the fast path has to agree with.
    fn reference_diff(old: &SpanSet, new: &SpanSet) -> Vec<(u32, u32)> {
        let end = old
            .spans()
            .last()
            .map(|s| s.end)
            .into_iter()
            .chain(new.spans().last().map(|s| s.end))
            .max()
            .unwrap_or(0);
        let mut out = Vec::new();
        let mut start = 0u32;
        let mut current: Option<(Option<StyleId>, Option<StyleId>)> = None;
        for offset in 0..end {
            let pair = (old.style_at(offset), new.style_at(offset));
            if current != Some(pair) {
                if let Some((o, n)) = current {
                    if o != n {
                        out.push((start, offset));
                    }
                }
                current = Some(pair);
                start = offset;
            }
        }
        if let Some((o, n)) = current {
            if o != n {
                out.push((start, end));
            }
        }
        out
    }

    #[test]
    fn the_diff_agrees_with_a_naive_reference_implementation() {
        let lines = [
            "echo hi",
            "echo 'hi there' | grep -i x",
            "FOO=bar cmd --flag=value 2>&1",
            "cat <<EOF\nbody\nEOF\n",
            "echo $(cmd `x` \"y\")",
        ];
        for a in lines {
            for b in lines {
                let sa = SpanSet::from_tokens(&lex(a.as_bytes()).unwrap().tokens);
                let sb = SpanSet::from_tokens(&lex(b.as_bytes()).unwrap().tokens);
                let fast: Vec<(u32, u32)> =
                    diff(&sa, &sb).iter().map(|c| (c.start, c.end)).collect();
                let slow = reference_diff(&sa, &sb);
                assert_eq!(fast, slow, "for {a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn the_diff_covers_exactly_the_offsets_whose_style_changed() {
        let a = SpanSet::from_tokens(&lex(b"echo hi").unwrap().tokens);
        let b = SpanSet::from_tokens(&lex(b"echo bye").unwrap().tokens);
        let changes = diff(&a, &b);
        // Every reported range must really differ, and every offset that
        // really differs must be reported.
        for offset in 0..8u32 {
            let differs = a.style_at(offset) != b.style_at(offset);
            let reported = changes.iter().any(|c| c.start <= offset && offset < c.end);
            assert_eq!(differs, reported, "offset {offset}");
        }
    }

    #[test]
    fn style_at_finds_the_covering_span() {
        let l = lex(b"echo hi").expect("scannable");
        let s = SpanSet::from_tokens(&l.tokens);
        assert_eq!(s.style_at(0), Some(StyleId::Command));
        assert_eq!(s.style_at(4), None, "the space is not styled");
        assert_eq!(s.style_at(6), Some(StyleId::Argument));
        assert_eq!(s.style_at(100), None);
    }
}
