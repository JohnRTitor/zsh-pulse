//! Bracket and quote matching: identifies paired brackets and unclosed syntax errors.

use crate::{HighlightSpan, StyleId};

/// A matched pair of bracket/quote offsets (inclusive byte offsets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BracketPair {
    /// Opening bracket/quote byte offset.
    pub open: u32,
    /// Closing bracket/quote byte offset.
    pub close: u32,
}

/// Result of bracket and quote analysis.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BracketAnalysis {
    /// Pairs of matching brackets and quotes.
    pub pairs: Vec<BracketPair>,
    /// Byte offsets of unclosed or mismatched brackets/quotes.
    pub errors: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delimiter {
    Paren(u32),
    Brace(u32),
    Bracket(u32),
    CmdSubst(u32),
    ParamExp(u32),
    DoubleQuote(u32),
    SingleQuote(u32),
    Backtick(u32),
}

/// Analyze brackets and quotes in `buffer`.
pub fn analyze_brackets(buffer: &[u8]) -> BracketAnalysis {
    let mut pairs = Vec::new();
    let mut errors = Vec::new();
    let mut stack: Vec<Delimiter> = Vec::new();

    let mut i = 0usize;
    let mut at_word_start = true;

    while i < buffer.len() {
        let b = buffer[i];
        let pos = i as u32;

        // Context-specific handling for quotes
        match stack.last().copied() {
            Some(Delimiter::SingleQuote(start)) => {
                if b == b'\'' {
                    stack.pop();
                    pairs.push(BracketPair {
                        open: start,
                        close: pos,
                    });
                }
                // In single quotes, every other byte is literal.
                i += 1;
                continue;
            }
            Some(Delimiter::DoubleQuote(start)) => {
                if b == b'\\' {
                    // Backslash escapes in double quotes
                    i += 2;
                    continue;
                }
                if b == b'"' {
                    stack.pop();
                    pairs.push(BracketPair {
                        open: start,
                        close: pos,
                    });
                    i += 1;
                    continue;
                }
                if b == b'`' {
                    stack.push(Delimiter::Backtick(pos));
                    i += 1;
                    continue;
                }
                if b == b'$' && i + 1 < buffer.len() {
                    if buffer[i + 1] == b'(' {
                        stack.push(Delimiter::CmdSubst(pos + 1));
                        i += 2;
                        continue;
                    } else if buffer[i + 1] == b'{' {
                        stack.push(Delimiter::ParamExp(pos + 1));
                        i += 2;
                        continue;
                    }
                }
                // Plain literal characters inside double quotes (including literal brackets)
                i += 1;
                continue;
            }
            Some(Delimiter::Backtick(start)) => {
                if b == b'\\' {
                    i += 2;
                    continue;
                }
                if b == b'`' {
                    stack.pop();
                    pairs.push(BracketPair {
                        open: start,
                        close: pos,
                    });
                    i += 1;
                    continue;
                }
                // Backtick allows shell commands inside, falling through to general parsing
            }
            _ => {}
        }

        // Outside single/double quotes
        if b == b'\\' {
            at_word_start = false;
            i += 2;
            continue;
        }

        // Shell comment: '#' at start of word
        if b == b'#' && at_word_start {
            while i < buffer.len() && buffer[i] != b'\n' {
                i += 1;
            }
            at_word_start = true;
            continue;
        }

        at_word_start = matches!(b, b' ' | b'\t' | b'\n' | b';' | b'|' | b'&');

        // Quotes and substitutions
        if b == b'\'' {
            stack.push(Delimiter::SingleQuote(pos));
            i += 1;
            continue;
        }
        if b == b'"' {
            stack.push(Delimiter::DoubleQuote(pos));
            i += 1;
            continue;
        }
        if b == b'`' {
            stack.push(Delimiter::Backtick(pos));
            i += 1;
            continue;
        }
        if b == b'$' && i + 1 < buffer.len() {
            if buffer[i + 1] == b'(' {
                stack.push(Delimiter::CmdSubst(pos + 1));
                i += 2;
                continue;
            } else if buffer[i + 1] == b'{' {
                stack.push(Delimiter::ParamExp(pos + 1));
                i += 2;
                continue;
            }
        }

        // Brackets
        match b {
            b'(' => {
                stack.push(Delimiter::Paren(pos));
            }
            b'{' => {
                stack.push(Delimiter::Brace(pos));
            }
            b'[' => {
                stack.push(Delimiter::Bracket(pos));
            }
            b')' => {
                match stack.pop() {
                    Some(Delimiter::Paren(open)) | Some(Delimiter::CmdSubst(open)) => {
                        pairs.push(BracketPair { open, close: pos });
                    }
                    Some(other) => {
                        let open = match other {
                            Delimiter::Brace(p)
                            | Delimiter::Bracket(p)
                            | Delimiter::ParamExp(p)
                            | Delimiter::DoubleQuote(p)
                            | Delimiter::SingleQuote(p)
                            | Delimiter::Backtick(p) => p,
                            _ => pos,
                        };
                        errors.push(open);
                        errors.push(pos);
                    }
                    None => {
                        errors.push(pos);
                    }
                }
            }
            b'}' => {
                match stack.pop() {
                    Some(Delimiter::Brace(open)) | Some(Delimiter::ParamExp(open)) => {
                        pairs.push(BracketPair { open, close: pos });
                    }
                    Some(other) => {
                        let open = match other {
                            Delimiter::Paren(p)
                            | Delimiter::Bracket(p)
                            | Delimiter::CmdSubst(p)
                            | Delimiter::DoubleQuote(p)
                            | Delimiter::SingleQuote(p)
                            | Delimiter::Backtick(p) => p,
                            _ => pos,
                        };
                        errors.push(open);
                        errors.push(pos);
                    }
                    None => {
                        errors.push(pos);
                    }
                }
            }
            b']' => {
                match stack.pop() {
                    Some(Delimiter::Bracket(open)) => {
                        pairs.push(BracketPair { open, close: pos });
                    }
                    Some(other) => {
                        let open = match other {
                            Delimiter::Paren(p)
                            | Delimiter::Brace(p)
                            | Delimiter::CmdSubst(p)
                            | Delimiter::ParamExp(p)
                            | Delimiter::DoubleQuote(p)
                            | Delimiter::SingleQuote(p)
                            | Delimiter::Backtick(p) => p,
                            _ => pos,
                        };
                        errors.push(open);
                        errors.push(pos);
                    }
                    None => {
                        errors.push(pos);
                    }
                }
            }
            _ => {}
        }

        i += 1;
    }

    // Any remaining delimiters on stack are unclosed errors
    for delim in stack {
        let open = match delim {
            Delimiter::Paren(p)
            | Delimiter::Brace(p)
            | Delimiter::Bracket(p)
            | Delimiter::CmdSubst(p)
            | Delimiter::ParamExp(p)
            | Delimiter::DoubleQuote(p)
            | Delimiter::SingleQuote(p)
            | Delimiter::Backtick(p) => p,
        };
        errors.push(open);
    }

    BracketAnalysis { pairs, errors }
}

/// Compute highlight spans for paired bracket touching and bracket errors.
pub fn bracket_spans(buffer: &[u8], cursor_byte: Option<usize>) -> Vec<HighlightSpan> {
    let analysis = analyze_brackets(buffer);
    let mut spans = Vec::new();

    // 1. Errors: unclosed or mismatched brackets/quotes
    for err_pos in analysis.errors {
        spans.push(HighlightSpan::new(err_pos, err_pos + 1, StyleId::BracketError));
    }

    // 2. Cursor touch: cursor touching either opening or closing of a pair.
    // If multiple pairs touch (e.g. nested brackets `((...))`), pick the innermost pair (smallest span).
    if let Some(cur_idx) = cursor_byte {
        let cur = cur_idx as u32;
        let mut best_pair: Option<BracketPair> = None;
        let mut best_span_len = u32::MAX;

        for pair in analysis.pairs {
            let touches_open = cur == pair.open || cur == pair.open + 1;
            let touches_close = cur == pair.close || cur == pair.close + 1;

            if touches_open || touches_close {
                let span_len = pair.close.saturating_sub(pair.open);
                if span_len < best_span_len {
                    best_span_len = span_len;
                    best_pair = Some(pair);
                }
            }
        }

        if let Some(pair) = best_pair {
            spans.push(HighlightSpan::new(pair.open, pair.open + 1, StyleId::PairedBracket));
            spans.push(HighlightSpan::new(pair.close, pair.close + 1, StyleId::PairedBracket));
        }
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matched_parentheses_paired_on_cursor_touch() {
        let line = b"echo (hello)";
        // Open is at 5, close is at 11
        let spans_not_touching = bracket_spans(line, Some(0));
        assert!(spans_not_touching.is_empty());

        let spans_touch_open = bracket_spans(line, Some(5));
        assert_eq!(
            spans_touch_open,
            vec![
                HighlightSpan::new(5, 6, StyleId::PairedBracket),
                HighlightSpan::new(11, 12, StyleId::PairedBracket),
            ]
        );

        let spans_touch_close = bracket_spans(line, Some(12));
        assert_eq!(
            spans_touch_close,
            vec![
                HighlightSpan::new(5, 6, StyleId::PairedBracket),
                HighlightSpan::new(11, 12, StyleId::PairedBracket),
            ]
        );
    }

    #[test]
    fn unclosed_bracket_is_error() {
        let line = b"echo (hello";
        let spans = bracket_spans(line, None);
        assert_eq!(
            spans,
            vec![HighlightSpan::new(5, 6, StyleId::BracketError)]
        );
    }

    #[test]
    fn unclosed_quote_is_error() {
        let line = b"echo \"hello";
        let spans = bracket_spans(line, None);
        assert_eq!(
            spans,
            vec![HighlightSpan::new(5, 6, StyleId::BracketError)]
        );
    }

    #[test]
    fn mismatched_bracket_is_error() {
        let line = b"echo (hello]";
        let spans = bracket_spans(line, None);
        assert!(spans.iter().any(|s| s.start == 5 && s.style == StyleId::BracketError));
        assert!(spans.iter().any(|s| s.start == 11 && s.style == StyleId::BracketError));
    }

    #[test]
    fn comments_with_brackets_and_quotes_do_not_error() {
        let line1 = b"echo hello # (this is in a comment";
        let analysis1 = analyze_brackets(line1);
        assert!(analysis1.errors.is_empty());

        let line2 = b"echo hello # don't report quote errors here";
        let analysis2 = analyze_brackets(line2);
        assert!(analysis2.errors.is_empty());
    }

    #[test]
    fn brackets_inside_double_quotes_do_not_interfere() {
        let line = b"echo ( \"foo ) bar\" )";
        let analysis = analyze_brackets(line);
        assert!(analysis.errors.is_empty(), "expected 0 errors, got: {:?}", analysis.errors);
        assert_eq!(analysis.pairs.len(), 2);
        // Outer parens: ( at 5, ) at 19
        assert!(analysis.pairs.contains(&BracketPair { open: 5, close: 19 }));
        // Double quotes: " at 7, " at 17
        assert!(analysis.pairs.contains(&BracketPair { open: 7, close: 17 }));
    }

    #[test]
    fn nested_command_substitution_quotes() {
        let line = b"echo \"$(echo 'hello')\"";
        let analysis = analyze_brackets(line);
        assert!(analysis.errors.is_empty(), "expected 0 errors, got: {:?}", analysis.errors);
        assert_eq!(analysis.pairs.len(), 3);
    }
}
