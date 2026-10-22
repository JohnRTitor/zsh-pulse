//! Lexical classes produced by the [`crate::Scanner`].
//!
//! The kinds are deliberately *syntactic*, not *semantic*: they say what the
//! bytes are, never what they mean. Mapping a kind onto a colour is the
//! [`highlight`](../../highlight/index.html) crate's job, which keeps this
//! crate free of any notion of style.

/// The lexical class of a token.
///
/// A token covers a byte range of the input. Adjacent tokens never overlap and
/// never leave a gap inside a word: the scanner splits a word at every
/// boundary that changes its class, so `a"b"c` is three tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TokenKind {
    /// A bare word, or the tail of one.
    Word,
    /// The command name: the first word of a command.
    ///
    /// Distinguished from [`TokenKind::Word`] because a highlighter usually
    /// wants the command to stand out from its arguments.
    Command,
    /// `name=value` in command position.
    Assignment,
    /// A single-quoted string, including its quotes.
    SingleQuoted,
    /// A double-quoted string. The quotes themselves are separate tokens of
    /// this kind, so the quoted text and the delimiters can be styled
    /// differently if a caller wants to.
    DoubleQuoted,
    /// A backslash escape, including the backslash.
    Escape,
    /// A comment, from `#` to the end of the line (not including the newline).
    Comment,
    /// A control operator: `|`, `||`, `&&`, `;`, `;;`, `&`, `|&`, `(`,
    /// `)`, or a newline acting as a command separator.
    Operator,
    /// A redirection operator: `<`, `>`, `>>`, `<<`, `<<-`, `>&`, `<&`,
    /// `&>`, `&>>`, `>|`, `<>`.
    Redirect,
    /// A command substitution: the `$(` opener, a backtick opener or closer.
    Substitution,
    /// A parameter expansion: the `${` opener, or a `$name` reference.
    ParamExpansion,
    /// A glob metacharacter: `*`, `?`, or a `[...]` bracket expression.
    Glob,
    /// The body of a here-document.
    Heredoc,
}

impl TokenKind {
    /// Whether this kind is part of a *word* — that is, whether it can appear
    /// between two [`TokenKind::Word`] tokens as one shell word.
    ///
    /// Used by the highlighter to decide which tokens may be merged into a
    /// single styled run.
    pub fn is_word_part(self) -> bool {
        matches!(
            self,
            TokenKind::Word
                | TokenKind::Command
                | TokenKind::Assignment
                | TokenKind::SingleQuoted
                | TokenKind::DoubleQuoted
                | TokenKind::Escape
                | TokenKind::Substitution
                | TokenKind::ParamExpansion
                | TokenKind::Glob
        )
    }

    /// Whether this kind represents shell *syntax* rather than text.
    pub fn is_syntax(self) -> bool {
        matches!(
            self,
            TokenKind::Comment | TokenKind::Operator | TokenKind::Redirect
        )
    }
}

/// A lexical token: a byte range and its class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    /// Inclusive start byte offset.
    pub start: u32,
    /// Exclusive end byte offset.
    pub end: u32,
    /// The class of the bytes in `start..end`.
    pub kind: TokenKind,
}

impl Token {
    /// Build a token.
    pub const fn new(start: u32, end: u32, kind: TokenKind) -> Self {
        Self { start, end, kind }
    }

    /// Length of the token in bytes.
    pub const fn len(&self) -> u32 {
        self.end - self.start
    }

    /// Whether the token covers no bytes.
    pub const fn is_empty(&self) -> bool {
        self.end == self.start
    }

    /// The token's bytes within `text`.
    ///
    /// Returns `None` if the range is out of bounds, which can only happen if
    /// a token was built against a different buffer.
    pub fn bytes<'a>(&self, text: &'a [u8]) -> Option<&'a [u8]> {
        text.get(self.start as usize..self.end as usize)
    }
}
