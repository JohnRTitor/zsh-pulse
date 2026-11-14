//! Randomised cross-checks for the incremental re-lexer.
//!
//! The incremental re-lexer is the most bug-prone code in the project: it
//! decides to reuse a tail it has not re-read, and a wrong decision
//! highlights a line as if it were a different one. A single hand-written
//! test cannot cover that space, so these tests generate many edits and
//! check every one of them against a full scan.
//!
//! The generator is a small deterministic PRNG, so a failure is
//! reproducible: the seed is part of the test name.

use crate::{incremental::LexCache, lex};

/// A tiny xorshift generator.
///
/// Deterministic on purpose: a failing case must be reproducible from the
/// seed in the test name.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// The alphabet the generator draws from.
///
/// It mixes the characters that make the lexer interesting: quotes, dollars,
/// backslashes, redirections, operators and blanks.
const ALPHABET: &[u8] = b"ab cd'\"`$\\<>|&;(){}=*?#~!-\n\t0123456789";

/// A random line, built to be *mostly* lexable.
fn random_line(rng: &mut Rng, len: usize) -> String {
    let mut s = String::with_capacity(len);
    for _ in 0..len {
        s.push(ALPHABET[rng.below(ALPHABET.len())] as char);
    }
    s
}

/// A random edit: an insertion, a deletion or a replacement.
fn random_edit(rng: &mut Rng, line: &mut String) {
    if line.is_empty() {
        line.push(ALPHABET[rng.below(ALPHABET.len())] as char);
        return;
    }
    let at = rng.below(line.len());
    match rng.below(3) {
        0 => {
            let c = ALPHABET[rng.below(ALPHABET.len())] as char;
            line.insert(at, c);
        }
        1 => {
            line.remove(at);
        }
        _ => {
            let c = ALPHABET[rng.below(ALPHABET.len())] as char;
            line.replace_range(at..at + 1, &c.to_string());
        }
    }
}

/// Check that an incremental re-lex agrees with a full scan.
///
/// The cache is **carried over** from call to call: the module keeps one
/// cache for the whole editing session, so the production path is always
/// "re-lex from an incrementally updated cache". A test that re-lexed from
/// the original line every time would miss exactly the bugs that matter.
fn assert_agrees(cache: &mut LexCache, old: &str, new: &str) {
    let full = match lex(new.as_bytes()) {
        Ok(f) => f,
        Err(_) => return,
    };
    // The raw incremental path: the policy in `LexCache::relex` re-parses
    // short lines, and the incremental machinery must be tested on its own.
    let outcome = match cache.relex_incremental(new.as_bytes()) {
        Ok(o) => o,
        Err(e) => panic!("relex of {new:?} failed after {old:?}: {e}"),
    };
    assert_eq!(
        cache.tokens(),
        full.tokens.as_slice(),
        "incremental result differs from a full scan for {old:?} -> {new:?} \
         (outcome: {outcome:?})"
    );
    check_resume_points_are_consistent(cache, new);

    // The final state and the resume points must agree with a full scan
    // too, not just the tokens: the splice reuses the old tail, so the
    // state at the end of the line is the old one.
    assert_eq!(
        cache.final_state(),
        full.final_state,
        "the final state differs from a full scan for {old:?} -> {new:?}"
    );
    assert_eq!(
        cache.resume_points().len(),
        full.resume_points.len(),
        "the number of resume points differs from a full scan for {old:?} -> {new:?}"
    );
    for (a, b) in cache.resume_points().iter().zip(full.resume_points.iter()) {
        assert_eq!(
            (a.byte_offset, a.state, a.guard),
            (b.byte_offset, b.state, b.guard),
            "a resume point differs from a full scan for {old:?} -> {new:?}"
        );
    }
}

/// Every resume point must describe a real token, and its guard must point
/// at the character that decided where the unit starts.
///
/// A resume point says "the parse state at this offset is such and such".
/// If its token index and its byte offset disagree, the state belongs to a
/// different token, and resuming from it would produce the wrong parse.
///
/// The guard is the offset of the character that ended the previous unit.
/// Two things follow from it: only blanks may lie between the guard and the
/// unit's start, and the guard must not be past the unit's start. A guard
/// that is too small would let a unit whose deciding character was edited
/// be used as a restart point — which produces a *wrong* highlight rather
/// than a missing one.
fn check_resume_points_are_consistent(cache: &LexCache, text: &str) {
    let bytes = text.as_bytes();
    for rp in cache.resume_points() {
        let token = cache
            .tokens()
            .get(rp.token_index as usize)
            .unwrap_or_else(|| panic!("resume point {rp:?} has no token"));
        assert_eq!(
            token.start, rp.byte_offset,
            "resume point {rp:?} does not match its token {token:?}"
        );
        let guard = rp.guard as usize;
        let offset = rp.byte_offset as usize;
        assert!(
            guard <= offset,
            "resume point {rp:?} has a guard past its own offset"
        );
        assert!(
            offset <= bytes.len(),
            "resume point {rp:?} is past the end of {text:?}"
        );
        assert!(
            bytes[guard..offset]
                .iter()
                .all(|b| *b == b' ' || *b == b'\t'),
            "resume point {rp:?} has a guard that is not the end of the \
             previous unit: {text:?}"
        );
    }
}

#[test]
fn random_edits_always_agree_with_a_full_scan() {
    let mut rng = Rng(0x5eed_1234_9abc_def0);
    for _round in 0..2000 {
        let n = 1 + rng.below(40);
        let mut line = random_line(&mut rng, n);
        let mut cache = LexCache::new();
        if cache.lex(line.as_bytes()).is_err() {
            continue;
        }
        // Apply a few edits in a row, checking each one. The cache is
        // carried over: this is the production path.
        for _ in 0..3 {
            let previous = line.clone();
            random_edit(&mut rng, &mut line);
            assert_agrees(&mut cache, &previous, &line);
        }
    }
}

#[test]
fn random_lines_always_agree_with_a_full_scan() {
    let mut rng = Rng(0xfeed_0f00_1122_3344);
    for _ in 0..2000 {
        let n = 1 + rng.below(60);
        let line = random_line(&mut rng, n);
        let mut cache = LexCache::new();
        if cache.lex(line.as_bytes()).is_err() {
            continue;
        }
        let full = lex(line.as_bytes()).expect("the cache accepted it, so a full scan must too");
        assert_eq!(cache.tokens(), full.tokens.as_slice(), "for {line:?}");
        check_resume_points_are_consistent(&cache, &line);
    }
}

#[test]
fn typing_a_whole_line_one_character_at_a_time_always_agrees() {
    let mut rng = Rng(0x0bad_c0de_1234_5678);
    for _ in 0..50 {
        let n = 1 + rng.below(50);
        let target = random_line(&mut rng, n);
        let mut cache = LexCache::new();
        let mut typed = String::new();
        for c in target.chars() {
            typed.push(c);
            if cache.lex(typed.as_bytes()).is_err() {
                // The line cannot be scanned at all; start over from it.
                break;
            }
            let previous = typed[..typed.len() - c.len_utf8()].to_string();
            let full = match lex(typed.as_bytes()) {
                Ok(f) => f,
                Err(_) => break,
            };
            // Re-lex from the *previous* line, the way the module does.
            let mut incremental = LexCache::new();
            incremental
                .lex(previous.as_bytes())
                .expect("the previous line scans");
            incremental
                .relex_incremental(typed.as_bytes())
                .expect("the typed line scans");
            assert_eq!(
                incremental.tokens(),
                full.tokens.as_slice(),
                "while typing {typed:?}"
            );
        }
    }
}

#[test]
fn deleting_characters_one_at_a_time_always_agrees() {
    let mut rng = Rng(0xaaaa_bbbb_cccc_dddd);
    for _ in 0..50 {
        let n = 1 + rng.below(50);
        let mut line = random_line(&mut rng, n);
        let original = line.clone();
        let mut cache = LexCache::new();
        if cache.lex(original.as_bytes()).is_err() {
            continue;
        }
        while !line.is_empty() {
            let at = rng.below(line.len());
            line.remove(at);
            let full = match lex(line.as_bytes()) {
                Ok(f) => f,
                Err(_) => break,
            };
            cache.relex_incremental(line.as_bytes()).expect("relex");
            assert_eq!(
                cache.tokens(),
                full.tokens.as_slice(),
                "after deleting from {original:?}"
            );
            check_resume_points_are_consistent(&cache, &line);
        }
    }
}
