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

/// Analyze brackets and quotes in `buffer`.
pub fn analyze_brackets(buffer: &[u8]) -> BracketAnalysis {
    let mut pairs = Vec::new();
    let mut errors = Vec::new();

    // Stack holds (bracket_char, byte_offset)
    let mut bracket_stack: Vec<(u8, u32)> = Vec::new();

    let mut in_single_quote = false;
    let mut single_quote_start = 0u32;

    let mut in_double_quote = false;
    let mut double_quote_start = 0u32;

    let mut in_backtick = false;
    let mut backtick_start = 0u32;

    let mut escaped = false;
    let mut i = 0usize;

    while i < buffer.len() {
        let b = buffer[i];
        let pos = i as u32;

        if escaped {
            escaped = false;
            i += 1;
            continue;
        }

        if b == b'\\' && !in_single_quote {
            escaped = true;
            i += 1;
            continue;
        }

        // Single quote handling
        if b == b'\'' && !in_double_quote && !in_backtick {
            if in_single_quote {
                pairs.push(BracketPair {
                    open: single_quote_start,
                    close: pos,
                });
                in_single_quote = false;
            } else {
                in_single_quote = true;
                single_quote_start = pos;
            }
            i += 1;
            continue;
        }

        // Inside single quotes, all other characters are literal text.
        if in_single_quote {
            i += 1;
            continue;
        }

        // Double quote handling
        if b == b'"' && !in_backtick {
            if in_double_quote {
                pairs.push(BracketPair {
                    open: double_quote_start,
                    close: pos,
                });
                in_double_quote = false;
            } else {
                in_double_quote = true;
                double_quote_start = pos;
            }
            i += 1;
            continue;
        }

        // Backtick handling
        if b == b'`' {
            if in_backtick {
                pairs.push(BracketPair {
                    open: backtick_start,
                    close: pos,
                });
                in_backtick = false;
            } else {
                in_backtick = true;
                backtick_start = pos;
            }
            i += 1;
            continue;
        }

        // Inside double quotes, handle nested command substitutions $(...)
        if in_double_quote {
            if b == b'$' && i + 1 < buffer.len() && buffer[i + 1] == b'(' {
                bracket_stack.push((b'(', pos + 1));
                i += 2;
                continue;
            }
            if b == b')' && !bracket_stack.is_empty() {
                if let Some((top_b, top_pos)) = bracket_stack.last().copied() {
                    if top_b == b'(' {
                        bracket_stack.pop();
                        pairs.push(BracketPair {
                            open: top_pos,
                            close: pos,
                        });
                        i += 1;
                        continue;
                    }
                }
            }
            i += 1;
            continue;
        }

        // Check opening and closing brackets
        match b {
            b'(' | b'{' | b'[' => {
                bracket_stack.push((b, pos));
            }
            b')' => {
                match bracket_stack.pop() {
                    Some((b'(', open_pos)) => {
                        pairs.push(BracketPair {
                            open: open_pos,
                            close: pos,
                        });
                    }
                    Some((_, unclosed_pos)) => {
                        errors.push(unclosed_pos);
                        errors.push(pos);
                    }
                    None => {
                        errors.push(pos);
                    }
                }
            }
            b'}' => {
                match bracket_stack.pop() {
                    Some((b'{', open_pos)) => {
                        pairs.push(BracketPair {
                            open: open_pos,
                            close: pos,
                        });
                    }
                    Some((_, unclosed_pos)) => {
                        errors.push(unclosed_pos);
                        errors.push(pos);
                    }
                    None => {
                        errors.push(pos);
                    }
                }
            }
            b']' => {
                match bracket_stack.pop() {
                    Some((b'[', open_pos)) => {
                        pairs.push(BracketPair {
                            open: open_pos,
                            close: pos,
                        });
                    }
                    Some((_, unclosed_pos)) => {
                        errors.push(unclosed_pos);
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

    // Any remaining unclosed quotes or brackets are syntax errors
    if in_single_quote {
        errors.push(single_quote_start);
    }
    if in_double_quote {
        errors.push(double_quote_start);
    }
    if in_backtick {
        errors.push(backtick_start);
    }
    for (_b, pos) in bracket_stack {
        errors.push(pos);
    }

    BracketAnalysis { pairs, errors }
}

/// Compute highlight spans for paired bracket touching and bracket errors.
pub fn bracket_spans(buffer: &[u8], cursor_byte: usize) -> Vec<HighlightSpan> {
    let analysis = analyze_brackets(buffer);
    let mut spans = Vec::new();

    // 1. Errors: unclosed or mismatched brackets/quotes
    for err_pos in analysis.errors {
        spans.push(HighlightSpan::new(err_pos, err_pos + 1, StyleId::BracketError));
    }

    // 2. Cursor touch: cursor touching either opening or closing of a pair
    let cur = cursor_byte as u32;
    for pair in analysis.pairs {
        let touches_open = cur == pair.open || cur == pair.open + 1;
        let touches_close = cur == pair.close || cur == pair.close + 1;

        if touches_open || touches_close {
            spans.push(HighlightSpan::new(pair.open, pair.open + 1, StyleId::PairedBracket));
            spans.push(HighlightSpan::new(pair.close, pair.close + 1, StyleId::PairedBracket));
            break;
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
        let spans_not_touching = bracket_spans(line, 0);
        assert!(spans_not_touching.is_empty());

        let spans_touch_open = bracket_spans(line, 5);
        assert_eq!(
            spans_touch_open,
            vec![
                HighlightSpan::new(5, 6, StyleId::PairedBracket),
                HighlightSpan::new(11, 12, StyleId::PairedBracket),
            ]
        );

        let spans_touch_close = bracket_spans(line, 12);
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
        let spans = bracket_spans(line, 0);
        assert_eq!(
            spans,
            vec![HighlightSpan::new(5, 6, StyleId::BracketError)]
        );
    }

    #[test]
    fn unclosed_quote_is_error() {
        let line = b"echo \"hello";
        let spans = bracket_spans(line, 0);
        assert_eq!(
            spans,
            vec![HighlightSpan::new(5, 6, StyleId::BracketError)]
        );
    }

    #[test]
    fn mismatched_bracket_is_error() {
        let line = b"echo (hello]";
        let spans = bracket_spans(line, 0);
        assert!(spans.iter().any(|s| s.start == 5 && s.style == StyleId::BracketError));
        assert!(spans.iter().any(|s| s.start == 11 && s.style == StyleId::BracketError));
    }
}
