//! The zsh boundary for `shell-pulse`.
//!
//! This crate contains **all** unsafe code in the project. Everything above it
//! (`engine`, and later `lexer`, `highlight`, `ranking`) is plain safe Rust.
//!
//! It is deliberately dependency-free and small enough to audit by reading.
//!
//! # The three rules
//!
//! 1. **Probe before use.** [`probe::probe`] resolves every zsh symbol during
//!    module load. A missing symbol is a clean load failure, never a later crash.
//! 2. **Never unwind into zsh.** Every entry point runs inside [`guard::guard`].
//! 3. **Never trust a layout you have not measured.** The `raw` module's layout
//!    tests assert the exact sizes and offsets against real zsh.
//!
//! # Verified facts this crate encodes
//!
//! - Module entry points are `setup_`, `features_`, `enables_`, `boot_`,
//!   `cleanup_`, `finish_` (`zsh-src/Src/module.c:1554-1565`). On builds with
//!   `DYNAMIC_NAME_CLASH_OK` the lookup is *unmangled*
//!   (`zsh-src/Src/module.c:1774-1777`).
//! - `getsparam()` returns `char *`, not `char **`
//!   (`zsh-src/Src/params.c:3037-3045`). Getting this wrong segfaults zsh.
//! - `addbuiltins()` takes three arguments: name, array, size
//!   (`zsh-src/Src/module.c:543`).
//! - `zmodload` rejects absolute paths and `.so` suffixes
//!   (`zsh-src/Src/module.c:2177-2186`).

pub mod api;
pub mod guard;
pub mod probe;
pub mod raw;

pub use guard::{guard, is_degraded};
pub use probe::{Capabilities, REQUIRED_SYMBOLS};
pub use api::Api;
pub use raw::{Builtin, CStr, HashNode, Module, Options};

/// Name this module registers its builtins under.
pub const BUILTIN_PREFIX: &str = "shell-pulse-";

/// The builtin name registered inside the `shell-pulse-` namespace.
///
/// C string literals (`c"…"`) are NUL-terminated; `&str` is not. Getting this
/// wrong corrupts zsh's builtin hash table.
pub const BUILTIN_NAME: &core::ffi::CStr = c"highlight";

/// Build the `zmodload` path used by the zsh adapter.
///
/// zsh requires slash-separated identifiers with no dots, so this yields
/// `shellpulse/highlight`, which resolves to `shellpulse/highlight.so` on
/// `$module_path`.
pub const fn zmodload_name() -> &'static str {
    "shellpulse/highlight"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zmodload_name_is_a_legal_zsh_module_name() {
        let n = zmodload_name();
        // modname_ok() (Src/module.c:2177-2186) accepts only IIDENT segments
        // separated by '/': no dots, no leading '/', no trailing '/'.
        assert!(!n.contains('.'));
        assert!(!n.starts_with('/'));
        assert!(!n.ends_with('/'));
        assert!(n.split('/').all(|seg| {
            !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        }));
    }

    #[test]
    fn builtin_prefix_has_no_illegal_characters() {
        assert!(BUILTIN_PREFIX.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    }
}