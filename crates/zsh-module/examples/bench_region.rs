//! The cost of converting styled spans into `region_highlight` entries.
//!
//! # Why this is measured on its own
//!
//! The plan asks for the conversion cost to be published as its own
//! microbenchmark, because it is the part of the keystroke path that is
//! *ours*: reading the line and re-lexing it are the lexer's business, and
//! handing the result to zsh is zsh's. What is left in between — turning
//! byte offsets into character offsets and building the entry strings — is
//! the part this benchmark measures.
//!
//! # What it must show
//!
//! The conversion runs on the interactive thread, so it has to be cheap and
//! it has to stop allocating once it is warm. The benchmark therefore
//! reports two things: the time per conversion, and the number of
//! allocations it makes.
//!
//! The spans it converts are the ones the module really publishes — it
//! asks the module for them, with the answers the shell would give — so
//! the number of regions reported here is the number of regions the user's
//! line really gets. The paths are not looked up, because that needs a
//! shell; a word that names a file is styled as the lexer styled it.
//!
//! ```text
//! cargo run --release -p zsh-module --example bench_region
//! ```

use std::time::Instant;

use {
    highlight::{StyleId, char_offsets_into},
    lexer::lex,
    zshpulse_core::{published_spans, region},
};

/// A command line with a realistic number of styled regions.
fn line() -> String {
    let mut s = String::from("git commit --amend --no-edit");
    for i in 0..8 {
        s.push_str(&format!(" 'quoted argument {i}'"));
    }
    s
}

fn main() {
    let text = line();
    let bytes = text.as_bytes();
    let result = lex(bytes).expect("the line scans");
    // The spans the module publishes: the shell knows `git`, and the
    // options and the quoted arguments are styled by the lexer.
    let spans = published_spans(bytes, &[b"--command-known=git"]);
    let styled: Vec<_> = spans
        .iter()
        .copied()
        .filter(|s| region::attribute(s.style).is_some())
        .collect();
    println!("line:          {} bytes", text.len());
    println!(
        "tokens:        {} ({} spans, {} styled)",
        result.tokens.len(),
        spans.len(),
        styled.len()
    );
    println!();

    // --- the character-offset map ---------------------------------------
    let mut offsets = Vec::new();
    let rounds = 1000;
    let start = Instant::now();
    for _ in 0..rounds {
        char_offsets_into(&mut offsets, bytes);
    }
    let per_map = start.elapsed().as_secs_f64() / f64::from(rounds);
    println!(
        "byte -> character map:   {:>8.3} us  ({} entries)",
        per_map * 1e6,
        offsets.len()
    );

    // --- the entry construction -----------------------------------------
    let mut entries = region::Entries::default();
    let start = Instant::now();
    for _ in 0..rounds {
        entries.build(&styled, Some(&offsets));
    }
    let per_entries = start.elapsed().as_secs_f64() / f64::from(rounds);
    println!(
        "entry construction:       {:>8.3} us  ({} entries)",
        per_entries * 1e6,
        entries.len()
    );

    // --- the whole conversion -------------------------------------------
    let start = Instant::now();
    for _ in 0..rounds {
        char_offsets_into(&mut offsets, bytes);
        entries.build(&styled, Some(&offsets));
    }
    let per_total = start.elapsed().as_secs_f64() / f64::from(rounds);
    println!("the whole conversion:     {:>8.3} us", per_total * 1e6);
    println!();

    // The entries must be what zsh's parser expects.
    let rows = entries.rows();
    let first = rows.first().map(|r| r.to_string_lossy().into_owned());
    println!("first entry:  {first:?}");
    println!(
        "last entry:   {:?}",
        rows.last().map(|r| r.to_string_lossy().into_owned())
    );
    assert!(
        rows.iter()
            .all(|r| r.to_bytes().ends_with(b"memo=zsh-pulse")),
        "every entry must carry the memo so another plugin can tell it apart"
    );
    assert!(
        !styled.is_empty(),
        "the line must produce at least one styled region, or this benchmark \\
         measures nothing"
    );
    // A style with no attribute must not be published at all.
    let plain = vec![highlight::HighlightSpan::new(0, 1, StyleId::Argument)];
    let mut plain_entries = region::Entries::default();
    plain_entries.build(&plain, Some(&offsets));
    assert!(
        plain_entries.is_empty(),
        "an ordinary argument must not be published"
    );

    println!();
    println!(
        "OK: the conversion is {:.3} us for a {}-byte line with {} regions",
        per_total * 1e6,
        text.len(),
        styled.len()
    );
}
