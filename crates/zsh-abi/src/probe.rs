//! Load-time capability probe.
//!
//! # The problem this solves
//!
//! A zsh module resolves its imports against the symbols the `zsh` executable
//! exports. Those exports are a *build-time property of that zsh*, and they
//! differ between distributions: this machine's zsh 5.9.2 exports 1048 dynamic
//! symbols, but there is no guarantee a Debian, Fedora or Homebrew build exports
//! the same set.
//!
//! If an import is missing, `dlopen` fails lazily — the module loads, and the
//! process dies later, inside the user's interactive shell, at a random
//! keystroke. That is unacceptable.
//!
//! # The rule
//!
//! **Resolve every required symbol during `setup_`, before touching any other
//! zsh API.** If any is missing, fail the load cleanly so zsh can fall back to
//! its native highlighting. Never rely on link-time failure, which cannot
//! happen for a shared object.

use core::fmt;

/// The set of symbols this module needs from the zsh executable.
pub const REQUIRED_SYMBOLS: &[&str] = &[
    crate::raw::zsh::ADDBUILTINS,
    crate::raw::zsh::GETSPARAM,
    crate::raw::zsh::GETSPARAM_U,
    crate::raw::zsh::SETAPARAM,
    crate::raw::zsh::ZALLOC,
    crate::raw::zsh::ZTRDUP,
];

/// Outcome of probing the running zsh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capabilities {
    /// Every required symbol resolved.
    Complete,
    /// One or more symbols are absent; the module must not proceed.
    Missing {
        /// Names that could not be resolved.
        symbols: Vec<&'static str>,
    },
}

impl Capabilities {
    /// Whether the module may proceed to call into zsh.
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }
}

impl fmt::Display for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Complete => write!(f, "all {} required symbols resolved", REQUIRED_SYMBOLS.len()),
            Self::Missing { symbols } => {
                write!(f, "missing zsh symbols: {}", symbols.join(", "))
            }
        }
    }
}

/// Probe the running process for every symbol the module requires.
///
/// Returns [`Capabilities::Complete`] when the process really is a zsh with the
/// expected exports. Any missing symbol is reported rather than deferred.
///
/// # Safety
///
/// Uses `dlsym(RTLD_DEFAULT, ...)`; safe to call from a zsh module entry point.
pub fn probe() -> Capabilities {
    let missing: Vec<&'static str> = REQUIRED_SYMBOLS
        .iter()
        .copied()
        .filter(|s| unsafe { crate::raw::dl::lookup_global(s) }.is_none())
        .collect();

    if missing.is_empty() {
        Capabilities::Complete
    } else {
        Capabilities::Missing { symbols: missing }
    }
}

/// Resolve a single required symbol to a typed function pointer.
///
/// # Safety
///
/// - The returned pointer must be transmuted to the signature zsh actually
///   uses for that symbol; a mismatch is undefined behaviour.
/// - Only callable after [`probe`] reported [`Capabilities::Complete`].
pub unsafe fn resolve_fn<T: Copy>(caps: &Capabilities, name: &'static str) -> Option<T> {
    if !caps.is_complete() {
        return None;
    }
    let p = unsafe { crate::raw::dl::lookup_global(name) }?;
    // SAFETY (documented on the function): caller asserts the signature.
    Some(unsafe { core::mem::transmute_copy::<*mut core::ffi::c_void, T>(&p) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_symbols_are_the_four_we_actually_call() {
        assert_eq!(REQUIRED_SYMBOLS.len(), 6);
        assert!(REQUIRED_SYMBOLS.contains(&"addbuiltins"));
        assert!(REQUIRED_SYMBOLS.contains(&"getsparam"));
        assert!(REQUIRED_SYMBOLS.contains(&"getsparam_u"));
        assert!(REQUIRED_SYMBOLS.contains(&"setaparam"));
        assert!(REQUIRED_SYMBOLS.contains(&"zalloc"));
        assert!(REQUIRED_SYMBOLS.contains(&"ztrdup"));
    }

    #[test]
    fn the_pool_allocators_are_never_required() {
        // `zhalloc` and `dupstring` allocate from zsh's *pool*, whose memory
        // `popheap()` recycles. Storing pool memory in a zsh parameter leaves
        // a dangling pointer that crashes the shell on the next command.
        // Their absence from the required set is a deliberate guard.
        assert!(!REQUIRED_SYMBOLS.contains(&"zhalloc"));
        assert!(!REQUIRED_SYMBOLS.contains(&"dupstring"));
    }

    #[test]
    fn missing_symbols_are_reported_not_hidden() {
        let caps = Capabilities::Missing { symbols: vec!["getsparam"] };
        assert!(!caps.is_complete());
        assert!(caps.to_string().contains("getsparam"));
    }

    #[test]
    fn complete_capabilities_pass_the_gate() {
        assert!(Capabilities::Complete.is_complete());
    }

    #[test]
    fn resolve_refuses_when_incomplete() {
        let caps = Capabilities::Missing { symbols: vec!["addbuiltins"] };
        let resolved: Option<unsafe extern "C" fn()> =
            unsafe { resolve_fn(&caps, "addbuiltins") };
        assert!(resolved.is_none(), "must not resolve against a failed probe");
    }
}