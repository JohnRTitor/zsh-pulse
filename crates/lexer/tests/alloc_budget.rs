//! The allocation budget on the keystroke path.
//!
//! # Why this is measured
//!
//! An allocation on the interactive thread is a malloc between the user's
//! keystroke and the redraw, and it is the reason the first implementation
//! of this project was ten times slower than it should have been. The plan
//! therefore requires an explicit allocation count on the hot path, not a
//! hope that the code happens not to allocate.
//!
//! # How it is measured
//!
//! A counting global allocator wraps the system allocator and counts every
//! call. The count is taken around the operation under test, so the test
//! harness's own allocations do not matter.
//!
//! The budget is a *bound*, not zero: a few allocations per keystroke are
//! acceptable, one per token is not.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use lexer::LexCache;

/// A global allocator that counts.
struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // Safety: the layout is forwarded unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // Safety: the pointer and layout came from `alloc`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // Safety: forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The number of allocations since the process started.
fn allocations() -> usize {
    ALLOCATIONS.load(Ordering::Relaxed)
}

/// The budget for one re-lex of a typical line.
///
/// The re-lex builds a new token list and a new resume-point list, and merges
/// them with the old ones. Four buffers are involved, so a handful of
/// allocations is the floor for the current design. The number is a bound,
/// not a target: a regression that allocates per *token* would blow past it
/// immediately.
const BUDGET: usize = 8;

/// Serialises the tests in this file.
///
/// The counting allocator is process-wide, so two tests measuring it at the
/// same time would see each other's allocations.
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn a_steady_state_relex_stays_within_the_allocation_budget() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut cache = LexCache::new();
    let original = b"git commit --amend --no-edit";
    cache.lex(original).expect("scannable");

    // Warm the cache up: the first re-lex also grows the buffers.
    cache.relex(b"git commit --amend --no-edit!").expect("scannable");

    let before = allocations();
    cache.relex(b"git commit --amend --no-edit!!").expect("scannable");
    let used = allocations() - before;

    println!("a re-lex of a 28-byte line allocated {used} times (budget {BUDGET})");
    assert!(
        used <= BUDGET,
        "a re-lex allocated {used} times, more than the budget of {BUDGET}"
    );
}

#[test]
fn the_allocation_count_does_not_grow_with_the_length_of_the_line() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // The important property: the cost must not be proportional to the
    // number of tokens. A line of a hundred words must not allocate a
    // hundred times more than a line of ten.
    let short: String = "word ".repeat(10);
    let long: String = "word ".repeat(100);

    let mut cache = LexCache::new();
    cache.lex(short.as_bytes()).expect("scannable");
    cache.relex(format!("{short}x").as_bytes()).expect("scannable");
    let before = allocations();
    cache.relex(format!("{short}xx").as_bytes()).expect("scannable");
    let short_used = allocations() - before;

    let mut cache = LexCache::new();
    cache.lex(long.as_bytes()).expect("scannable");
    cache.relex(format!("{long}x").as_bytes()).expect("scannable");
    let before = allocations();
    cache.relex(format!("{long}xx").as_bytes()).expect("scannable");
    let long_used = allocations() - before;

    assert!(
        long_used <= short_used + 2,
        "a ten-times longer line allocated {long_used} times against {short_used}: \\
         the cost must not grow with the length of the line"
    );
}
