//! A zsh command-line lexer, in pure Rust.
//!
//! # What this crate is
//!
//! `shell-pulse` highlights the command line as the user types. That needs
//! two things the shell cannot give us: a *token* view of the line (so a
//! quoted string can be coloured differently from a bare word), and the
//! ability to re-lex a line after a single keystroke **without re-reading
//! the whole line**.
//!
//! This crate provides both, and nothing else. It has no dependency on zsh,
//! no `unsafe`, and no I/O, so it can be unit-tested and benchmarked on its
//! own.
//!
//! # The two pieces
//!
//! - [`lex`] scans a whole line into [`Token`]s.
//! - [`LexCache`] keeps the last scan and re-lexes an edited line
//!   incrementally, splicing the unchanged tail back on.
//!
//! # Why the state is exact
//!
//! The incremental splice works by finding a point where the new parse and
//! the old parse agree about *everything* that affects the rest of the line:
//! which quotes are open, how deep the nesting is, whether a command is
//! expected. That agreement is checked with [`LexState::same_parse`], which
//! is an exact comparison, never a similarity score. A heuristic here would
//! produce a line that is highlighted as if it were a different line.
//!
//! # Failure is loud
//!
//! Some inputs cannot be represented exactly — nesting deeper than
//! [`MAX_DEPTH`], a here-document delimiter that does not fit in the state.
//! Those are reported as [`LexError`] rather than mis-parsed, and the caller
//! is expected to fall back to unstyled text.

#![forbid(unsafe_code)]

pub mod state;
pub mod token;

mod incremental;
mod scanner;

#[cfg(test)]
mod proptest;

pub use incremental::{INCREMENTAL_MIN_LEN, LexCache, RelexOutcome};
pub use scanner::LexError;
pub use state::{Ctx, Heredoc, LexState, MAX_DEPTH, MAX_HEREDOC_LEN};
pub use token::{Token, TokenKind};

/// A point at which a scan may be stopped and resumed.
///
/// The state stored here is exactly the state at `byte_offset`, so resuming
/// from a resume point produces the same tokens as a full scan would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumePoint {
    /// Index of the first token of the unit that starts here.
    pub token_index: u32,
    /// Byte offset of the point.
    pub byte_offset: u32,
    /// The parse state at `byte_offset`.
    pub state: LexState,
    /// The offset of the character that decided where this unit starts.
    ///
    /// A unit starts where the previous one stopped, and that is decided by
    /// a single character: the blank after a word, the newline after a
    /// here-document delimiter, the `)` that closed a substitution. If that
    /// character is edited, the unit may start somewhere else entirely, and
    /// this resume point stops being valid.
    ///
    /// The incremental re-lexer uses this to decide which resume points are
    /// still trustworthy after an edit.
    pub guard: u32,
}

/// The result of a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lexed {
    /// The tokens, in order and non-overlapping.
    pub tokens: Vec<Token>,
    /// The points at which the scan may be resumed.
    pub resume_points: Vec<ResumePoint>,
    /// The state at the end of the input.
    pub final_state: LexState,
}

/// Scan a whole command line.
///
/// # Errors
///
/// Returns [`LexError`] for input the scanner cannot represent exactly.
///
/// # Examples
///
/// ```
/// use lexer::{lex, TokenKind};
///
/// let result = lex(b"echo 'hi'").expect("a simple line is always scannable");
/// assert_eq!(result.tokens[0].kind, TokenKind::Command);
/// assert_eq!(result.tokens[1].kind, TokenKind::SingleQuoted);
/// ```
pub fn lex(text: &[u8]) -> Result<Lexed, LexError> {
    let mut tokens = Vec::new();
    let mut resume_points = Vec::new();
    let final_state = {
        let scanner = scanner::Scanner::new(
            text,
            LexState::new(),
            &mut tokens,
            &mut resume_points,
        );
        let (_, _, final_state) = scanner.run()?;
        final_state
    };
    Ok(Lexed { tokens, resume_points, final_state })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::TokenKind;

    fn kinds(text: &str) -> Vec<TokenKind> {
        lex(text.as_bytes())
            .unwrap_or_else(|e| panic!("{text:?} failed to lex: {e}"))
            .tokens
            .iter()
            .map(|t| t.kind)
            .collect()
    }

    fn words(text: &str) -> Vec<String> {
        lex(text.as_bytes())
            .unwrap_or_else(|e| panic!("{text:?} failed to lex: {e}"))
            .tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t.bytes(text.as_bytes()).unwrap()).into_owned())
            .collect()
    }

    /// The byte range every token of `text` covers.
    fn spans(text: &str) -> Vec<(u32, u32)> {
        lex(text.as_bytes())
            .unwrap_or_else(|e| panic!("{text:?} failed to lex: {e}"))
            .tokens
            .iter()
            .map(|t| (t.start, t.end))
            .collect()
    }

    #[test]
    fn an_option_in_argument_position_is_an_option() {
        // An option is a word that starts with a hyphen, is longer than one
        // character and is not exactly `--`. The reference implementation
        // styles every such word as an option
        // (`research/upstream/fsh/fast-highlight:869-881`).
        assert_eq!(
            kinds("cmd -l"),
            vec![TokenKind::Command, TokenKind::Option]
        );
        assert_eq!(words("cmd -l"), vec!["cmd", "-l"]);
    }

    #[test]
    fn a_long_option_in_argument_position_is_an_option() {
        // The long form is not just "a word that starts with a hyphen": it
        // is a word that starts with two of them.
        assert_eq!(
            kinds("cmd --amend"),
            vec![TokenKind::Command, TokenKind::Option]
        );
        assert_eq!(words("cmd --amend"), vec!["cmd", "--amend"]);
    }

    #[test]
    fn a_bundle_of_short_options_is_one_option() {
        // `-abc` is three options to the program that reads it, but it is
        // one word, and the lexer spells words, not options.
        assert_eq!(
            kinds("cmd -abc"),
            vec![TokenKind::Command, TokenKind::Option]
        );
        assert_eq!(words("cmd -abc"), vec!["cmd", "-abc"]);
    }

    #[test]
    fn an_option_with_a_value_is_one_option() {
        // The value belongs to the option: `--amend=1` is one word.
        assert_eq!(
            kinds("cmd --amend=1"),
            vec![TokenKind::Command, TokenKind::Option]
        );
        assert_eq!(words("cmd --amend=1"), vec!["cmd", "--amend=1"]);
    }

    #[test]
    fn a_lone_hyphen_is_the_name_of_standard_input_not_an_option() {
        // `-` is the word "standard input". It is not an option, and
        // painting it like one would be wrong.
        assert_eq!(
            kinds("cmd -"),
            vec![TokenKind::Command, TokenKind::Word]
        );
        assert_eq!(words("cmd -"), vec!["cmd", "-"]);
    }

    #[test]
    fn a_lone_double_hyphen_is_the_end_of_options_marker_not_an_option() {
        // `--` ends the options: everything after it is an operand, not an
        // option. The reference implementation gives it a style of its own;
        // this one does not, because there is no style for "the end of the
        // options" in the palette it copies.
        assert_eq!(
            kinds("cmd --"),
            vec![TokenKind::Command, TokenKind::Word]
        );
        assert_eq!(words("cmd --"), vec!["cmd", "--"]);
    }

    #[test]
    fn a_command_word_is_a_command_even_when_it_looks_like_an_option() {
        // The first word of the line is a command whatever it looks like:
        // the shell runs it, and it is the shell's own tables that say
        // whether it exists.
        assert_eq!(kinds("-l"), vec![TokenKind::Command]);
        assert_eq!(
            kinds("cmd | -l"),
            vec![
                TokenKind::Command,
                TokenKind::Operator,
                TokenKind::Command
            ]
        );
    }

    #[test]
    fn the_target_of_a_redirection_is_a_file_name_not_an_option() {
        // `cmd > -l` writes to a file called `-l`. This is zsh's own rule:
        // the target of a redirection is never an option
        // (`zsh-src/Src/lex.c:362-367`, `inredir`).
        assert_eq!(
            kinds("cmd > -l"),
            vec![TokenKind::Command, TokenKind::Redirect, TokenKind::Word]
        );
        assert_eq!(words("cmd > -l"), vec!["cmd", ">", "-l"]);
    }

    #[test]
    fn a_quoted_option_is_a_quoted_string_not_an_option() {
        // `"-l"` is a string that happens to look like an option. The
        // program that reads it gets the three characters `-`, `"` and `l`.
        assert_eq!(
            kinds("cmd \"-l\""),
            vec![
                TokenKind::Command,
                TokenKind::DoubleQuoted,
                TokenKind::DoubleQuoted,
                TokenKind::DoubleQuoted
            ]
        );
        assert_eq!(
            kinds("cmd '-l'"),
            vec![TokenKind::Command, TokenKind::SingleQuoted]
        );
    }

    #[test]
    fn a_hyphen_before_a_glob_is_a_glob_not_an_option() {
        // `-*` is a glob that matches every file whose name starts with a
        // hyphen. The plain text of the word is only `-`, which is not an
        // option.
        assert_eq!(
            kinds("cmd -*"),
            vec![TokenKind::Command, TokenKind::Word, TokenKind::Glob]
        );
        assert_eq!(words("cmd -*"), vec!["cmd", "-", "*"]);
    }

    #[test]
    fn an_assignment_whose_value_looks_like_an_option_is_an_assignment() {
        // `FOO=-x cmd` sets `FOO` and runs `cmd`. An assignment does not
        // end the command position either: the command is still the command.
        assert_eq!(
            kinds("FOO=-x cmd"),
            vec![TokenKind::Assignment, TokenKind::Command]
        );
    }

    #[test]
    fn an_option_after_the_end_of_options_marker_is_still_an_option() {
        // The lexer is lexical: it does not know that `--` changes the
        // meaning of the words that follow. The shell, not the lexer, is
        // what turns `-- -l` into an operand.
        assert_eq!(
            kinds("cmd -- -l"),
            vec![TokenKind::Command, TokenKind::Word, TokenKind::Option]
        );
    }

    #[test]
    fn an_option_token_covers_only_the_option_word() {
        // The byte range of the token matters: a highlighter that coloured
        // one character too many would paint the space after the option.
        assert_eq!(spans("cmd -l"), vec![(0, 3), (4, 6)]);
        assert_eq!(spans("cmd --amend=1"), vec![(0, 3), (4, 13)]);
        assert_eq!(spans("cmd -*"), vec![(0, 3), (4, 5), (5, 6)]);
    }

    #[test]
    fn an_option_is_part_of_a_word() {
        // The highlighter merges the tokens of a word into one styled run,
        // so an option has to count as part of a word.
        assert!(TokenKind::Option.is_word_part());
        assert!(!TokenKind::Option.is_syntax());
    }

    #[test]
    fn only_the_plain_text_of_a_word_decides_whether_it_is_an_option() {
        // The plain text is the part of the word that is not quoted. A word
        // that starts with a quote is a string, and a word whose plain text
        // is only `-` is a glob.
        assert_eq!(
            kinds("cmd -l\"x\""),
            vec![
                TokenKind::Command,
                TokenKind::Option,
                TokenKind::DoubleQuoted,
                TokenKind::DoubleQuoted,
                TokenKind::DoubleQuoted
            ]
        );
        assert_eq!(words("cmd -l\"x\""), vec!["cmd", "-l", "\"", "x", "\""]);
    }

    #[test]
    fn an_option_inside_a_command_substitution_is_an_option() {
        // The substitution holds a command line of its own, and the option
        // in it is an option like any other.
        assert_eq!(
            kinds("cmd $(ls -l)"),
            vec![
                TokenKind::Command,
                TokenKind::Substitution,
                TokenKind::Command,
                TokenKind::Option,
                TokenKind::Operator
            ]
        );
        // An expansion is not a word at all: the whole of `${-l}` is the
        // expansion, and nothing inside it is an option.
        assert_eq!(
            kinds("cmd ${-l}"),
            vec![
                TokenKind::Command,
                TokenKind::ParamExpansion,
                TokenKind::ParamExpansion,
                TokenKind::ParamExpansion
            ]
        );
        assert_eq!(words("cmd ${-l}"), vec!["cmd", "${", "-l", "}"]);
    }

    #[test]
    fn a_simple_command_is_a_command_and_a_word() {
        assert_eq!(kinds("echo hi"), vec![TokenKind::Command, TokenKind::Word]);
        assert_eq!(words("echo hi"), vec!["echo", "hi"]);
    }

    #[test]
    fn a_pipe_starts_a_new_command() {
        assert_eq!(
            kinds("echo hi | grep h"),
            vec![
                TokenKind::Command,
                TokenKind::Word,
                TokenKind::Operator,
                TokenKind::Command,
                TokenKind::Word
            ]
        );
    }

    #[test]
    fn a_quoted_string_is_its_own_token() {
        assert_eq!(kinds("echo 'hi there'"), vec![TokenKind::Command, TokenKind::SingleQuoted]);
        assert_eq!(words("echo 'hi there'"), vec!["echo", "'hi there'"]);
    }

    #[test]
    fn an_empty_single_quoted_string_ends_the_quoted_run() {
        // zsh's `RCQUOTES` is `OPT_EMULATE` (`zsh-src/Src/options.c:239`),
        // so it is unset in zsh's default mode: `''` is an empty string, not
        // a literal quote.
        assert_eq!(words("a''b"), vec!["a", "''", "b"]);
    }

    #[test]
    fn a_double_quoted_string_is_split_into_its_parts() {
        // The lexer reports the quotes separately so the highlighter can
        // style the text and its delimiters differently; the highlighter
        // merges them back into one run.
        assert_eq!(
            kinds("echo \"hi\""),
            vec![
                TokenKind::Command,
                TokenKind::DoubleQuoted,
                TokenKind::DoubleQuoted,
                TokenKind::DoubleQuoted
            ]
        );
    }

    #[test]
    fn a_comment_starts_at_the_beginning_of_a_word_only() {
        assert_eq!(kinds("echo a#b"), vec![TokenKind::Command, TokenKind::Word]);
        assert_eq!(words("echo a#b"), vec!["echo", "a#b"]);
        assert_eq!(kinds("echo a # b"), vec![TokenKind::Command, TokenKind::Word, TokenKind::Comment]);
    }

    #[test]
    fn a_redirection_breaks_the_word() {
        // zsh: `>` is in `lx2` and ends the word (zsh-src/Src/lex.c:412).
        assert_eq!(kinds("echo hi>f"), vec![TokenKind::Command, TokenKind::Word, TokenKind::Redirect, TokenKind::Word]);
        assert_eq!(kinds("echo 2>f"), vec![TokenKind::Command, TokenKind::Redirect, TokenKind::Word]);
    }

    #[test]
    fn a_redirection_target_is_not_a_command() {
        // `> file cmd` still runs `cmd` (zsh-src/Src/lex.c:360-367).
        assert_eq!(
            kinds("> file cmd"),
            vec![TokenKind::Redirect, TokenKind::Word, TokenKind::Command]
        );
    }

    #[test]
    fn an_assignment_is_only_an_assignment_in_command_position() {
        assert_eq!(kinds("FOO=bar cmd"), vec![TokenKind::Assignment, TokenKind::Command]);
        assert_eq!(kinds("cmd FOO=bar"), vec![TokenKind::Command, TokenKind::Word]);
    }

    #[test]
    fn a_parenthesis_is_only_special_in_command_position() {
        assert_eq!(kinds("(cmd)"), vec![TokenKind::Operator, TokenKind::Command, TokenKind::Operator]);
        assert_eq!(kinds("echo a(b"), vec![TokenKind::Command, TokenKind::Word]);
    }

    #[test]
    fn a_substitution_is_a_substitution() {
        assert_eq!(
            kinds("echo $(cmd)"),
            vec![
                TokenKind::Command,
                TokenKind::Substitution,
                TokenKind::Command,
                TokenKind::Operator
            ]
        );
        // A backtick substitution is a *string* in zsh, not a command list,
        // so its contents are one styled run rather than a nested command.
        assert_eq!(
            kinds("echo `cmd`"),
            vec![
                TokenKind::Command,
                TokenKind::Substitution,
                TokenKind::Substitution,
                TokenKind::Substitution
            ]
        );
    }

    #[test]
    fn a_parameter_expansion_is_a_parameter_expansion() {
        assert_eq!(kinds("echo $HOME"), vec![TokenKind::Command, TokenKind::ParamExpansion]);
        // `${`, its contents and `}` are all part of the expansion.
        assert_eq!(
            kinds("echo ${HOME}"),
            vec![
                TokenKind::Command,
                TokenKind::ParamExpansion,
                TokenKind::ParamExpansion,
                TokenKind::ParamExpansion
            ]
        );
    }

    #[test]
    fn a_glob_metacharacter_is_its_own_token() {
        assert_eq!(kinds("ls *.rs"), vec![TokenKind::Command, TokenKind::Glob, TokenKind::Word]);
    }

    #[test]
    fn a_heredoc_body_is_one_token() {
        let line = "cat <<EOF\nbody\nEOF\n";
        let result = lex(line.as_bytes()).expect("scannable");
        assert!(
            result.tokens.iter().any(|t| t.kind == TokenKind::Heredoc),
            "expected a heredoc body token in {:?}",
            result.tokens
        );
    }

    #[test]
    fn deeply_nested_input_is_rejected_rather_than_mis_parsed() {
        let line = "$(".repeat(MAX_DEPTH + 2);
        assert_eq!(lex(line.as_bytes()), Err(LexError::TooDeep));
    }

    #[test]
    fn an_over_long_heredoc_delimiter_is_rejected() {
        let line = format!("cat <<{}\nbody\n", "x".repeat(MAX_HEREDOC_LEN + 1));
        assert_eq!(lex(line.as_bytes()), Err(LexError::HeredocDelimiterTooLong));
    }

    #[test]
    fn a_tab_stripping_heredoc_ends_at_the_indented_delimiter() {
        // `<<-` strips leading tabs from the body, so the delimiter may be
        // indented. Getting this wrong would end the body on the wrong line
        // and highlight the rest of the file as heredoc.
        let line = "cat <<-EOF\n\tbody\n\tEOF\n";
        let result = lex(line.as_bytes()).expect("scannable");
        let body = result
            .tokens
            .iter()
            .find(|t| t.kind == TokenKind::Heredoc)
            .expect("a heredoc body");
        let text = &line[body.start as usize..body.end as usize];
        // The body token starts at the newline that ended the command.
        assert_eq!(text, "\n\tbody\n\tEOF\n", "the body must end at the delimiter");
    }

    #[test]
    fn a_plain_heredoc_is_not_ended_by_an_indented_delimiter() {
        // Without `<<-` the delimiter must match exactly, so an indented one
        // does not end the body.
        let line = "cat <<EOF\n\tbody\n\tEOF\n";
        let result = lex(line.as_bytes()).expect("scannable");
        let body = result
            .tokens
            .iter()
            .find(|t| t.kind == TokenKind::Heredoc)
            .expect("a heredoc body");
        let text = &line[body.start as usize..body.end as usize];
        assert!(
            text.contains("\tEOF"),
            "an indented delimiter must not end a plain heredoc: {text:?}"
        );
    }

    #[test]
    fn a_quoted_heredoc_delimiter_is_matched_literally() {
        // `<<'EOF'` ends at a line that is exactly `EOF`, whatever the
        // delimiter would otherwise expand to.
        let line = "cat <<'EOF'\nbody\nEOF\n";
        let result = lex(line.as_bytes()).expect("scannable");
        let body = result
            .tokens
            .iter()
            .find(|t| t.kind == TokenKind::Heredoc)
            .expect("a heredoc body");
        let text = &line[body.start as usize..body.end as usize];
        assert_eq!(text, "\nbody\nEOF\n");
    }

    #[test]
    fn the_redirection_operators_are_recognised() {
        // Each of these is a redirection, not part of a word.
        for op in ["&>", "&>>", ">|", "<>", "2>&1", ">&2", "<&0"] {
            let line = format!("cmd {op} file");
            let result = lex(line.as_bytes()).expect("scannable");
            assert!(
                result
                    .tokens
                    .iter()
                    .any(|t| t.kind == TokenKind::Redirect),
                "{op:?} must be a redirection: {:?}",
                result.tokens
            );
        }
    }

    #[test]
    fn tokens_are_ordered_and_do_not_overlap() {
        for line in [
            "echo hi",
            "echo 'hi there' | grep -i 'x'",
            "FOO=bar cmd --flag=value 2>&1",
            "cat <<EOF\nbody\nEOF\n",
            "echo $(cmd `x` \"y\")",
            "echo ${VAR:-default}",
        ] {
            let result = lex(line.as_bytes()).expect("scannable");
            let mut prev_end = 0u32;
            for t in &result.tokens {
                assert!(t.start >= prev_end, "overlap in {line:?}: {t:?}");
                assert!(t.end > t.start, "empty token in {line:?}: {t:?}");
                assert!(t.end as usize <= line.len(), "out of range in {line:?}: {t:?}");
                prev_end = t.end;
            }
        }
    }

    #[test]
    fn every_resume_point_describes_a_real_token() {
        // A resume point's token index and byte offset must agree, or the
        // state it carries belongs to a different token.
        for line in ["echo hi | grep x", "FOO=bar cmd --flag", "cat <<EOF\nbody\nEOF\n"] {
            let result = lex(line.as_bytes()).expect("scannable");
            for rp in &result.resume_points {
                let token = result
                    .tokens
                    .get(rp.token_index as usize)
                    .unwrap_or_else(|| panic!("resume point {rp:?} has no token"));
                assert_eq!(token.start, rp.byte_offset, "resume point {rp:?} vs {token:?}");
            }
        }
    }

    #[test]
    fn every_resume_point_agrees_with_a_full_scan() {
        // Resuming from a resume point must produce the same tokens as
        // scanning the whole line would.
        let line = "echo hi | grep -i 'x' > out";
        let full = lex(line.as_bytes()).expect("scannable");
        for rp in &full.resume_points {
            let mut tokens = Vec::new();
            let mut resumes = Vec::new();
            {
                let scanner = scanner::Scanner::new_at(
                    line.as_bytes(),
                    rp.byte_offset as usize,
                    rp.state,
                    rp.guard as usize,
                    &mut tokens,
                    &mut resumes,
                );
                scanner.run().expect("resumable")
            };
            let expected = &full.tokens[rp.token_index as usize..];
            assert_eq!(
                tokens,
                expected,
                "resuming at {} gave a different token stream",
                rp.byte_offset
            );
        }
    }
}
