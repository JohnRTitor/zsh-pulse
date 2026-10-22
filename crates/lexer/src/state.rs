//! The resumable lexer state.
//!
//! # Why the state has to be exact
//!
//! The incremental lexer works by resuming from a checkpoint and comparing the
//! state it reaches with the state recorded at the same place in the previous
//! parse. If the two states are merely *similar* the splice is wrong: the
//! tail would be highlighted as if it were somewhere it is not.
//!
//! So the state is a complete description of "where am I in the grammar", it
//! is `Copy` (no heap, no lifetimes) and it compares exactly.
//!
//! # What the state holds
//!
//! | Component | Why it matters |
//! |---|---|
//! | the context stack | a `)` only closes a `$(` if the `$(` is still open |
//! | the per-level flags | `foo=bar` is an assignment only in command position |
//! | the pending here-document | the body is only terminated by its delimiter |
//!
//! # Why the flags are per level
//!
//! `expect_command` ("the next word starts a command") is a property of the
//! *current* context, not of the whole parse: inside `$( ... )` the next word
//! is a command, but the word after the closing `)` is an argument again.
//! A single global flag would lose that, so each level of the stack saves
//! the flags of the level below it and restores them on the way out.
//!
//! The here-document delimiter is stored **inline** rather than behind a
//! pointer or a hash, because the state must be exactly comparable: two
//! states that differ only in their delimiter text are different states.

/// Maximum number of nested contexts (`(`, `"`, `` ` ``, `${`).
///
/// A deeper input is rejected rather than truncated: see
/// [`crate::LexError::TooDeep`].
pub const MAX_DEPTH: usize = 8;

/// Maximum length, in bytes, of a here-document delimiter.
///
/// Longer delimiters are rejected rather than truncated, because a truncated
/// delimiter would terminate the body at the wrong place.
pub const MAX_HEREDOC_LEN: usize = 32;

/// The context stack is full.
///
/// Returned by [`LexState::push`] instead of a bare `()`, so the error says
/// what went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooDeep;

/// An open syntactic context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctx {
    /// Top level: bare words and operators.
    Word,
    /// Inside `"..."`.
    Double,
    /// Inside `` `...` ``.
    Backtick,
    /// Inside `$(...)` or `(...)`.
    Paren,
    /// Inside `${...}`.
    Brace,
}

/// A pending here-document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heredoc {
    /// The delimiter bytes, inline.
    buf: [u8; MAX_HEREDOC_LEN],
    /// Number of valid bytes in `buf`. Zero means "no pending here-document".
    len: u8,
    /// `<<-`: leading tabs are stripped from the body and from the delimiter
    /// line before comparison.
    strip_tabs: bool,
    /// The delimiter was quoted, so it is matched literally.
    quoted: bool,
}

impl Heredoc {
    /// The empty, "no pending here-document" value.
    pub const EMPTY: Self =
        Self { buf: [0; MAX_HEREDOC_LEN], len: 0, strip_tabs: false, quoted: false };

    /// Build a pending here-document from its delimiter bytes.
    ///
    /// Returns `None` if the delimiter is empty or longer than
    /// [`MAX_HEREDOC_LEN`].
    pub fn new(delimiter: &[u8], strip_tabs: bool, quoted: bool) -> Option<Self> {
        if delimiter.is_empty() || delimiter.len() > MAX_HEREDOC_LEN {
            return None;
        }
        let mut buf = [0u8; MAX_HEREDOC_LEN];
        buf[..delimiter.len()].copy_from_slice(delimiter);
        Some(Self { buf, len: delimiter.len() as u8, strip_tabs, quoted })
    }

    /// The delimiter bytes.
    pub fn delimiter(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }

    /// Whether the body has leading tabs stripped (`<<-`).
    pub fn strip_tabs(&self) -> bool {
        self.strip_tabs
    }

    /// Whether the delimiter was quoted.
    pub fn quoted(&self) -> bool {
        self.quoted
    }
}

/// The flags of one level of the context stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags(u8);

impl Flags {
    const EXPECT_COMMAND: u8 = 1 << 0;
    const AFTER_REDIRECT: u8 = 1 << 1;

    /// Flags for a context that holds a string: the contents are not a
    /// command list, so nothing is in command position.
    pub const fn string_context() -> Self {
        Self(0)
    }

    /// Flags for a context that holds a command list: the next word starts a
    /// command.
    pub const fn command_context() -> Self {
        Self(Self::EXPECT_COMMAND)
    }

    /// Whether the next word begins a command.
    pub fn expect_command(self) -> bool {
        self.0 & Self::EXPECT_COMMAND != 0
    }

    /// Set or clear "the next word begins a command".
    pub fn set_expect_command(&mut self, on: bool) {
        if on {
            self.0 |= Self::EXPECT_COMMAND;
        } else {
            self.0 &= !Self::EXPECT_COMMAND;
        }
    }

    /// Whether the previous token was a redirection.
    ///
    /// zsh's `inredir` (`zsh-src/Src/lex.c:362-367`): the token after a
    /// redirection is the redirection's target, not a command.
    pub fn after_redirect(self) -> bool {
        self.0 & Self::AFTER_REDIRECT != 0
    }

    /// Set or clear "the previous token was a redirection".
    pub fn set_after_redirect(&mut self, on: bool) {
        if on {
            self.0 |= Self::AFTER_REDIRECT;
        } else {
            self.0 &= !Self::AFTER_REDIRECT;
        }
    }
}

/// The complete, exactly-comparable lexer state.
///
/// `Copy` and allocation-free by construction: the context stack and the
/// here-document delimiter are stored inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexState {
    /// Open contexts, outermost first. `stack[0]` is always
    /// [`Ctx::Word`].
    stack: [Ctx; MAX_DEPTH],
    /// The flags of the level below each stack entry, restored on pop.
    saved_flags: [Flags; MAX_DEPTH],
    /// Number of live entries in `stack`.
    len: u8,
    /// The flags of the current level.
    flags: Flags,
    /// The pending here-document, if any.
    heredoc: Heredoc,
}

impl Default for LexState {
    fn default() -> Self {
        Self::new()
    }
}

impl LexState {
    /// The state of a fresh, top-level scan.
    pub const fn new() -> Self {
        Self {
            stack: [Ctx::Word; MAX_DEPTH],
            saved_flags: [Flags(0); MAX_DEPTH],
            len: 1,
            flags: Flags::command_context(),
            heredoc: Heredoc::EMPTY,
        }
    }

    /// The innermost open context.
    pub fn context(&self) -> Ctx {
        self.stack[self.len as usize - 1]
    }

    /// How many contexts are open.
    pub fn depth(&self) -> usize {
        self.len as usize
    }

    /// Push a context with the given flags.
    ///
    /// The current level's flags are saved and restored when the context is
    /// popped.
    ///
    /// # Errors
    ///
    /// Returns [`TooDeep`] if the stack is already [`MAX_DEPTH`] deep.
    pub fn push(&mut self, ctx: Ctx, flags: Flags) -> Result<(), TooDeep> {
        let len = self.len as usize;
        if len >= MAX_DEPTH {
            return Err(TooDeep);
        }
        self.saved_flags[len] = self.flags;
        self.stack[len] = ctx;
        self.len += 1;
        self.flags = flags;
        Ok(())
    }

    /// Pop the innermost context, restoring the flags of the level below.
    ///
    /// The bottom context is never popped: the state always has at least one
    /// entry.
    pub fn pop(&mut self) {
        if self.len > 1 {
            self.len -= 1;
            self.flags = self.saved_flags[self.len as usize];
            // Keep the state canonical: two states that mean the same thing
            // must compare equal, so the vacated slot is reset.
            self.stack[self.len as usize] = Ctx::Word;
            self.saved_flags[self.len as usize] = Flags(0);
        }
    }

    /// Whether the next word begins a command.
    pub fn expect_command(&self) -> bool {
        self.flags.expect_command()
    }

    /// Set or clear "the next word begins a command".
    pub fn set_expect_command(&mut self, on: bool) {
        self.flags.set_expect_command(on);
    }

    /// Whether the previous token was a redirection.
    pub fn after_redirect(&self) -> bool {
        self.flags.after_redirect()
    }

    /// Set or clear "the previous token was a redirection".
    pub fn set_after_redirect(&mut self, on: bool) {
        self.flags.set_after_redirect(on);
    }

    /// Whether the scanner is inside a here-document body.
    pub fn in_heredoc_body(&self) -> bool {
        false
    }

    /// The pending here-document, if any.
    pub fn pending_heredoc(&self) -> Option<&Heredoc> {
        if self.heredoc.len == 0 {
            None
        } else {
            Some(&self.heredoc)
        }
    }

    /// Record a pending here-document.
    pub fn set_pending_heredoc(&mut self, heredoc: Heredoc) {
        self.heredoc = heredoc;
    }

    /// Forget the pending here-document.
    pub fn clear_pending_heredoc(&mut self) {
        self.heredoc = Heredoc::EMPTY;
    }

    /// A compact fingerprint of the state.
    ///
    /// This is a *pre-filter*, not the comparison itself: two states with the
    /// same key are not guaranteed to be equal, but two states with different
    /// keys are definitely different. The incremental lexer uses it to skip
    /// the exact comparison for the overwhelming majority of checkpoints.
    pub fn key(&self) -> u64 {
        // FNV-1a over the raw state bytes.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |b: u8| {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        };
        for c in &self.stack[..self.len as usize] {
            mix(*c as u8);
        }
        for f in &self.saved_flags[..self.len as usize] {
            mix(f.0);
        }
        mix(self.len);
        mix(self.flags.0);
        mix(self.heredoc.len);
        for b in self.heredoc.delimiter() {
            mix(*b);
        }
        mix(self.heredoc.strip_tabs as u8);
        mix(self.heredoc.quoted as u8);
        h
    }

    /// Whether this state and `other` describe the same parse position.
    ///
    /// Exact: no hashing, no tolerance. This is the check the splice relies
    /// on, so it must never report "equal" for states that differ.
    pub fn same_parse(&self, other: &LexState) -> bool {
        self == other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_state_is_at_command_position() {
        let s = LexState::new();
        assert!(s.expect_command());
        assert_eq!(s.context(), Ctx::Word);
        assert_eq!(s.depth(), 1);
        assert!(s.pending_heredoc().is_none());
    }

    #[test]
    fn state_is_copy_and_small() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<LexState>();
        // The state is stored once per resume point; keeping it small keeps
        // the resume point array cheap.
        assert!(
            core::mem::size_of::<LexState>() <= 64,
            "LexState grew to {} bytes",
            core::mem::size_of::<LexState>()
        );
    }

    #[test]
    fn equal_states_have_equal_keys_and_different_ones_differ() {
        let a = LexState::new();
        let mut b = LexState::new();
        assert_eq!(a.key(), b.key());
        assert!(a.same_parse(&b));
        b.set_expect_command(false);
        assert_ne!(a.key(), b.key());
        assert!(!a.same_parse(&b));
    }

    #[test]
    fn the_stack_cannot_be_overflowed_or_underflowed() {
        let mut s = LexState::new();
        for _ in 0..MAX_DEPTH - 1 {
            s.push(Ctx::Paren, Flags::command_context()).expect("room");
        }
        assert!(s.push(Ctx::Paren, Flags::command_context()).is_err());
        // The bottom context is never popped.
        for _ in 0..MAX_DEPTH {
            s.pop();
        }
        assert_eq!(s.depth(), 1);
        assert_eq!(s.context(), Ctx::Word);
    }

    #[test]
    fn flags_are_restored_when_a_context_is_popped() {
        let mut s = LexState::new();
        s.set_expect_command(false);
        s.push(Ctx::Paren, Flags::command_context()).expect("room");
        assert!(s.expect_command(), "inside `$( )` a command is expected");
        s.pop();
        assert!(!s.expect_command(), "the outer level's flags must come back");
    }

    #[test]
    fn a_popped_state_compares_equal_to_a_fresh_one() {
        // Two states that mean the same thing must compare equal, or the
        // incremental lexer would refuse to splice a tail it could safely
        // reuse.
        let mut a = LexState::new();
        a.push(Ctx::Paren, Flags::command_context()).expect("room");
        a.pop();
        let b = LexState::new();
        assert!(a.same_parse(&b), "{a:?} vs {b:?}");
        assert_eq!(a.key(), b.key());
    }

    #[test]
    fn heredoc_delimiters_are_compared_exactly() {
        let a = Heredoc::new(b"EOF", false, false).unwrap();
        let b = Heredoc::new(b"EOT", false, false).unwrap();
        assert_ne!(a, b);
        assert_eq!(a.delimiter(), b"EOF");
    }

    #[test]
    fn heredoc_rejects_empty_and_oversized_delimiters() {
        assert!(Heredoc::new(b"", false, false).is_none());
        assert!(Heredoc::new(&[b'x'; MAX_HEREDOC_LEN], false, false).is_some());
        assert!(Heredoc::new(&[b'x'; MAX_HEREDOC_LEN + 1], false, false).is_none());
    }

    #[test]
    fn a_pending_heredoc_is_part_of_the_state() {
        let mut a = LexState::new();
        let b = LexState::new();
        a.set_pending_heredoc(Heredoc::new(b"EOF", false, false).unwrap());
        assert!(!a.same_parse(&b));
        assert_eq!(a.pending_heredoc().map(|h| h.delimiter()), Some(&b"EOF"[..]));
    }
}
