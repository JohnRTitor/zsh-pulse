//! Panic containment at the FFI boundary.
//!
//! # Why this matters
//!
//! zsh calls our entry points through plain C function pointers. A Rust panic
//! must never propagate across that boundary: unwinding into zsh's C frames is
//! undefined behaviour, and in practice it aborts the user's interactive shell.
//!
//! Every entry point therefore runs inside [`catch_unwind`]. A caught panic is
//! reported into a sticky "degraded" flag and the entry point returns a benign
//! value, so the shell stays usable and the user gets a diagnosable message
//! rather than a lost session.
//!
//! Note this catches *unwinding* panics. The workspace is built with
//! `panic = "unwind"` (see the root `Cargo.toml`) precisely because abort-on-
//! panic would make this mechanism ineffective.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::{AtomicBool, Ordering},
};

static DEGRADED: AtomicBool = AtomicBool::new(false);

/// Whether a panic has been caught at the boundary since load.
pub fn is_degraded() -> bool {
    DEGRADED.load(Ordering::Relaxed)
}

/// Clear the degraded flag (used by tests and by an explicit reload).
pub fn reset_degraded() {
    DEGRADED.store(false, Ordering::Relaxed);
}

/// Record that the engine has failed and must not be trusted again.
pub fn mark_degraded() {
    DEGRADED.store(true, Ordering::Relaxed);
}

/// Run `f`, converting any panic into `fallback` and marking the engine
/// degraded.
///
/// The fallback is returned only if `f` panicked; on success `f`'s value is
/// returned untouched.
///
/// # Panics
///
/// Does not panic: panics from `f` are caught.
pub fn guard<T, F>(fallback: T, f: F) -> T
where
    F: FnOnce() -> T,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            mark_degraded();
            fallback
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the tests in this module.
    ///
    /// `DEGRADED` is a process-wide flag, so parallel tests would otherwise
    /// clobber each other's state — a panic test setting it while another
    /// asserts it is clear.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serialise() -> std::sync::MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn panic_is_caught_and_fallback_returned() {
        let _g = serialise();
        reset_degraded();
        let r = guard(-1, || panic!("boom"));
        assert_eq!(r, -1);
        assert!(
            is_degraded(),
            "a caught panic must mark the engine degraded"
        );
    }

    #[test]
    fn success_passes_value_through_untouched() {
        let _g = serialise();
        reset_degraded();
        assert_eq!(guard(-1, || 42), 42);
        assert!(!is_degraded());
    }

    #[test]
    fn repeated_panics_stay_contained() {
        let _g = serialise();
        reset_degraded();
        for _ in 0..1000 {
            let _ = guard(0u32, || panic!("repeat"));
        }
        assert!(is_degraded());
    }
}
