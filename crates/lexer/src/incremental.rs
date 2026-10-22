//! Incremental re-lexing.
//!
//! # The idea
//!
//! Typing one character changes one small part of the line. Re-scanning the
//! whole line for every keystroke is wasteful, and on long lines it is
//! wasteful *quadratically* over a session.
//!
//! The approach, borrowed from ble.sh's checkpoint splice
//! (`ble.sh/lib/core-syntax.sh:5540-5550`), is:
//!
//! 1. Diff the new line against the old one: find the common prefix and the
//!    common suffix. Everything between them changed.
//! 2. Resume scanning from the last **resume point** at or before the start
//!    of the change.
//! 3. While re-scanning, look for a point where the new parse and the old
//!    parse **agree exactly** about the state of the grammar and the
//!    surrounding text is unchanged. At such a point the old tail can be
//!    spliced back on.
//!
//! # Why the agreement has to be exact
//!
//! The spliced tail is the old parse's tail, byte for byte. It is only
//! correct if the new parse has reached the same grammatical position. A
//! "close enough" test would splice a tail parsed as if it were inside a
//! quote onto a line where it is not, and the user would see highlighting
//! that lies about their command.
//!
//! So the comparison is [`LexState::same_parse`], an exact equality, and the
//! byte ranges must line up as well.
//!
//! # What is measured
//!
//! [`RelexOutcome`] reports how much work was actually done, so the claim
//! "the incremental path is cheaper" can be checked rather than assumed.

use crate::scanner::Scanner;
use crate::ResumePoint;
use crate::state::LexState;
use crate::token::Token;
use crate::LexError;

/// The shortest line for which the incremental path is worth its overhead.
///
/// Measured, not guessed: below this length a full re-parse is faster,
/// because the incremental path has to diff the line against the previous
/// one and merge two token lists, and a short line does not have enough
/// work for that to pay off.
///
/// The crossover is measured by `examples/bench_incremental.rs`, against the
/// module's *own* full parse (which reuses its buffers, so the comparison
/// is not between an allocating and an allocation-free implementation).
/// With that baseline the crossover is between 86 and 169 bytes.
pub const INCREMENTAL_MIN_LEN: usize = 128;

/// The capacity the lexer keeps for itself, in bytes.
///
/// The same figure the module's own scratch buffer keeps
/// (`crates/zsh-module/src/lib.rs`): a kilobyte is more than an ordinary
/// command line needs, and the module only ever hands the lexer lines of a
/// few kilobytes at most.
const KEEP_BYTES: usize = 1024;

/// How much bigger than the line the capacity may be before it is released.
///
/// Four times the line is comfortably more than a command line needs, so
/// the buffers do not reallocate while the user is editing one.
const KEEP_RATIO: usize = 4;

/// Give back the capacity a buffer no longer needs.
///
/// `Vec::clear` keeps the capacity, so without this the cache would hold
/// the largest line the user has ever pasted for the rest of the session:
/// the token and resume-point buffers are sized by the *longest* line the
/// user ever typed, not by the current one.
///
/// The rule is hysteretic — the same rule the module's own scratch buffer
/// uses. Shrinking on every keystroke would be worse than the problem it
/// solves: `shrink_to` reallocates, and an allocation on the interactive
/// thread is exactly what these buffers exist to avoid. The capacity is
/// therefore released only when it is large in absolute terms *and* much
/// larger than what the line needs, so a line that grows and shrinks within
/// the retained capacity never triggers a reallocation.
///
/// `needed` is how many elements the buffer has to hold for the current
/// line: the yardstick the capacity is measured against. The buffer itself
/// may be holding something else — or nothing at all.
fn release_capacity<T>(buf: &mut Vec<T>, needed: usize) {
    let element = core::mem::size_of::<T>();
    // A zero-sized element has no capacity to give back.
    if element == 0 {
        return;
    }
    let capacity = buf.capacity().saturating_mul(element);
    // Keep enough room for the line itself and for one a few times longer,
    // so that ordinary editing never reallocates.
    let keep = KEEP_BYTES.max(needed.saturating_mul(element).saturating_mul(KEEP_RATIO));
    if capacity > keep {
        // `shrink_to` never drops below the length, so a buffer that still
        // holds something keeps exactly what it holds.
        buf.shrink_to(KEEP_BYTES / element);
    }
}

/// What an incremental re-lex did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelexOutcome {
    /// Whether the previous parse's tail was reused.
    pub spliced: bool,
    /// How many bytes of the new line were scanned.
    pub rescanned_bytes: usize,
    /// How many tokens came from the previous parse.
    pub reused_tokens: usize,
    /// How many tokens the line has in total.
    pub total_tokens: usize,
    /// Whether the line was byte-for-byte unchanged.
    pub unchanged: bool,
}

/// A cached parse of a line, able to re-lex an edited line incrementally.
///
/// The cache owns the previous line's text, its tokens and its resume
/// points. All three are reused across keystrokes, so a keystroke allocates
/// nothing once the line's length has stabilised.
#[derive(Debug, Default)]
pub struct LexCache {
    text: Vec<u8>,
    /// The tokens of the current parse.
    tokens: Vec<Token>,
    /// The resume points of the current parse.
    resumes: Vec<ResumePoint>,
    /// Scratch space for the re-scan.
    ///
    /// The re-scan writes here rather than into fresh vectors, so a
    /// keystroke does not allocate.
    scratch_tokens: Vec<Token>,
    scratch_resumes: Vec<ResumePoint>,
    /// Scratch space for the merge.
    ///
    /// The merge reads the current parse and writes the new one, so it
    /// needs somewhere else to put the result. The two are swapped
    /// afterwards, which keeps both allocations for the life of the cache.
    merged_tokens: Vec<Token>,
    merged_resumes: Vec<ResumePoint>,
    final_state: LexState,
}

impl LexCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self {
            text: Vec::new(),
            tokens: Vec::new(),
            resumes: Vec::new(),
            scratch_tokens: Vec::new(),
            scratch_resumes: Vec::new(),
            merged_tokens: Vec::new(),
            merged_resumes: Vec::new(),
            final_state: LexState::new(),
        }
    }

    /// The tokens of the last parse.
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    /// The resume points of the last parse.
    pub fn resume_points(&self) -> &[ResumePoint] {
        &self.resumes
    }

    /// The text of the last parse.
    pub fn text(&self) -> &[u8] {
        &self.text
    }

    /// Scan a line, replacing whatever was cached.
    ///
    /// # Errors
    ///
    /// Returns [`LexError`] for input the scanner cannot represent exactly.
    pub fn lex(&mut self, text: &[u8]) -> Result<usize, LexError> {
        let final_state = {
            let scanner = Scanner::new(
                text,
                LexState::new(),
                &mut self.scratch_tokens,
                &mut self.scratch_resumes,
            );
            let (_, _, final_state) = scanner.run()?;
            final_state
        };
        self.text.clear();
        self.text.extend_from_slice(text);
        core::mem::swap(&mut self.tokens, &mut self.scratch_tokens);
        core::mem::swap(&mut self.resumes, &mut self.scratch_resumes);
        self.final_state = final_state;
        // The parse is complete and the buffers have been swapped, so
        // nothing that is still in use can be touched.
        self.release_unused_capacity();
        Ok(self.tokens.len())
    }

    /// Re-lex a line that has been edited.
    ///
    /// The edit is detected by comparing the new line with the cached one; the
    /// caller does not have to describe it.
    ///
    /// For a line shorter than [`INCREMENTAL_MIN_LEN`] this does a full
    /// re-parse instead, because that is measurably faster at that length.
    /// Use [`LexCache::relex_incremental`] to force the incremental path.
    ///
    /// # Errors
    ///
    /// Returns [`LexError`] for input the scanner cannot represent exactly.
    /// The cache is left untouched in that case, so the caller can fall back
    /// to unstyled text and try again later.
    pub fn relex(&mut self, new_text: &[u8]) -> Result<RelexOutcome, LexError> {
        if new_text.len() < INCREMENTAL_MIN_LEN {
            return self.relex_short(new_text);
        }
        self.relex_incremental(new_text)
    }

    /// Re-lex a line, always taking the incremental path.
    ///
    /// This is the raw machinery, without the length policy of
    /// [`LexCache::relex`]. It exists so that the incremental path can be
    /// tested and measured on its own.
    ///
    /// # Errors
    ///
    /// Returns [`LexError`] for input the scanner cannot represent exactly.
    pub fn relex_incremental(&mut self, new_text: &[u8]) -> Result<RelexOutcome, LexError> {
        let old_len = self.text.len();
        let new_len = new_text.len();

        // The common prefix and the common suffix. The suffix is bounded so
        // that it cannot overlap the prefix.
        let max_p = old_len.min(new_len);
        let mut p = 0usize;
        while p < max_p && self.text[p] == new_text[p] {
            p += 1;
        }
        let mut s = 0usize;
        while s < max_p - p && self.text[old_len - 1 - s] == new_text[new_len - 1 - s] {
            s += 1;
        }
        let delta = new_len as isize - old_len as isize;

        if p == old_len && p == new_len {
            // The line did not change at all.
            return Ok(RelexOutcome {
                spliced: false,
                rescanned_bytes: 0,
                reused_tokens: self.tokens.len(),
                total_tokens: self.tokens.len(),
                unchanged: true,
            });
        }

        // The resume point to restart from.
        //
        // A resume point is only still valid if the character that decided
        // where its unit starts is unchanged. That character is at or before
        // the start of the change, so a resume point whose `guard` is at or
        // after the change may have moved: editing the newline that ended a
        // here-document delimiter, for instance, changes where the
        // delimiter word ends.
        //
        // If no resume point survives, the whole line is re-scanned.
        let restart = self
            .resumes
            .partition_point(|r| (r.guard as usize) < p)
            .checked_sub(1)
            .filter(|i| (self.resumes[*i].byte_offset as usize) <= p);

        let (cp_token_index, cp_offset, cp_state, cp_guard) = match restart {
            Some(i) => {
                let r = self.resumes[i];
                (
                    r.token_index as usize,
                    r.byte_offset as usize,
                    r.state,
                    r.guard as usize,
                )
            }
            None => (0, 0, LexState::new(), 0),
        };

        // Re-scan from the resume point, stopping as soon as the new parse
        // and the old one agree about the state of the grammar.
        //
        // The scan may only stop inside the *common suffix*: before it, the
        // text differs, so the old parse says nothing about the new one.
        let suffix_start = new_len - s;
        // (new token index, old resume point index, the new guard)
        //
        // A cell, because the probe closure holds the borrow.
        let convergence: std::cell::Cell<Option<(usize, usize, usize)>> =
            std::cell::Cell::new(None);
        // Where the scan actually stopped.
        let scanned_to = std::cell::Cell::new(new_len);

        // The probe walks forward through the old resume points, so the
        // lookup is a cursor rather than a binary search: the scan only
        // ever moves forward, and a binary search per unit would make the
        // re-scan O(units log units).
        let mut cursor = 0usize;
        let mut probe = |token_index: usize, offset: usize, guard: usize, state: &LexState| {
                if offset < suffix_start {
                    return false;
                }
                let o = offset as isize - delta;
                if !(0..=old_len as isize).contains(&o) {
                    return false;
                }
                let o = o as usize;
                while cursor < self.resumes.len()
                    && (self.resumes[cursor].byte_offset as usize) < o
                {
                    cursor += 1;
                }
                if cursor >= self.resumes.len()
                    || (self.resumes[cursor].byte_offset as usize) != o
                {
                    return false;
                }
                let old = &self.resumes[cursor];
                // The key is a cheap fingerprint; the exact comparison is
                // what decides.
                if old.state.key() != state.key() || !old.state.same_parse(state) {
                    return false;
                }
                convergence.set(Some((token_index, cursor, guard)));
                scanned_to.set(offset);
                true
        };
        let (new_tokens, new_resumes, new_final_state) = {
            let scanner = Scanner::new_at(
                new_text,
                cp_offset,
                cp_state,
                cp_guard,
                &mut self.scratch_tokens,
                &mut self.scratch_resumes,
            )
            .with_probe(&mut probe);
            scanner.run()?
        };
        // The bytes the scanner actually read: the scan stopped at the
        // convergence point, so this is the distance to convergence and not
        // the length of the rest of the line.
        let rescanned_bytes = scanned_to.get().saturating_sub(cp_offset);

        // The merge reads the current parse and writes the new one, so it
        // writes into separate buffers which are swapped in afterwards. No
        // allocation happens in the steady state.
        let mut tokens = std::mem::take(&mut self.merged_tokens);
        let mut resumes = std::mem::take(&mut self.merged_resumes);
        tokens.clear();
        resumes.clear();

        // Everything before the restart point is untouched by the edit.
        let head_resumes = self
            .resumes
            .partition_point(|r| (r.token_index as usize) < cp_token_index);
        resumes.extend_from_slice(&self.resumes[..head_resumes]);
        tokens.extend_from_slice(&self.tokens[..cp_token_index]);

        match convergence.get() {
            Some((new_count, oi, new_guard)) => {
                // The new parse's tokens up to the convergence point.
                tokens.extend_from_slice(&new_tokens[..new_count]);
                // The resume points of the units that were re-scanned. The
                // unit at the convergence point was never scanned, so it has
                // no resume point of its own.
                let li = new_resumes.partition_point(|r| (r.token_index as usize) < new_count);
                for r in new_resumes[..li].iter() {
                    resumes.push(ResumePoint {
                        token_index: r.token_index + cp_token_index as u32,
                        byte_offset: r.byte_offset,
                        state: r.state,
                        guard: r.guard,
                    });
                }
                // The old tail, shifted by the length of the edit.
                //
                // The tail's token indices are *re-based*: the changed
                // region may have produced a different number of tokens than
                // the region it replaced, so the old indices cannot be
                // reused as they are.
                let tail_start = self.resumes[oi].token_index as usize;
                let tail_base = cp_token_index + new_count;
                for t in &self.tokens[tail_start..] {
                    tokens.push(Token::new(
                        (t.start as isize + delta) as u32,
                        (t.end as isize + delta) as u32,
                        t.kind,
                    ));
                }
                for (i, r) in self.resumes[oi..].iter().enumerate() {
                    // The guard of the *first* tail unit cannot be shifted:
                    // it is the end of the last unit that was re-scanned,
                    // and that unit's length may have changed. The guards of
                    // the units after it are inside the tail, so shifting
                    // them by the length of the edit is right.
                    let guard = if i == 0 {
                        new_guard as u32
                    } else {
                        (r.guard as isize + delta) as u32
                    };
                    let point = ResumePoint {
                        token_index: tail_base as u32
                            + (r.token_index as usize - tail_start) as u32,
                        byte_offset: (r.byte_offset as isize + delta) as u32,
                        state: r.state,
                        guard,
                    };
                    // The new and the old point can describe the same place
                    // when the edit did not move the text.
                    if resumes.last().is_some_and(|last| {
                        last.token_index == point.token_index
                            && last.byte_offset == point.byte_offset
                    }) {
                        continue;
                    }
                    resumes.push(point);
                }
                let reused = self.tokens.len() - tail_start;
                let total = tokens.len();
                // The tail was spliced back on, so the state at the end of
                // the line is the *old* one: the spliced tail is the end of
                // the line, and it did not change.
                self.commit(new_text, tokens, resumes, self.final_state);
                Ok(RelexOutcome {
                    spliced: true,
                    rescanned_bytes,
                    reused_tokens: reused,
                    total_tokens: total,
                    unchanged: false,
                })
            }
            None => {
                // No agreement: the re-scan stands on its own.
                tokens.extend_from_slice(new_tokens);
                for r in new_resumes.iter() {
                    resumes.push(ResumePoint {
                        token_index: r.token_index + cp_token_index as u32,
                        byte_offset: r.byte_offset,
                        state: r.state,
                        guard: r.guard,
                    });
                }
                let total = tokens.len();
                self.commit(new_text, tokens, resumes, new_final_state);
                Ok(RelexOutcome {
                    spliced: false,
                    rescanned_bytes,
                    reused_tokens: cp_token_index,
                    total_tokens: total,
                    unchanged: false,
                })
            }
        }
    }

    /// Re-lex a short line by re-parsing it from scratch.
    ///
    /// A short line has too little work in it for the incremental path to
    /// pay for its own overhead.
    fn relex_short(&mut self, new_text: &[u8]) -> Result<RelexOutcome, LexError> {
        // A redraw that does not change the line is common — cursor
        // movement, a completion display, a resize. Re-parsing it would make
        // navigating with the arrow keys as expensive as typing.
        if self.text == new_text {
            return Ok(RelexOutcome {
                spliced: false,
                rescanned_bytes: 0,
                reused_tokens: self.tokens.len(),
                total_tokens: self.tokens.len(),
                unchanged: true,
            });
        }
        let total = self.lex(new_text)?;
        Ok(RelexOutcome {
            spliced: false,
            rescanned_bytes: new_text.len(),
            reused_tokens: 0,
            total_tokens: total,
            unchanged: false,
        })
    }

    fn commit(
        &mut self,
        text: &[u8],
        mut tokens: Vec<Token>,
        mut resumes: Vec<ResumePoint>,
        final_state: LexState,
    ) {
        self.text.clear();
        self.text.extend_from_slice(text);
        // The buffers that were just filled become the current parse; the
        // old ones become the scratch for the next time round.
        core::mem::swap(&mut self.tokens, &mut tokens);
        core::mem::swap(&mut self.resumes, &mut resumes);
        self.merged_tokens = tokens;
        self.merged_resumes = resumes;
        self.final_state = final_state;
        // The merge is over and the buffers have been swapped, so the
        // capacity can be released. Doing it any earlier would shrink a
        // buffer the merge is about to read: `shrink_to` reallocates, and
        // the parse would be taken out from under the merge.
        self.release_unused_capacity();
    }

    /// Give back the capacity the buffers no longer need.
    ///
    /// # Why
    ///
    /// `Vec::clear` keeps the capacity, so without this the cache would
    /// hold the largest line the user has ever pasted for the rest of the
    /// session: the buffers are sized by the *longest* line, not by the
    /// current one.
    ///
    /// # When
    ///
    /// Only once a parse has finished and the buffers have been swapped, so
    /// that no buffer which is still in use is touched. The current parse
    /// (`text`, `tokens`, `resumes`) keeps what it holds — it is what the
    /// caller reads. The scratch and the merge buffers are refilled from
    /// scratch on the next keystroke, so what they hold is of no use to
    /// anybody and is given back here.
    fn release_unused_capacity(&mut self) {
        // How much the current line needs: the yardstick every buffer is
        // measured against.
        let text = self.text.len();
        let tokens = self.tokens.len();
        let resumes = self.resumes.len();

        // The current parse: it keeps its contents, and enough room for
        // the line to grow into.
        release_capacity(&mut self.text, text);
        release_capacity(&mut self.tokens, tokens);
        release_capacity(&mut self.resumes, resumes);

        // The scratch and the merge buffers are emptied before every use,
        // so whatever they hold is dead weight.
        self.scratch_tokens.clear();
        self.scratch_resumes.clear();
        self.merged_tokens.clear();
        self.merged_resumes.clear();
        release_capacity(&mut self.scratch_tokens, tokens);
        release_capacity(&mut self.scratch_resumes, resumes);
        release_capacity(&mut self.merged_tokens, tokens);
        release_capacity(&mut self.merged_resumes, resumes);
    }

    /// The parse state at the end of the last scan.
    pub fn final_state(&self) -> LexState {
        self.final_state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex;

    /// Re-lex a line and check the result against a full scan.
    fn check(old: &str, new: &str) -> RelexOutcome {
        let mut cache = LexCache::new();
        cache.lex(old.as_bytes()).expect("old line scans");
        // The raw incremental path, so that the machinery is exercised even
        // for the short lines the policy would re-parse.
        let outcome = cache.relex_incremental(new.as_bytes()).expect("new line scans");
        let full = lex(new.as_bytes()).expect("full scan");
        assert_eq!(
            cache.tokens(),
            full.tokens.as_slice(),
            "incremental result differs from a full scan for {old:?} -> {new:?}"
        );
        outcome
    }

    /// The capacity the cache is holding on to, in bytes.
    ///
    /// The sum of every buffer's capacity: what the shell would keep alive
    /// for the rest of the session, whether or not the line it holds needs
    /// it.
    fn retained_capacity(cache: &LexCache) -> usize {
        let token = core::mem::size_of::<Token>();
        let resume = core::mem::size_of::<ResumePoint>();
        cache.text.capacity()
            + cache.tokens.capacity() * token
            + cache.resumes.capacity() * resume
            + cache.scratch_tokens.capacity() * token
            + cache.scratch_resumes.capacity() * resume
            + cache.merged_tokens.capacity() * token
            + cache.merged_resumes.capacity() * resume
    }

    #[test]
    fn an_unchanged_line_is_recognised() {
        let mut cache = LexCache::new();
        cache.lex(b"echo hi").expect("scannable");
        let outcome = cache.relex_incremental(b"echo hi").expect("scannable");
        assert!(outcome.unchanged);
        assert_eq!(outcome.rescanned_bytes, 0);
    }

    #[test]
    fn a_short_line_is_re_parsed_rather_than_incrementally_edited() {
        // Below the measured crossover a full re-parse is cheaper, so that is
        // what happens. The result must still be correct.
        let mut cache = LexCache::new();
        cache.lex(b"echo hi").expect("scannable");
        let outcome = cache.relex(b"echo bye").expect("scannable");
        assert!(!outcome.spliced);
        assert_eq!(outcome.rescanned_bytes, 8);
        let full = lex(b"echo bye").expect("scannable");
        assert_eq!(cache.tokens(), full.tokens.as_slice());
    }

    #[test]
    fn a_long_line_uses_the_incremental_path() {
        let mut cache = LexCache::new();
        // Longer than `INCREMENTAL_MIN_LEN`, so the policy takes the
        // incremental path.
        let long = format!(
            "echo {} bbbb cccc dddd",
            "a".repeat(INCREMENTAL_MIN_LEN + 32)
        );
        cache.lex(long.as_bytes()).expect("scannable");
        // An edit in the middle, so that there is a tail to splice.
        let edited = long.replace("cccc", "eeee");
        let outcome = cache.relex(edited.as_bytes()).expect("scannable");
        assert!(outcome.spliced, "a long line must take the incremental path");
    }

    #[test]
    fn a_mid_line_edit_produces_the_same_tokens_as_a_full_scan() {
        let outcome = check("echo hello world", "echo brave world");
        assert!(!outcome.unchanged);
        assert!(outcome.rescanned_bytes < 17, "should not rescan the whole line");
    }

    #[test]
    fn every_single_character_edit_agrees_with_a_full_scan() {
        let base = "git commit --amend --no-edit";
        for i in 0..=base.len() {
            let mut edited = base.to_string();
            edited.insert(i, 'x');
            check(base, &edited);
        }
    }

    #[test]
    fn every_single_character_deletion_agrees_with_a_full_scan() {
        let base = "git commit --amend --no-edit";
        for i in 0..base.len() {
            let mut edited = base.to_string();
            edited.remove(i);
            check(base, &edited);
        }
    }

    #[test]
    fn an_edit_that_opens_a_quote_agrees_with_a_full_scan() {
        check("echo hi", "echo 'hi");
        check("echo 'hi'", "echo 'hi'");
        check("echo 'hi'", "echo \"hi");
        check("echo $(cmd)", "echo $(cmd");
        check("echo ${VAR}", "echo ${VAR");
    }

    #[test]
    fn an_edit_inside_a_heredoc_agrees_with_a_full_scan() {
        check("cat <<EOF\nbody\nEOF\n", "cat <<EOF\nbodyx\nEOF\n");
        check("cat <<EOF\nbody\nEOF\n", "cat <<EOFX\nbody\nEOF\n");
    }

    #[test]
    fn a_chained_edit_that_changes_a_token_kind_agrees_with_a_full_scan() {
        // Regression test.
        //
        // `$$` is a parameter expansion; a lone `$` is an ordinary
        // character. So the second `$` in this line changes the *kind* of
        // the token, not just its length.
        //
        // The incremental path decides to reuse the old tail by comparing
        // the parse state. The state alone does not decide the kind of a
        // token — that also depends on the character that follows it — so
        // the restart point has to be chosen so that the character which
        // decided the previous token is not one of the edited ones.
        let mut cache = LexCache::new();
        cache.lex(b"cat <<EOF\nbody line\nEOF\n").expect("scannable");
        cache.relex(b"cat $<<EOF\nbody line\nEOF\n").expect("scannable");
        cache.relex(b"cat $$<<EOF\nbody line\nEOF\n").expect("scannable");
        let full = lex(b"cat $$<<EOF\nbody line\nEOF\n").expect("scannable");
        assert_eq!(cache.tokens(), full.tokens.as_slice());
    }

    #[test]
    fn a_splice_is_found_for_a_mid_line_edit() {
        // The tail ("world") is unchanged, so it can be spliced back on.
        let outcome = check("echo hello world", "echo brave world");
        assert!(outcome.spliced, "the tail should have been reused");
        assert!(outcome.reused_tokens > 0);
    }

    #[test]
    fn an_append_has_no_tail_to_splice() {
        // Nothing after the edit is unchanged, so there is nothing to reuse.
        let outcome = check("echo hello world", "echo hello worlds");
        assert!(!outcome.spliced);
    }

    #[test]
    fn the_cache_survives_many_keystrokes() {
        let mut cache = LexCache::new();
        let mut line = String::new();
        for c in "echo 'hello world' | grep -i x > out.txt".chars() {
            line.push(c);
            cache.relex(line.as_bytes()).expect("scannable");
            let full = lex(line.as_bytes()).expect("scannable");
            assert_eq!(cache.tokens(), full.tokens.as_slice(), "after typing {line:?}");
        }
    }

    #[test]
    fn deleting_everything_leaves_an_empty_line() {
        let mut cache = LexCache::new();
        cache.lex(b"echo hi").expect("scannable");
        cache.relex(b"").expect("scannable");
        assert!(cache.tokens().is_empty());
    }

    // --- the retained capacity ---------------------------------------------

    /// A pasted line: many words, so that the token and resume-point
    /// buffers grow as well as the text.
    fn pasted_line(words: usize) -> String {
        "word ".repeat(words)
    }

    #[test]
    fn the_cache_releases_the_capacity_of_a_line_it_no_longer_holds() {
        // The point of the whole exercise: without this the shell would
        // hold the biggest line the user has ever pasted for the rest of
        // the session.
        let mut cache = LexCache::new();
        let big = pasted_line(16 * 1024);
        assert_eq!(big.len(), 80 * 1024);
        cache.lex(big.as_bytes()).expect("scannable");
        let grown = retained_capacity(&cache);
        assert!(
            grown > 1024 * 1024,
            "the pasted line must be held: {grown} bytes"
        );

        // The line goes away; the capacity must go with it.
        cache.lex(b"ls").expect("scannable");
        let released = retained_capacity(&cache);
        assert!(
            released < grown / 4,
            "the cache kept {released} bytes of a line it no longer holds \
             (it was holding {grown})"
        );
        // The line it does hold must still be there, and correct.
        assert_eq!(cache.tokens().len(), 1);
        let full = lex(b"ls").expect("full scan");
        assert_eq!(cache.tokens(), full.tokens.as_slice());
    }

    #[test]
    fn the_cache_releases_the_capacity_of_a_line_it_no_longer_holds_on_the_incremental_path() {
        // The same, but through the incremental path: the release has to
        // happen *after* the merge, so it cannot be left to `lex` alone.
        let mut cache = LexCache::new();
        let big = pasted_line(16 * 1024);
        cache.lex(big.as_bytes()).expect("scannable");
        let grown = retained_capacity(&cache);

        // Long enough for the incremental path, short enough to be an
        // ordinary command line.
        let small = pasted_line(64);
        assert!(small.len() > INCREMENTAL_MIN_LEN);
        cache.relex(small.as_bytes()).expect("scannable");
        let released = retained_capacity(&cache);
        assert!(
            released < grown / 4,
            "the cache kept {released} bytes of a line it no longer holds \
             (it was holding {grown})"
        );
        let full = lex(small.as_bytes()).expect("full scan");
        assert_eq!(cache.tokens(), full.tokens.as_slice());
    }

    #[test]
    fn the_cache_does_not_reallocate_for_every_keystroke() {
        // The shrink must be rare: a reallocation on the interactive thread
        // is exactly what these buffers exist to avoid.
        //
        // The line is a few hundred bytes on purpose. It is long enough
        // that the *hysteresis* — four times the line — is what protects
        // the buffers, rather than the absolute floor: a policy that
        // released the capacity as soon as it was bigger than the line
        // would keep the capacity of this line changing on every
        // keystroke, and this test would catch it.
        let mut cache = LexCache::new();
        let line = "word ".repeat(120);
        let edited = "word ".repeat(120) + "x";
        cache.lex(line.as_bytes()).expect("scannable");
        // Warm the buffers up: the first re-lex also grows them.
        cache.relex(edited.as_bytes()).expect("scannable");
        let capacity = retained_capacity(&cache);

        for i in 0..1000 {
            let line: &[u8] = if i % 2 == 0 { edited.as_bytes() } else { line.as_bytes() };
            cache.relex(line).expect("scannable");
        }
        assert_eq!(
            retained_capacity(&cache),
            capacity,
            "a short line must not make the cache reallocate"
        );
    }

    #[test]
    fn the_cache_does_not_reallocate_for_every_keystroke_of_a_short_line() {
        // The same, for a line short enough to be re-parsed rather than
        // edited: this is the path almost every keystroke takes.
        let mut cache = LexCache::new();
        let line = "word ".repeat(12);
        let edited = "word ".repeat(12) + "x";
        cache.lex(line.as_bytes()).expect("scannable");
        // Warm the buffers up: the first re-lex also grows them.
        cache.relex(edited.as_bytes()).expect("scannable");
        let capacity = retained_capacity(&cache);

        for i in 0..1000 {
            let line: &[u8] = if i % 2 == 0 { edited.as_bytes() } else { line.as_bytes() };
            cache.relex(line).expect("scannable");
        }
        assert_eq!(
            retained_capacity(&cache),
            capacity,
            "a short line must not make the cache reallocate"
        );
    }

    #[test]
    fn the_cache_keeps_the_capacity_a_line_needs() {
        // The line is still there, so the capacity it needs must be kept:
        // giving it back would mean reallocating on the next keystroke.
        let mut cache = LexCache::new();
        let line = "word ".repeat(120);
        cache.lex(line.as_bytes()).expect("scannable");
        let capacity = retained_capacity(&cache);
        // The same line again: nothing changed, so nothing may be
        // reallocated.
        cache.relex(line.as_bytes()).expect("scannable");
        assert_eq!(retained_capacity(&cache), capacity);
    }
}
