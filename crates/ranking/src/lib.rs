//! Fuzzy ranking and scoring for completion candidates.
//!
//! # The algorithm
//!
//! Based on `fzf`'s scoring constants (`research/upstream/LESSONS.md` §Borrow 9,
//! citing `fzf/src/algo/algo.go:112-146`), implemented in pure Rust without
//! allocations on the hot scoring path.
//!
//! Top-K selection uses `select_nth_unstable` (Borrow §8, citing `atuin`),
//! partitioning candidates in $O(N)$ time before sorting only the top $K$.

#![forbid(unsafe_code)]

/// `fzf` matching constants.
pub const SCORE_MATCH: i32 = 16;
pub const SCORE_GAP_START: i32 = -3;
pub const SCORE_GAP_EXTENSION: i32 = -1;
pub const BONUS_BOUNDARY_WHITE: i32 = 10;
pub const BONUS_BOUNDARY_DELIMITER: i32 = 9;
pub const BONUS_CAMEL_123: i32 = 7;
pub const BONUS_CONSECUTIVE: i32 = 8;
pub const BONUS_FIRST_CHAR_MULTIPLIER: i32 = 2;

/// Character classification for boundary bonuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Whitespace,
    Delimiter,
    Lower,
    Upper,
    Digit,
    Other,
}

impl CharClass {
    fn of(b: u8) -> Self {
        match b {
            b' ' | b'\t' | b'\n' | b'\r' => Self::Whitespace,
            b'/' | b',' | b':' | b';' | b'.' | b'_' | b'-' | b'\\' => Self::Delimiter,
            b'a'..=b'z' => Self::Lower,
            b'A'..=b'Z' => Self::Upper,
            b'0'..=b'9' => Self::Digit,
            _ => Self::Other,
        }
    }
}

/// Calculate the bonus for matching a character at `idx` in `candidate`.
fn match_bonus(prev_class: CharClass, curr_class: CharClass) -> i32 {
    match prev_class {
        CharClass::Whitespace => BONUS_BOUNDARY_WHITE,
        CharClass::Delimiter => BONUS_BOUNDARY_DELIMITER,
        CharClass::Lower if curr_class == CharClass::Upper => BONUS_CAMEL_123,
        CharClass::Other if curr_class == CharClass::Digit => BONUS_CAMEL_123,
        _ => 0,
    }
}

/// Whether the pattern is all lowercase (smart case rule).
fn is_all_lowercase(pattern: &[u8]) -> bool {
    pattern.iter().all(|&b| !b.is_ascii_uppercase())
}

/// Compute fuzzy match score for `candidate` against `pattern`.
///
/// Returns `None` if `pattern` is not a subsequence of `candidate`.
/// Higher score means a better match.
pub fn fuzzy_match(pattern: &[u8], candidate: &[u8]) -> Option<i32> {
    if pattern.is_empty() {
        return Some(0);
    }
    if candidate.len() < pattern.len() {
        return None;
    }

    let smart_case = is_all_lowercase(pattern);

    // Fast reject: verify all pattern bytes occur in order.
    let mut p_idx = 0;
    for &b in candidate {
        let pb = pattern[p_idx];
        let matches = if smart_case {
            b.to_ascii_lowercase() == pb
        } else {
            b == pb
        };
        if matches {
            p_idx += 1;
            if p_idx == pattern.len() {
                break;
            }
        }
    }
    if p_idx < pattern.len() {
        return None;
    }

    // Exact prefix match fast-path:
    // If candidate starts with pattern, give maximum priority.
    let is_prefix = if smart_case {
        candidate.starts_with(pattern)
            || (candidate.len() >= pattern.len()
                && candidate[..pattern.len()]
                    .iter()
                    .zip(pattern.iter())
                    .all(|(&c, &p)| c.to_ascii_lowercase() == p))
    } else {
        candidate.starts_with(pattern)
    };

    let mut score = 0i32;
    let mut consecutive = 0i32;
    let mut prev_idx: Option<usize> = None;
    let mut cand_idx = 0;

    for &p_char in pattern {
        let mut found = false;
        while cand_idx < candidate.len() {
            let c_char = candidate[cand_idx];
            let matches = if smart_case {
                c_char.to_ascii_lowercase() == p_char
            } else {
                c_char == p_char
            };

            if matches {
                found = true;
                let prev_class = if cand_idx == 0 {
                    CharClass::Whitespace
                } else {
                    CharClass::of(candidate[cand_idx - 1])
                };
                let curr_class = CharClass::of(c_char);
                let bonus = match_bonus(prev_class, curr_class);

                score += SCORE_MATCH;

                if let Some(prev) = prev_idx {
                    let gap = (cand_idx - prev).saturating_sub(1);
                    if gap == 0 {
                        consecutive += 1;
                        score += BONUS_CONSECUTIVE * consecutive;
                    } else {
                        consecutive = 0;
                        score += SCORE_GAP_START + (gap as i32 - 1) * SCORE_GAP_EXTENSION;
                    }
                } else {
                    // First character match
                    if cand_idx == 0 {
                        score += bonus * BONUS_FIRST_CHAR_MULTIPLIER;
                    } else {
                        score += bonus;
                    }
                }

                prev_idx = Some(cand_idx);
                cand_idx += 1;
                break;
            }
            cand_idx += 1;
        }

        if !found {
            return None;
        }
    }

    if is_prefix {
        score += 100;
    }

    // Exact equal boost
    if pattern.len() == candidate.len() {
        score += 200;
    }

    // Penalize excess candidate length
    let excess = candidate.len() - pattern.len();
    score = score.saturating_sub(excess as i32);

    Some(score)
}

/// A ranked candidate entry with its index and score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScoredCandidate {
    /// Index in the original input candidate slice.
    pub index: usize,
    /// Matching score (higher is better).
    pub score: i32,
}

/// Rank candidates against `pattern` and return the top `k` items.
///
/// If candidates count exceeds `k`, uses `select_nth_unstable_by` to
/// partition in $O(N)$ time before sorting the top `k`.
pub fn rank_top_k<'a, F>(
    candidate_count: usize,
    mut get_candidate: F,
    pattern: &[u8],
    k: usize,
) -> Vec<ScoredCandidate>
where
    F: FnMut(usize) -> &'a [u8],
{
    if candidate_count == 0 || k == 0 {
        return Vec::new();
    }

    let mut scored: Vec<ScoredCandidate> = Vec::with_capacity(candidate_count.min(1024));

    for i in 0..candidate_count {
        let text = get_candidate(i);
        if let Some(score) = fuzzy_match(pattern, text) {
            scored.push(ScoredCandidate { index: i, score });
        }
    }

    if scored.is_empty() {
        return Vec::new();
    }

    let target_k = k.min(scored.len());
    if scored.len() > target_k {
        // Partition so that the top `target_k` elements are at 0..target_k
        // Sort descending by score.
        scored.select_nth_unstable_by(target_k - 1, |a, b| b.score.cmp(&a.score));
        scored.truncate(target_k);
    }

    // Sort only the top K elements
    scored.sort_unstable_by_key(|a| std::cmp::Reverse(a.score));
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pattern_matches_everything() {
        assert_eq!(fuzzy_match(b"", b"anything"), Some(0));
    }

    #[test]
    fn exact_match_has_high_score() {
        let score_exact = fuzzy_match(b"git", b"git").expect("matches");
        let score_partial = fuzzy_match(b"git", b"github").expect("matches");
        assert!(score_exact > score_partial);
    }

    #[test]
    fn prefix_match_scores_higher_than_mid_string() {
        let score_prefix = fuzzy_match(b"log", b"log_output").expect("matches");
        let score_mid = fuzzy_match(b"log", b"dialog_box").expect("matches");
        assert!(score_prefix > score_mid);
    }

    #[test]
    fn smart_case_matching() {
        assert!(fuzzy_match(b"cargo", b"Cargo.toml").is_some());
        assert!(fuzzy_match(b"Cargo", b"cargo").is_none());
    }

    #[test]
    fn non_matching_returns_none() {
        assert_eq!(fuzzy_match(b"xyz", b"abcde"), None);
    }

    #[test]
    fn boundary_bonuses_rank_word_boundaries_higher() {
        let boundary = fuzzy_match(b"gc", b"git-commit").expect("matches");
        let middle = fuzzy_match(b"gc", b"logical").expect("matches");
        assert!(boundary > middle);
    }

    #[test]
    fn top_k_ranking_returns_best_sorted() {
        let items: &[&[u8]] = &[
            b"arbitrary_string",
            b"checkout",
            b"check",
            b"che",
        ];
        let ranked = rank_top_k(items.len(), |i| items[i], b"che", 2);
        assert_eq!(ranked.len(), 2);
        assert_eq!(items[ranked[0].index], b"che"); // exact match first
        assert_eq!(items[ranked[1].index], b"check"); // prefix next
    }
}
