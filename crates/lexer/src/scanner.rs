//! The scanner: zsh command lines into tokens.
//!
//! # The rules come from zsh, not from intuition
//!
//! zsh's own tokenizer lives in `zsh-src/Src/lex.c`. Two tables there decide
//! everything:
//!
//! - `lx1` (`zsh-src/Src/lex.c:411`) lists the characters that are special
//!   **at the start of a token**: `\ q \n ; ! & | ( ) { } [ ] < >`.
//! - `lx2` (`zsh-src/Src/lex.c:412`) lists the characters that are special
//!   **inside a token**: `; ) | $ [ ] ~ ( { } > < = \ ' " ` , - !`, plus `&`
//!   which is explicitly `LX2_BREAK`.
//!
//! The practical difference matters: `a>b` is the word `a` followed by a
//! redirection, because `>` is in `lx2` and breaks the word; but `a(b` is a
//! single word, because `(` is only special at the start of a token and only
//! in command position (`zsh-src/Src/lex.c:783-800`).
//!
//! # Units and resume points
//!
//! The scanner emits *units*: a word, an operator, a redirection, a comment.
//! A unit may emit several tokens (`a"b"` is two), but it is entered at a
//! single point in the input, and that point is a **resume point**: the state
//! there fully determines the rest of the scan.
//!
//! The state is captured *before* the unit is processed, which is why
//! resuming from a resume point reproduces exactly the tokens a full scan
//! would have produced from that offset.
//!
//! # What this scanner does *not* do
//!
//! It is a *highlighter's* lexer, not zsh's parser. It recognises the
//! constructs that change how a line should be coloured and treats everything
//! else as plain word text. In particular it does not implement history
//! expansion (`!`), `((...))` arithmetic contexts, or `${...}` internals.
//! None of those change *where* a token starts or ends, so none of them
//! affect the highlighter.
//!
//! # Failure is loud
//!
//! Input that the scanner cannot represent exactly — nesting deeper than
//! [`MAX_DEPTH`], a here-document delimiter that does not fit — is reported
//! as a [`LexError`] rather than being silently mis-parsed. The caller is
//! expected to fall back to unstyled text.

use crate::{
    ResumePoint,
    state::{Ctx, Flags, Heredoc, LexState, MAX_DEPTH, MAX_HEREDOC_LEN},
    token::{Token, TokenKind},
};

/// Why a scan could not be completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexError {
    /// The input nests more deeply than [`MAX_DEPTH`].
    TooDeep,
    /// A here-document delimiter is longer than [`MAX_HEREDOC_LEN`].
    ///
    /// The delimiter is stored inline in the lexer state so that two states
    /// can be compared exactly; a truncated delimiter would terminate the
    /// body at the wrong place.
    HeredocDelimiterTooLong,
    /// A second here-document was opened before the first one was consumed.
    ///
    /// Only one body can be pending at a time.
    TooManyHeredocs,
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LexError::TooDeep => write!(f, "nesting deeper than {MAX_DEPTH} levels"),
            LexError::HeredocDelimiterTooLong => write!(
                f,
                "here-document delimiter longer than {MAX_HEREDOC_LEN} bytes"
            ),
            LexError::TooManyHeredocs => write!(f, "more than one here-document pending"),
        }
    }
}

impl std::error::Error for LexError {}

/// Which shell-word context the scanner is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Top level.
    Top,
    /// Inside `$(...)` or `(...)`.
    Paren,
    /// Inside `${...}`.
    Brace,
}

/// Characters that end a word (`lx2`'s `LX2_BREAK` set plus `)`).
///
/// From `zsh-src/Src/lex.c:425` (`lexact2['&'] = LX2_BREAK`) and the
/// `goto brk` arms for `;` `)` `|` `>` `<`.
fn breaks_word(b: u8) -> bool {
    matches!(b, b';' | b')' | b'|' | b'&' | b'>' | b'<')
}

/// Whether `b` ends the run of ordinary characters at the start of a word.
///
/// These are the characters that either end the word or make the scanner
/// emit a token of their own kind (a quote, an escape, an expansion, a
/// glob). The text between two of them is the word's *plain* text: the part
/// that is emitted with the word's own kind.
///
/// `~` is deliberately not listed: it is only special at the very start of a
/// word, and a word that starts with `~` is never the one this is used for.
fn ends_plain_text(b: u8) -> bool {
    breaks_word(b)
        || matches!(
            b,
            b' ' | b'\t' | b'\n' | b'\'' | b'"' | b'`' | b'\\' | b'$' | b'*' | b'?' | b'['
        )
}

/// Whether `c` can start a parameter name.
fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

/// Whether `c` can continue a parameter name.
fn is_name_cont(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The single-character special parameters (`$?`, `$!`, `$$`, …).
fn is_special_param(c: u8) -> bool {
    matches!(
        c,
        b'?' | b'!' | b'#' | b'*' | b'@' | b'-' | b'$' | b'0'..=b'9'
    )
}

/// A callback consulted at the start of every unit.
///
/// It receives the index of the next token, the offset the unit starts at,
/// the guard of that unit (see [`crate::ResumePoint::guard`]) and the parse
/// state there. Returning `true` stops the scan: the tokens produced so far
/// are the answer, and the caller supplies the rest.
///
/// This is how the incremental re-lexer stops at the point where the new
/// parse and the old one agree, instead of scanning the rest of the line
/// only to throw it away.
pub(crate) type Probe<'a> = &'a mut dyn FnMut(usize, usize, usize, &LexState) -> bool;

/// The scanner.
pub(crate) struct Scanner<'a> {
    text: &'a [u8],
    pos: usize,
    state: LexState,
    /// The tokens produced.
    ///
    /// The buffers are borrowed rather than owned so that the caller can
    /// reuse them: a keystroke must not allocate.
    tokens: &'a mut Vec<Token>,
    /// The resume points produced.
    resumes: &'a mut Vec<ResumePoint>,
    /// Consulted at the start of every unit; may stop the scan.
    probe: Option<Probe<'a>>,
    /// Set when the probe asked the scan to stop.
    stopped: bool,
    /// The state at the start of the current unit.
    unit_state: LexState,
    /// The offset of the character that decided where the current unit
    /// starts. See [`ResumePoint::guard`].
    unit_guard: usize,
    /// The guard of the *first* unit of a resumed scan.
    ///
    /// A resumed scan starts in the middle of the line, so the character
    /// that decided where its first unit starts is the one the caller
    /// resumed from — not the position the scan happens to start at.
    initial_guard: usize,
    /// Whether the first unit has been started yet.
    first_unit: bool,
    /// Whether the current unit has already recorded its resume point.
    unit_recorded: bool,
}

impl<'a> Scanner<'a> {
    /// Start scanning `text` from the beginning.
    pub(crate) fn new(
        text: &'a [u8],
        state: LexState,
        tokens: &'a mut Vec<Token>,
        resumes: &'a mut Vec<ResumePoint>,
    ) -> Self {
        Self::new_at(text, 0, state, 0, tokens, resumes)
    }

    /// Start scanning `text` at `pos`, in `state`.
    ///
    /// `guard` is the offset of the character that decided where the scan
    /// resumes; it becomes the guard of the first unit.
    pub(crate) fn new_at(
        text: &'a [u8],
        pos: usize,
        state: LexState,
        guard: usize,
        tokens: &'a mut Vec<Token>,
        resumes: &'a mut Vec<ResumePoint>,
    ) -> Self {
        tokens.clear();
        resumes.clear();
        Self {
            text,
            pos,
            state,
            tokens,
            resumes,
            unit_state: state,
            unit_guard: guard,
            initial_guard: guard,
            first_unit: true,
            unit_recorded: false,
            probe: None,
            stopped: false,
        }
    }

    /// Install a probe that is consulted at the start of every unit.
    ///
    /// The probe is a borrowed trait object rather than a boxed one so that
    /// installing it does not allocate.
    pub(crate) fn with_probe(mut self, probe: Probe<'a>) -> Self {
        self.probe = Some(probe);
        self
    }

    /// Ask the probe whether the scan should stop here.
    fn probe_unit(&mut self) {
        let Some(probe) = self.probe.as_mut() else {
            return;
        };
        // `LexState` is `Copy`, so the state can be read while the probe
        // holds the mutable borrow of the scanner.
        let state = self.state;
        if probe(self.tokens.len(), self.pos, self.unit_guard, &state) {
            self.stopped = true;
        }
    }

    /// Scan to the end of the input.
    pub(crate) fn run(
        mut self,
    ) -> Result<(&'a mut Vec<Token>, &'a mut Vec<ResumePoint>, LexState), LexError> {
        loop {
            match self.state.context() {
                Ctx::Double => {
                    self.scan_double_body()?;
                }
                Ctx::Backtick => {
                    self.scan_backtick_body()?;
                }
                Ctx::Word => {
                    self.scan_shell(Mode::Top)?;
                }
                Ctx::Paren => {
                    self.scan_shell(Mode::Paren)?;
                }
                Ctx::Brace => {
                    self.scan_shell(Mode::Brace)?;
                }
            }
            if self.stopped || self.pos >= self.text.len() {
                break;
            }
        }
        // A here-document that is never terminated has no body; drop it so the
        // final state does not claim a pending body that does not exist.
        self.state.clear_pending_heredoc();
        Ok((self.tokens, self.resumes, self.state))
    }

    // -- helpers ----------------------------------------------------------

    fn peek(&self) -> Option<u8> {
        self.text.get(self.pos).copied()
    }

    fn at(&self, off: usize) -> Option<u8> {
        self.text.get(self.pos + off).copied()
    }

    /// Begin a new unit: a word, an operator, a redirection or a comment.
    ///
    /// `guard` is the offset of the character that decided where this unit
    /// starts. If that character changes, the unit may start somewhere else,
    /// so the resume point is no longer valid.
    fn begin_unit(&mut self, guard: usize) {
        self.unit_state = self.state;
        // The first unit of a resumed scan keeps the guard the caller
        // resumed from; the rest use the character that ended the previous
        // unit.
        self.unit_guard = if self.first_unit {
            self.initial_guard
        } else {
            guard
        };
        self.first_unit = false;
        self.unit_recorded = false;
    }

    /// Emit a token, recording the unit's resume point if this is the unit's
    /// first token.
    fn emit(&mut self, start: usize, end: usize, kind: TokenKind) {
        if end <= start {
            return;
        }
        if !self.unit_recorded {
            self.resumes.push(ResumePoint {
                token_index: self.tokens.len() as u32,
                byte_offset: start as u32,
                state: self.unit_state,
                guard: self.unit_guard as u32,
            });
            self.unit_recorded = true;
        }
        self.tokens.push(Token::new(start as u32, end as u32, kind));
    }

    fn push(&mut self, ctx: Ctx, flags: Flags) -> Result<(), LexError> {
        self.state.push(ctx, flags).map_err(|_| LexError::TooDeep)
    }

    // -- top-level dispatch ----------------------------------------------

    /// Scan a shell-word context until it is exhausted or a nested context is
    /// pushed.
    ///
    /// Returns `true` when a nested context was pushed and the caller must
    /// re-dispatch on the new context.
    fn scan_shell(&mut self, mode: Mode) -> Result<bool, LexError> {
        loop {
            let before = self.pos;
            // The offset before the blanks are skipped is where the previous
            // unit ended: the character there is what decided where this
            // unit starts.
            let guard = self.pos;
            while matches!(self.peek(), Some(b' ' | b'\t')) {
                self.pos += 1;
            }
            let Some(b) = self.peek() else {
                return Ok(false);
            };
            self.begin_unit(guard);
            self.probe_unit();
            if self.stopped {
                return Ok(false);
            }

            // Every branch must report whether it opened a nested context:
            // if it did, the caller has to re-dispatch on the new context
            // instead of carrying on with the old one.
            let pushed = match b {
                b'\n' => {
                    self.handle_newline()?;
                    false
                }
                b'#' => {
                    self.scan_comment();
                    false
                }
                b';' => {
                    self.scan_operator(b";;&");
                    false
                }
                b'&' => {
                    self.scan_ampersand()?;
                    false
                }
                b'|' => {
                    self.scan_operator(b"||&");
                    false
                }
                b'(' => {
                    if mode == Mode::Brace {
                        // A `{` inside `${...}` is nesting, not a subshell.
                        self.scan_word(mode)?
                    } else if mode == Mode::Paren || self.state.expect_command() {
                        let start = self.pos;
                        self.pos += 1;
                        self.emit(start, self.pos, TokenKind::Operator);
                        self.state.set_after_redirect(false);
                        self.push(Ctx::Paren, Flags::command_context())?;
                        self.state.set_expect_command(true);
                        true
                    } else {
                        self.scan_word(mode)?
                    }
                }
                b')' => {
                    if mode == Mode::Brace {
                        self.scan_word(mode)?
                    } else {
                        let start = self.pos;
                        self.pos += 1;
                        self.emit(start, self.pos, TokenKind::Operator);
                        self.state.set_after_redirect(false);
                        if self.state.context() == Ctx::Paren {
                            self.state.pop();
                            // The context changed, so the caller must
                            // re-dispatch: continuing here would scan the
                            // enclosing context with this one's rules.
                            true
                        } else {
                            false
                        }
                    }
                }
                b'}' => {
                    if mode == Mode::Brace {
                        let start = self.pos;
                        self.pos += 1;
                        self.emit(start, self.pos, TokenKind::ParamExpansion);
                        self.state.set_after_redirect(false);
                        self.state.pop();
                        // The context changed: re-dispatch.
                        true
                    } else {
                        self.scan_word(mode)?
                    }
                }
                b'<' | b'>' => self.scan_redirect()?,
                b'0'..=b'9' => {
                    if self.digit_starts_redirect() {
                        self.scan_redirect()?
                    } else {
                        self.scan_word(mode)?
                    }
                }
                _ => self.scan_word(mode)?,
            };

            if pushed {
                return Ok(true);
            }
            // Every step must consume something. A step that does not would
            // spin forever, which is a far worse failure than a wrong token.
            debug_assert!(
                self.pos > before,
                "the scanner made no progress at offset {before} in {mode:?}"
            );
        }
    }

    /// A comment runs to the end of the line.
    ///
    /// `#` only starts a comment at the start of a token
    /// (`zsh-src/Src/lex.c:673-676`), so `a#b` is one word.
    fn scan_comment(&mut self) {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b == b'\n' {
                break;
            }
            self.pos += 1;
        }
        self.emit(start, self.pos, TokenKind::Comment);
        self.state.set_after_redirect(false);
    }

    /// A newline ends a command — unless a here-document body starts here.
    fn handle_newline(&mut self) -> Result<(), LexError> {
        if self.state.pending_heredoc().is_some() {
            self.scan_heredoc_body()?;
            return Ok(());
        }
        let start = self.pos;
        self.pos += 1;
        self.emit(start, self.pos, TokenKind::Operator);
        self.state.set_after_redirect(false);
        self.state.set_expect_command(true);
        self.state.set_after_end_of_options(false);
        Ok(())
    }

    /// An operator made of `c` optionally followed by one of `rest`.
    ///
    /// `rest[0]` is the optional second character, `rest[1]` the optional
    /// third.
    fn scan_operator(&mut self, rest: &[u8; 3]) {
        let start = self.pos;
        self.pos += 1;
        if self.peek() == Some(rest[0]) {
            self.pos += 1;
            if self.peek() == Some(rest[1]) {
                self.pos += 1;
            }
        }
        self.emit(start, self.pos, TokenKind::Operator);
        self.state.set_after_redirect(false);
        self.state.set_expect_command(true);
        self.state.set_after_end_of_options(false);
    }

    /// `&`, `&&`, `&!`, `&|`, or the `&>` family of redirections.
    fn scan_ampersand(&mut self) -> Result<(), LexError> {
        let start = self.pos;
        match self.at(1) {
            Some(b'&') => {
                self.pos += 2;
                self.emit(start, self.pos, TokenKind::Operator);
                self.state.set_after_redirect(false);
                self.state.set_expect_command(true);
                self.state.set_after_end_of_options(false);
            }
            Some(b'!') | Some(b'|') => {
                self.pos += 2;
                self.emit(start, self.pos, TokenKind::Operator);
                self.state.set_after_redirect(false);
                self.state.set_expect_command(true);
                self.state.set_after_end_of_options(false);
            }
            Some(b'>') => {
                // `&>`, `&>>`, `&>!`, `&>|`
                self.pos += 2;
                if self.peek() == Some(b'>') {
                    self.pos += 1;
                }
                if matches!(self.peek(), Some(b'!') | Some(b'|')) {
                    self.pos += 1;
                }
                self.emit(start, self.pos, TokenKind::Redirect);
                self.state.set_after_redirect(true);
            }
            _ => {
                self.pos += 1;
                self.emit(start, self.pos, TokenKind::Operator);
                self.state.set_after_redirect(false);
                self.state.set_expect_command(true);
                self.state.set_after_end_of_options(false);
            }
        }
        Ok(())
    }

    /// Whether the digits at the current position are a file-descriptor prefix.
    ///
    /// zsh accepts `2>file` (`zsh-src/Src/lex.c:654-670`).
    fn digit_starts_redirect(&self) -> bool {
        let mut i = self.pos;
        while self.text.get(i).is_some_and(|c| c.is_ascii_digit()) {
            i += 1;
        }
        matches!(self.text.get(i), Some(b'<' | b'>'))
    }

    /// A redirection operator, optionally with a file-descriptor prefix.
    ///
    /// `<(` and `>(` are process substitutions (command-list contexts producing a
    /// path-like word), tokenized like `$(` so inner commands are lexed/styled.
    fn scan_redirect(&mut self) -> Result<bool, LexError> {
        let start = self.pos;
        // Optional fd prefix.
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'<') => {
                self.pos += 1;
                match self.peek() {
                    Some(b'<') => {
                        // `<<` or `<<-`: a here-document.
                        self.pos += 1;
                        let strip_tabs = self.peek() == Some(b'-');
                        if strip_tabs {
                            self.pos += 1;
                        }
                        self.emit(start, self.pos, TokenKind::Redirect);
                        self.state.set_after_redirect(true);
                        self.scan_heredoc_delimiter(strip_tabs)?;
                        return Ok(false);
                    }
                    Some(b'(') => {
                        // `<( ... )` process substitution (input).
                        self.pos += 1;
                        self.emit(start, self.pos, TokenKind::Substitution);
                        self.push(Ctx::Paren, Flags::command_context())?;
                        self.state.set_after_redirect(false);
                        self.state.set_after_end_of_options(false);
                        self.state.set_expect_command(true);
                        return Ok(true);
                    }
                    Some(b'&') | Some(b'>') => {
                        self.pos += 1;
                    }
                    _ => {}
                }
            }
            Some(b'>') => {
                self.pos += 1;
                match self.peek() {
                    Some(b'(') => {
                        // `>( ... )` process substitution (output).
                        self.pos += 1;
                        self.emit(start, self.pos, TokenKind::Substitution);
                        self.push(Ctx::Paren, Flags::command_context())?;
                        self.state.set_after_redirect(false);
                        self.state.set_after_end_of_options(false);
                        self.state.set_expect_command(true);
                        return Ok(true);
                    }
                    Some(b'>') => {
                        self.pos += 1;
                    }
                    Some(b'&') => {
                        self.pos += 1;
                        if matches!(self.peek(), Some(b'!') | Some(b'|')) {
                            self.pos += 1;
                        }
                    }
                    Some(b'!') | Some(b'|') => {
                        self.pos += 1;
                    }
                    _ => {}
                }
            }
            _ => {
                // Not a redirection after all: treat the digits as a word.
                self.pos = start;
                return self.scan_word(Mode::Top);
            }
        }
        self.emit(start, self.pos, TokenKind::Redirect);
        self.state.set_after_redirect(true);
        Ok(false)
    }

    /// Read the delimiter of a here-document.
    fn scan_heredoc_delimiter(&mut self, strip_tabs: bool) -> Result<(), LexError> {
        // Skip the blanks between `<<` and the delimiter.
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
        let start = self.pos;
        let (delim, kind, quoted) = match self.peek() {
            Some(b'\'') => {
                self.pos += 1;
                let dstart = self.pos;
                while let Some(b) = self.peek() {
                    if b == b'\'' {
                        break;
                    }
                    self.pos += 1;
                }
                let d = self.text[dstart..self.pos].to_vec();
                if self.peek() == Some(b'\'') {
                    self.pos += 1;
                }
                (d, TokenKind::SingleQuoted, true)
            }
            Some(b'"') => {
                self.pos += 1;
                let dstart = self.pos;
                while let Some(b) = self.peek() {
                    if b == b'"' || b == b'\\' {
                        break;
                    }
                    self.pos += 1;
                }
                let d = self.text[dstart..self.pos].to_vec();
                if self.peek() == Some(b'\\') {
                    self.pos += 1;
                    if self.peek().is_some() {
                        self.pos += 1;
                    }
                }
                if self.peek() == Some(b'"') {
                    self.pos += 1;
                }
                (d, TokenKind::DoubleQuoted, true)
            }
            _ => {
                let dstart = self.pos;
                while let Some(b) = self.peek() {
                    if breaks_word(b)
                        || matches!(b, b' ' | b'\t' | b'\n' | b'\'' | b'"' | b'`' | b'\\' | b'$')
                    {
                        break;
                    }
                    self.pos += 1;
                }
                (self.text[dstart..self.pos].to_vec(), TokenKind::Word, false)
            }
        };
        self.emit(start, self.pos, kind);
        if delim.is_empty() {
            return Ok(());
        }
        if delim.len() > MAX_HEREDOC_LEN {
            return Err(LexError::HeredocDelimiterTooLong);
        }
        if self.state.pending_heredoc().is_some() {
            return Err(LexError::TooManyHeredocs);
        }
        self.state.set_pending_heredoc(
            Heredoc::new(&delim, strip_tabs, quoted).ok_or(LexError::HeredocDelimiterTooLong)?,
        );
        Ok(())
    }

    /// Consume a here-document body, up to and including its delimiter line.
    fn scan_heredoc_body(&mut self) -> Result<(), LexError> {
        // Copy the delimiter out of the state so the state can be mutated
        // while it is compared. The buffer is stack-allocated: the delimiter
        // is bounded by `MAX_HEREDOC_LEN`.
        let mut dbuf = [0u8; MAX_HEREDOC_LEN];
        let dlen = {
            let h = self.state.pending_heredoc().expect("checked by caller");
            let d = h.delimiter();
            dbuf[..d.len()].copy_from_slice(d);
            d.len()
        };
        let strip_tabs = self
            .state
            .pending_heredoc()
            .is_some_and(Heredoc::strip_tabs);

        let body_start = self.pos;
        loop {
            // Find the end of the current line.
            let mut line_end = self.pos;
            while line_end < self.text.len() && self.text[line_end] != b'\n' {
                line_end += 1;
            }
            let line = &self.text[self.pos..line_end];
            let mut cmp = line;
            if strip_tabs {
                while cmp.first() == Some(&b'\t') {
                    cmp = &cmp[1..];
                }
            }
            if cmp == &dbuf[..dlen] {
                let end = if line_end < self.text.len() {
                    line_end + 1
                } else {
                    line_end
                };
                self.emit(body_start, end, TokenKind::Heredoc);
                self.pos = end;
                self.state.clear_pending_heredoc();
                self.state.set_after_redirect(false);
                self.state.set_expect_command(true);
                return Ok(());
            }
            if line_end >= self.text.len() {
                // The body runs to the end of the input without a delimiter.
                self.emit(body_start, self.text.len(), TokenKind::Heredoc);
                self.pos = self.text.len();
                self.state.clear_pending_heredoc();
                self.state.set_after_redirect(false);
                self.state.set_expect_command(true);
                return Ok(());
            }
            self.pos = line_end + 1;
        }
    }

    // -- words ------------------------------------------------------------

    /// Scan one word, splitting it into the pieces that carry different
    /// styles.
    ///
    /// Returns `true` when a nested context was pushed.
    fn scan_word(&mut self, mode: Mode) -> Result<bool, LexError> {
        let word_start = self.pos;
        let kind = self.classify_word(word_start, mode);
        let mut plain_start = self.pos;

        while let Some(b) = self.peek() {
            match b {
                b' ' | b'\t' | b'\n' => break,
                b';' | b'|' | b'&' | b'>' | b'<' => break,
                // `)` closes a `$(...)`; inside `${...}` it is an ordinary
                // character.
                b')' if mode != Mode::Brace => break,
                b'}' if mode == Mode::Brace => break,
                b'\'' => {
                    self.flush(plain_start, self.pos, kind);
                    self.scan_single()?;
                    plain_start = self.pos;
                }
                b'"' => {
                    self.flush(plain_start, self.pos, kind);
                    self.push_double_quote()?;
                    return Ok(true);
                }
                b'`' => {
                    self.flush(plain_start, self.pos, kind);
                    self.push_backtick()?;
                    return Ok(true);
                }
                b'\\' => {
                    self.flush(plain_start, self.pos, kind);
                    self.scan_escape();
                    plain_start = self.pos;
                }
                b'$' => {
                    if self.scan_dollar(&mut plain_start, kind)? {
                        return Ok(true);
                    }
                }
                b'*' | b'?' => {
                    self.flush(plain_start, self.pos, kind);
                    self.emit(self.pos, self.pos + 1, TokenKind::Glob);
                    self.pos += 1;
                    plain_start = self.pos;
                }
                b'[' => {
                    self.flush(plain_start, self.pos, kind);
                    self.scan_bracket();
                    plain_start = self.pos;
                }
                b'~' if self.pos == word_start => {
                    // Tilde expansion is only special at the start of a word.
                    self.flush(plain_start, self.pos, kind);
                    self.emit(self.pos, self.pos + 1, TokenKind::Glob);
                    self.pos += 1;
                    plain_start = self.pos;
                }
                _ => self.pos += 1,
            }
        }
        self.flush(plain_start, self.pos, kind);
        // `--` ends option parsing for the rest of this simple command.
        if kind == TokenKind::Word
            && !self.state.expect_command()
            && !self.state.after_redirect()
            && !self.state.after_end_of_options()
            && mode != Mode::Brace
            && self.text[word_start..self.pos] == b"--"[..]
        {
            self.state.set_after_end_of_options(true);
        }
        self.finish_word(kind);
        Ok(false)
    }

    /// Decide what kind of word starts at `start`.
    fn classify_word(&self, start: usize, mode: Mode) -> TokenKind {
        if mode == Mode::Brace {
            // The inside of `${...}` is part of the expansion, not a word.
            return TokenKind::ParamExpansion;
        }
        if self.state.after_redirect() {
            // The target of a redirection names a file, not an option:
            // `cmd > -l` writes to a file called `-l`. This is zsh's own
            // `inredir` (`zsh-src/Src/lex.c:362-367`).
            return TokenKind::Word;
        }
        if !self.state.expect_command() {
            // Argument position: the word may be an option, unless the
            // end-of-options marker `--` has already been seen.
            if self.state.after_end_of_options() {
                return TokenKind::Word;
            }
            return if self.looks_like_option(start) {
                TokenKind::Option
            } else {
                TokenKind::Word
            };
        }
        if self.looks_like_assignment(start) {
            TokenKind::Assignment
        } else {
            TokenKind::Command
        }
    }

    /// Whether the word at `start` is a command-line option.
    ///
    /// An option is a word that starts with `-`, is longer than one
    /// character, and is not exactly `--`:
    ///
    /// - `-l` and `--amend` are options;
    /// - `-` is the word "standard input", not an option;
    /// - `--` is the end-of-options marker, not an option.
    ///
    /// The reference implementation styles every word that starts with a
    /// hyphen as an option
    /// (`research/upstream/fsh/fast-highlight:869-881`); the exceptions
    /// above are the ones that make a difference to the user.
    ///
    /// Only the *plain* text of the word is looked at. A word that starts
    /// with a quote (`"-l"`) is a literal string rather than an option, and
    /// a word whose plain text is only `-` (`-*`) is a glob.
    ///
    /// This is only ever asked of a word in argument position: the command
    /// word is a command whatever it looks like, and the target of a
    /// redirection names a file.
    fn looks_like_option(&self, start: usize) -> bool {
        if self.text.get(start).copied() != Some(b'-') {
            return false;
        }
        let mut end = start + 1;
        while self.text.get(end).is_some_and(|&b| !ends_plain_text(b)) {
            end += 1;
        }
        let plain = &self.text[start..end];
        plain.len() > 1 && plain != b"--"
    }

    /// Whether the word at `start` is a `NAME=value` assignment.
    ///
    /// zsh only treats `=` as an assignment in command position
    /// (`zsh-src/Src/lex.c:1229-1260`).
    fn looks_like_assignment(&self, start: usize) -> bool {
        let mut i = start;
        let Some(first) = self.text.get(i).copied() else {
            return false;
        };
        if !is_name_start(first) {
            return false;
        }
        i += 1;
        while self.text.get(i).is_some_and(|&c| is_name_cont(c)) {
            i += 1;
        }
        match self.text.get(i) {
            Some(b'=') => true,
            Some(b'+') if self.text.get(i + 1) == Some(&b'=') => true,
            _ => false,
        }
    }

    /// Record the effect of a completed word on the parse state.
    fn finish_word(&mut self, kind: TokenKind) {
        self.state.set_after_redirect(false);
        if kind == TokenKind::Command {
            self.state.set_expect_command(false);
        }
        // An assignment does not end the command position: `FOO=bar cmd` runs
        // `cmd`.
    }

    fn flush(&mut self, from: usize, to: usize, kind: TokenKind) {
        if to > from {
            self.emit(from, to, kind);
        }
    }

    // -- quoted sections --------------------------------------------------

    /// A single-quoted string. It ends at the next `'`.
    ///
    /// zsh's `RCQUOTES` option would make `''` a literal quote inside a
    /// single-quoted string, but it is `OPT_EMULATE`
    /// (`zsh-src/Src/options.c:239`) and therefore **unset** in zsh's
    /// default mode: `a''b` is three words' worth of quoting, not one.
    fn scan_single(&mut self) -> Result<bool, LexError> {
        let start = self.pos;
        self.pos += 1;
        while let Some(b) = self.peek() {
            self.pos += 1;
            if b == b'\'' {
                break;
            }
        }
        self.emit(start, self.pos, TokenKind::SingleQuoted);
        self.state.set_after_redirect(false);
        Ok(false)
    }

    /// Open a double-quoted section.
    fn push_double_quote(&mut self) -> Result<(), LexError> {
        let start = self.pos;
        self.pos += 1;
        self.emit(start, self.pos, TokenKind::DoubleQuoted);
        self.push(Ctx::Double, Flags::string_context())?;
        self.state.set_after_redirect(false);
        Ok(())
    }

    /// The body of a double-quoted section.
    fn scan_double_body(&mut self) -> Result<bool, LexError> {
        // The contents of the string are a unit of their own: the resume
        // points inside it must carry the state of the quoted context, not
        // the state of whatever unit opened the quote.
        self.begin_unit(self.pos);
        self.probe_unit();
        if self.stopped {
            return Ok(false);
        }
        let mut plain_start = self.pos;
        loop {
            let Some(b) = self.peek() else {
                // The string is unterminated: the text up to the end of the
                // input is still part of it.
                self.flush(plain_start, self.pos, TokenKind::DoubleQuoted);
                return Ok(false);
            };
            match b {
                b'"' => {
                    self.flush(plain_start, self.pos, TokenKind::DoubleQuoted);
                    let start = self.pos;
                    self.pos += 1;
                    self.emit(start, self.pos, TokenKind::DoubleQuoted);
                    self.state.pop();
                    self.state.set_after_redirect(false);
                    return Ok(false);
                }
                b'\\' => {
                    self.flush(plain_start, self.pos, TokenKind::DoubleQuoted);
                    self.scan_escape();
                    plain_start = self.pos;
                }
                b'$' => {
                    if self.scan_dollar(&mut plain_start, TokenKind::DoubleQuoted)? {
                        return Ok(true);
                    }
                }
                b'`' => {
                    self.flush(plain_start, self.pos, TokenKind::DoubleQuoted);
                    self.push_backtick()?;
                    return Ok(true);
                }
                _ => self.pos += 1,
            }
        }
    }

    /// Open a backtick substitution.
    fn push_backtick(&mut self) -> Result<(), LexError> {
        let start = self.pos;
        self.pos += 1;
        self.emit(start, self.pos, TokenKind::Substitution);
        self.push(Ctx::Backtick, Flags::string_context())?;
        self.state.set_after_redirect(false);
        Ok(())
    }

    /// The body of a backtick substitution.
    fn scan_backtick_body(&mut self) -> Result<bool, LexError> {
        self.begin_unit(self.pos);
        self.probe_unit();
        if self.stopped {
            return Ok(false);
        }
        let mut plain_start = self.pos;
        loop {
            let Some(b) = self.peek() else {
                // The substitution is unterminated.
                self.flush(plain_start, self.pos, TokenKind::Substitution);
                return Ok(false);
            };
            match b {
                b'`' => {
                    self.flush(plain_start, self.pos, TokenKind::Substitution);
                    let start = self.pos;
                    self.pos += 1;
                    self.emit(start, self.pos, TokenKind::Substitution);
                    self.state.pop();
                    self.state.set_after_redirect(false);
                    return Ok(false);
                }
                b'\\' => {
                    self.flush(plain_start, self.pos, TokenKind::Substitution);
                    self.scan_escape();
                    plain_start = self.pos;
                }
                b'$' => {
                    if self.scan_dollar(&mut plain_start, TokenKind::Substitution)? {
                        return Ok(true);
                    }
                }
                _ => self.pos += 1,
            }
        }
    }

    /// A backslash escape: the backslash and the byte it escapes.
    fn scan_escape(&mut self) {
        let start = self.pos;
        self.pos += 1;
        if self.pos < self.text.len() {
            // A backslash-newline is a line continuation, not an escape.
            self.pos += 1;
        }
        self.emit(start, self.pos, TokenKind::Escape);
    }

    /// A glob bracket expression `[...]`.
    fn scan_bracket(&mut self) {
        let start = self.pos;
        self.pos += 1;
        while let Some(b) = self.peek() {
            if b == b']' {
                self.pos += 1;
                break;
            }
            if breaks_word(b) || matches!(b, b' ' | b'\t' | b'\n') {
                break;
            }
            self.pos += 1;
        }
        self.emit(start, self.pos, TokenKind::Glob);
    }

    /// A `$`-introduced expansion.
    ///
    /// Returns `true` when a nested context was pushed, in which case the
    /// caller must return to the main loop.
    fn scan_dollar(&mut self, plain_start: &mut usize, kind: TokenKind) -> Result<bool, LexError> {
        let start = self.pos;
        match self.at(1) {
            Some(b'(') => {
                self.flush(*plain_start, start, kind);
                self.pos += 2;
                self.emit(start, self.pos, TokenKind::Substitution);
                self.push(Ctx::Paren, Flags::command_context())?;
                self.state.set_after_redirect(false);
                self.state.set_expect_command(true);
                *plain_start = self.pos;
                Ok(true)
            }
            Some(b'{') => {
                self.flush(*plain_start, start, kind);
                self.pos += 2;
                self.emit(start, self.pos, TokenKind::ParamExpansion);
                self.push(Ctx::Brace, Flags::string_context())?;
                self.state.set_after_redirect(false);
                self.state.set_expect_command(false);
                *plain_start = self.pos;
                Ok(true)
            }
            Some(c) if is_name_start(c) => {
                self.flush(*plain_start, start, kind);
                let mut n = 2;
                while self.at(n).is_some_and(is_name_cont) {
                    n += 1;
                }
                self.pos += n;
                self.emit(start, self.pos, TokenKind::ParamExpansion);
                *plain_start = self.pos;
                Ok(false)
            }
            Some(c) if is_special_param(c) => {
                self.flush(*plain_start, start, kind);
                self.pos += 2;
                self.emit(start, self.pos, TokenKind::ParamExpansion);
                *plain_start = self.pos;
                Ok(false)
            }
            _ => {
                // A lone `$` is an ordinary character.
                self.pos += 1;
                Ok(false)
            }
        }
    }
}
