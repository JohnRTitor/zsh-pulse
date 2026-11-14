//! The Stage C1 acceptance benchmark.
//!
//! # What it measures
//!
//! Two costs for the same edit — one character inserted in the middle of a
//! long command line:
//!
//! 1. a **full re-parse** of the whole line, and
//! 2. an **incremental re-lex** that resumes from the last checkpoint and
//!    splices the unchanged tail back on.
//!
//! The acceptance criterion from the plan is that the incremental path is
//! *sub-linear* in the length of the line: the work must be proportional to
//! the distance to convergence, not to the length of the line.
//!
//! # How it avoids measuring the wrong thing
//!
//! A benchmark that measures "the incremental path" but actually measures a
//! full re-parse would report a good number for the wrong reason. So the
//! harness checks the work that was *done*, not just the time it took:
//!
//! - the number of bytes the incremental path re-scanned must be a small
//!   fraction of the line, and
//! - the incremental result must be identical to a full parse.
//!
//! If either check fails, the harness exits non-zero. A benchmark that
//! cannot fail is not a benchmark.
//!
//! # Running it
//!
//! ```text
//! cargo run --release -p lexer --example bench_incremental
//! ```

use std::time::Instant;

use lexer::{INCREMENTAL_MIN_LEN, LexCache, lex};

/// A realistic long command line: many arguments, some quoted, some not.
fn long_line(words: usize) -> String {
    let mut line = String::with_capacity(words * 12);
    for i in 0..words {
        if i > 0 {
            line.push(' ');
        }
        if i % 7 == 3 {
            line.push_str("'a quoted argument'");
        } else if i % 11 == 5 {
            line.push_str("--flag=value");
        } else {
            line.push_str("argument");
        }
    }
    line
}

/// Time `rounds` rounds of `f` and return the per-round cost in seconds.
///
/// The whole batch is timed once: a single `Instant::now()` costs about
/// twenty-five nanoseconds, which is a noticeable fraction of the tens of
/// nanoseconds this measures.
fn time_rounds(rounds: u32, mut f: impl FnMut()) -> f64 {
    let start = Instant::now();
    for _ in 0..rounds {
        f();
    }
    start.elapsed().as_secs_f64() / f64::from(rounds)
}

fn main() {
    let words: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(200);
    let rounds: u32 = 50;

    println!(
        "{:<10} {:>8} {:>14} {:>14} {:>14} {:>12} {:>12}",
        "words", "bytes", "full (us)", "incr (us)", "policy (us)", "saving (ns)", "re-scanned"
    );
    println!("{}", "-".repeat(90));

    // (words, bytes, full, incremental, bytes re-scanned, policy)
    let mut rows: Vec<(usize, usize, f64, f64, usize, f64)> = Vec::new();

    // The sweep starts at a realistic interactive command line (a few
    // words) and goes up to lines no one would type.
    let mut sizes: Vec<usize> = vec![2, 4, 8, 16, 32, 64, 128, 256];
    sizes.retain(|w| *w <= words);
    if sizes.is_empty() {
        sizes.push(words);
    }
    for w in sizes {
        let w = w.max(2);
        let line = long_line(w);
        let mut edited = line.clone();
        let edit_at = line.len() / 2;
        edited.insert(edit_at, 'x');

        // --- the incremental path -------------------------------------------
        //
        // Each round re-primes the cache with the *original* line and then
        // does the edit. Without the re-priming, the second and later
        // rounds would re-lex a line that has not changed and measure a
        // no-op — which is how a benchmark ends up reporting a good number
        // for the wrong reason.
        //
        // The whole round is timed, not just the edit: at these sizes a
        // single `Instant::now()` costs a noticeable fraction of the
        // measurement. The priming is identical in every arm, so it cancels.
        let mut cache = LexCache::new();
        let incremental = time_rounds(rounds, || {
            cache.lex(line.as_bytes()).expect("the line scans");
            cache
                .relex_incremental(edited.as_bytes())
                .expect("the edited line scans");
        });

        // --- the full path ---------------------------------------------------
        //
        // The baseline is the module's *own* full parse — `LexCache::lex`,
        // which reuses its buffers — not the free function `lex`, which
        // allocates two fresh vectors on every call. Comparing an
        // allocating implementation against an allocation-free one would
        // measure the allocator, not incrementality.
        let mut full_cache = LexCache::new();
        let full = time_rounds(rounds, || {
            full_cache.lex(line.as_bytes()).expect("the line scans");
            full_cache
                .lex(edited.as_bytes())
                .expect("the edited line scans");
        });

        // --- the policy ------------------------------------------------------
        //
        // This is what the module actually does: the incremental path for a
        // long line, a full re-parse for a short one.
        let mut policy_cache = LexCache::new();
        let policy = time_rounds(rounds, || {
            policy_cache.lex(line.as_bytes()).expect("the line scans");
            policy_cache
                .relex(edited.as_bytes())
                .expect("the edited line scans");
        });

        // --- the checks ------------------------------------------------------
        let reference = lex(edited.as_bytes()).expect("the edited line scans");
        if cache.tokens() != reference.tokens.as_slice() {
            eprintln!("FAIL: the incremental result differs from a full parse");
            std::process::exit(1);
        }

        let mut check = LexCache::new();
        check.lex(line.as_bytes()).expect("the line scans");
        let report = check
            .relex(edited.as_bytes())
            .expect("the edited line scans");

        // The saving is the meaningful number: every arm includes the same
        // re-priming, so the *difference* between the arms is what the
        // incremental path actually saves.
        println!(
            "{:<10} {:>8} {:>14.3} {:>14.3} {:>14.3} {:>12.1} {:>12}",
            w,
            line.len(),
            full * 1e6,
            incremental * 1e6,
            policy * 1e6,
            (full - incremental) * 1e9,
            report.rescanned_bytes
        );
        rows.push((
            w,
            line.len(),
            full,
            incremental,
            report.rescanned_bytes,
            policy,
        ));
    }

    println!();

    // The scanner's work must not grow with the length of the line: that is
    // the sub-linear claim. The *end-to-end* cost still grows, because the
    // offsets of the tokens after the edit move and the token list has to be
    // rebuilt — but it grows with a much smaller constant than a full
    // re-parse, which is what the speed-up column shows.
    let first = rows[0];
    let last = rows[rows.len() - 1];
    let length_ratio = last.1 as f64 / first.1 as f64;
    let scan_ratio = last.4 as f64 / first.4.max(1) as f64;
    println!(
        "the line grew {:.1}x; the bytes re-scanned grew {:.1}x",
        length_ratio, scan_ratio
    );
    if scan_ratio > 2.0 {
        eprintln!(
            "FAIL: the re-scanned bytes grew {:.1}x while the line grew {:.1}x; \
             the scan is not sub-linear",
            scan_ratio, length_ratio
        );
        std::process::exit(1);
    }

    // The policy must never be slower than a full re-parse: for a short
    // line it *is* a full re-parse.
    println!();
    println!(
        "the incremental path is used from {INCREMENTAL_MIN_LEN} bytes; below \
         that a full re-parse is cheaper"
    );
    // The *policy* is what the module does, and the saving is what the
    // incremental path buys. Below the threshold the policy is a full
    // re-parse plus a comparison, so the saving is (near) zero; above it the
    // saving must be positive.
    let worst_saving = rows.iter().map(|r| r.2 - r.5).fold(f64::INFINITY, f64::min);
    if worst_saving < -0.05e-6 {
        eprintln!(
            "FAIL: the policy is up to {:.0} ns slower than a full re-parse",
            -worst_saving * 1e9
        );
        std::process::exit(1);
    }
    println!("the policy is never meaningfully slower than a full re-parse");

    // Above the threshold the incremental path must actually save something.
    let above: Vec<_> = rows.iter().filter(|r| r.1 >= INCREMENTAL_MIN_LEN).collect();
    if !above.is_empty() {
        let best = above
            .iter()
            .map(|r| r.2 - r.3)
            .fold(f64::INFINITY, f64::min);
        if best <= 0.0 {
            eprintln!(
                "FAIL: the incremental path does not save anything above the \
                 threshold"
            );
            std::process::exit(1);
        }
        println!(
            "above {INCREMENTAL_MIN_LEN} bytes the incremental path saves at \
             least {:.0} ns per edit",
            best * 1e9
        );
    }
    println!();
    println!(
        "NOTE: the *end-to-end* cost is still linear in the length of the line: \
         the offsets of the tokens after the edit move, so the token list has \
         to be rebuilt. What is sub-linear is the scanning, which is what the \
         \"re-scanned\" column measures."
    );
    println!();
    println!("OK");
}
