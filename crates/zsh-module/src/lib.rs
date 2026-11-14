//! The zsh loadable module: `zshpulse/core.so`.
//!
//! # Safety contract
//!
//! Every `unsafe fn` below is an FFI shim whose only caller is zsh itself,
//! via a C function pointer, with a valid `Module` handle. That contract cannot
//! be expressed in the signature, so it is stated once here rather than
//! repeated verbatim on eight entry points. Everything reachable *from* those
//! entry points carries its own `# Safety` documentation.
//!
//! Additionally, every entry point:
//! - is wrapped in [`zsh_abi::guard::guard`], so a Rust panic can never unwind
//!   into zsh;
//! - probes before use, so a zsh missing a symbol never reaches the faulting
//!   call.
//!
//! The result is that a hostile or unusual zsh produces a clean load failure
//! and a working shell, never a crash mid-session.
//!
#![allow(clippy::missing_safety_doc)]

#[cfg(not(any(
    feature = "highlight",
    feature = "autocomplete",
    feature = "history",
    feature = "history-navigation"
)))]
compile_error!("At least one feature must be enabled: 'highlight', 'autocomplete', or 'history'.");

use std::{ptr::addr_of_mut, sync::OnceLock};

use {
    engine::EngineState,
    zsh_abi::{
        Api, guard, probe,
        raw::{Builtin, Module},
    },
};

#[cfg(feature = "highlight")]
pub mod region;

#[cfg(feature = "highlight")]
pub mod highlight;
#[cfg(feature = "highlight")]
mod precommand;
#[cfg(feature = "highlight")]
pub use highlight::{BUILTIN_FULL, highlight_builtin, published_spans};

#[cfg(any(
    feature = "autocomplete",
    feature = "history",
    feature = "history-navigation"
))]
pub mod complete;
#[cfg(any(
    feature = "autocomplete",
    feature = "history",
    feature = "history-navigation"
))]
pub use complete::{BUILTIN_COMPLETE, complete_builtin};

/// Process-wide engine state, created on first use.
static ENGINE: OnceLock<EngineState> = OnceLock::new();

pub(crate) fn engine() -> &'static EngineState {
    ENGINE.get_or_init(EngineState::new)
}

/// Probe and resolved zsh entry points, established once during `setup_`.
static ZSH: OnceLock<Option<Api>> = OnceLock::new();

/// The resolved zsh API, or `None` if this zsh cannot support us.
pub(crate) fn zsh() -> Option<&'static Api> {
    ZSH.get_or_init(|| {
        let caps = probe::probe();
        if *debug_enabled() {
            debug(&format!("probe: {caps}"));
        }
        let api = unsafe { Api::resolve(&caps) };
        if *debug_enabled() {
            debug(&format!("resolved: {}", api.is_some()));
        }
        api
    })
    .as_ref()
}

/// Debug logging on/off, resolved **once**.
static DEBUG: OnceLock<bool> = OnceLock::new();

pub(crate) fn debug_enabled() -> &'static bool {
    DEBUG.get_or_init(|| std::env::var_os("ZSH_PULSE_DEBUG").is_some())
}

/// The most arguments any builtin will look at.
#[allow(dead_code)]
pub(crate) const MAX_ARGUMENTS: usize = 64;

/// Safe iterator over a NULL-terminated array of C strings (`*mut *mut c_char`).
pub(crate) struct RawArgIter<'a> {
    args: *mut *mut std::ffi::c_char,
    idx: usize,
    _marker: std::marker::PhantomData<&'a std::ffi::CStr>,
}

impl<'a> RawArgIter<'a> {
    /// # Safety
    ///
    /// `args` must be NULL or point to a valid NULL-terminated array of valid C string pointers.
    pub unsafe fn new(args: *mut *mut std::ffi::c_char) -> Self {
        Self {
            args,
            idx: 0,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a> Iterator for RawArgIter<'a> {
    type Item = &'a std::ffi::CStr;

    fn next(&mut self) -> Option<Self::Item> {
        if self.args.is_null() || self.idx >= MAX_ARGUMENTS {
            return None;
        }
        // Safety: `args` is NULL-terminated; if non-null, slot is valid to read up to NULL.
        let ptr = unsafe { *self.args.add(self.idx) };
        if ptr.is_null() {
            return None;
        }
        self.idx += 1;
        Some(unsafe { std::ffi::CStr::from_ptr(ptr) })
    }
}

#[allow(dead_code)]
pub(crate) fn has_argument(args: *mut *mut std::ffi::c_char, wanted: &[u8]) -> bool {
    if args.is_null() {
        return false;
    }
    // Safety: args is passed directly by zsh to the builtin handler.
    let iter = unsafe { RawArgIter::new(args) };
    iter.into_iter().any(|s| s.to_bytes() == wanted)
}

/// Emit a diagnostic when `ZSH_PULSE_DEBUG` is set in the environment.
pub(crate) fn debug(msg: &str) {
    use std::io::Write as _;
    if std::env::var_os("ZSH_PULSE_DEBUG").is_none() {
        return;
    }
    let mut err = std::io::stderr();
    let _ = writeln!(err, "[zsh-pulse] {msg}");
}

/// Install a panic hook that prints one line.
pub(crate) fn install_panic_hook() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        std::panic::set_hook(Box::new(|info| {
            use std::io::Write as _;
            let location = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()));
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown".to_string());
            let _ = writeln!(
                std::io::stderr(),
                "zsh-pulse: internal error at {}: {}",
                location.as_deref().unwrap_or("?"),
                message
            );
        }));
    });
}

/// Whether the user has been told that the plugin gave up.
#[allow(dead_code)]
static DEGRADED_REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[allow(dead_code)]
pub(crate) fn report_degraded() {
    use std::io::Write as _;
    if DEGRADED_REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let _ = writeln!(
        std::io::stderr(),
        "zsh-pulse: disabled after an internal error; the shell is unaffected"
    );
}

#[cfg(feature = "highlight")]
#[allow(unused_imports)]
pub(crate) use ::highlight::char_offsets_into;

#[cfg(not(feature = "highlight"))]
pub(crate) fn char_offsets_into(out: &mut Vec<u32>, text: &[u8]) {
    out.clear();
    out.resize(text.len() + 1, 0);
    let mut chars = 0u32;
    let mut i = 0usize;
    while i < text.len() {
        if text[i] & 0b1100_0000 == 0b1000_0000 {
            out[i] = chars;
            chars += 1;
            out[i + 1] = chars;
            i += 1;
            continue;
        }
        let mut end = i + 1;
        while end < text.len() && text[end] & 0b1100_0000 == 0b1000_0000 {
            end += 1;
        }
        for slot in &mut out[i..end] {
            *slot = chars;
        }
        chars += 1;
        out[end] = chars;
        i = end;
    }
}

// ---------------------------------------------------------------------------
// The builtin table
// ---------------------------------------------------------------------------

#[cfg(all(
    feature = "highlight",
    any(
        feature = "autocomplete",
        feature = "history",
        feature = "history-navigation"
    )
))]
const BUILTIN_COUNT: usize = 2;

#[cfg(not(all(
    feature = "highlight",
    any(
        feature = "autocomplete",
        feature = "history",
        feature = "history-navigation"
    )
)))]
const BUILTIN_COUNT: usize = 1;

static mut PULSE_BUILTINS: [Builtin; BUILTIN_COUNT + 1] = [
    #[cfg(feature = "highlight")]
    Builtin {
        node: zsh_abi::raw::HashNode {
            next: std::ptr::null_mut(),
            nam: BUILTIN_FULL.as_ptr() as *mut _,
            flags: 0,
        },
        handlerfunc: highlight_builtin,
        minargs: 0,
        maxargs: -1,
        funcid: 0,
        optstr: std::ptr::null_mut(),
        defopts: std::ptr::null_mut(),
    },
    #[cfg(any(
        feature = "autocomplete",
        feature = "history",
        feature = "history-navigation"
    ))]
    Builtin {
        node: zsh_abi::raw::HashNode {
            next: std::ptr::null_mut(),
            nam: BUILTIN_COMPLETE.as_ptr() as *mut _,
            flags: 0,
        },
        handlerfunc: complete_builtin,
        minargs: 0,
        maxargs: -1,
        funcid: 0,
        optstr: std::ptr::null_mut(),
        defopts: std::ptr::null_mut(),
    },
    Builtin {
        node: zsh_abi::raw::HashNode {
            next: std::ptr::null_mut(),
            nam: std::ptr::null_mut(),
            flags: 0,
        },
        #[cfg(feature = "highlight")]
        handlerfunc: highlight_builtin,
        #[cfg(not(feature = "highlight"))]
        handlerfunc: complete_builtin,
        minargs: 0,
        maxargs: 0,
        funcid: 0,
        optstr: std::ptr::null_mut(),
        defopts: std::ptr::null_mut(),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn builtins() -> &'static [Builtin; BUILTIN_COUNT + 1] {
        // Safety: unit tests run in isolation and only read the static builtin table.
        unsafe { &*addr_of_mut!(PULSE_BUILTINS) }
    }

    fn builtin_name(b: &Builtin) -> &std::ffi::CStr {
        if b.node.nam.is_null() {
            c""
        } else {
            // Safety: Builtin nam is a static C string literal.
            unsafe { std::ffi::CStr::from_ptr(b.node.nam) }
        }
    }

    #[test]
    fn builtin_table_is_null_terminated() {
        let t = builtins();
        assert!(
            t[BUILTIN_COUNT].node.nam.is_null(),
            "builtin table must be NULL-terminated"
        );
        for b in &t[..BUILTIN_COUNT] {
            assert!(!b.node.nam.is_null());
        }
    }

    #[test]
    fn builtin_name_matches_the_declared_prefix() {
        let t = builtins();
        #[cfg(all(
            feature = "highlight",
            any(
                feature = "autocomplete",
                feature = "history",
                feature = "history-navigation"
            )
        ))]
        {
            assert_eq!(
                builtin_name(&t[0]).to_bytes_with_nul(),
                b"zsh-pulse-highlight\0"
            );
            assert_eq!(
                builtin_name(&t[1]).to_bytes_with_nul(),
                b"zsh-pulse-complete\0"
            );
        }
        #[cfg(all(
            feature = "highlight",
            not(any(
                feature = "autocomplete",
                feature = "history",
                feature = "history-navigation"
            ))
        ))]
        {
            assert_eq!(
                builtin_name(&t[0]).to_bytes_with_nul(),
                b"zsh-pulse-highlight\0"
            );
        }
        #[cfg(not(feature = "highlight"))]
        {
            assert_eq!(
                builtin_name(&t[0]).to_bytes_with_nul(),
                b"zsh-pulse-complete\0"
            );
        }
    }

    #[test]
    fn builtin_accepts_any_argument_count() {
        let t = builtins();
        assert_eq!(t[0].minargs, 0);
        assert_eq!(t[0].maxargs, -1, "must accept arbitrary args");
    }

    #[test]
    fn mangled_aliases_are_exported() {
        let fns: [zsh_abi::raw::ModuleVoidFunc; 8] = [
            setup_,
            boot_,
            cleanup_,
            finish_,
            setup_zshpulseQscore,
            boot_zshpulseQscore,
            cleanup_zshpulseQscore,
            finish_zshpulseQscore,
        ];
        for f in fns {
            let _: zsh_abi::raw::ModuleVoidFunc = f;
        }
        assert_eq!(fns.len(), 8);
    }

    #[test]
    fn probe_in_a_non_zsh_process_reports_missing() {
        assert!(!probe::probe().is_complete());
    }
}

// ---------------------------------------------------------------------------
// Module entry points
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn setup_(_m: Module) -> std::ffi::c_int {
    guard(-1, || {
        install_panic_hook();
        debug("setup_ entered");
        let Some(api) = zsh() else {
            debug("setup_ refusing load: API unavailable");
            return -1;
        };

        let (builtin_nam, builtin_ptr, builtin_count) = unsafe {
            (
                PULSE_BUILTINS[0].node.nam,
                addr_of_mut!(PULSE_BUILTINS).cast::<Builtin>(),
                BUILTIN_COUNT as std::ffi::c_int,
            )
        };
        unsafe {
            (api.addbuiltins)(builtin_nam, builtin_ptr, builtin_count);
        }
        debug("setup_ registered builtins");
        0
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn boot_(_m: Module) -> std::ffi::c_int {
    guard(0, || {
        if let Some(w) = engine::worker::shared() {
            if let Ok(reply) = w.submit(
                0,
                b"zsh-pulse-probe".to_vec(),
                std::time::Duration::from_millis(200),
            ) {
                debug(&format!(
                    "worker ready: rev={} checksum={}",
                    reply.revision, reply.checksum
                ));
            }
        } else {
            debug("worker unavailable; continuing single-threaded");
        }
        0
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cleanup_(_m: Module) -> std::ffi::c_int {
    guard(0, || {
        debug("cleanup_: stopping the worker");
        engine::worker::shutdown_shared();
        0
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn finish_(_m: Module) -> std::ffi::c_int {
    guard(0, || {
        debug("finish_: stopping the worker");
        engine::worker::shutdown_shared();
        0
    })
}

macro_rules! mangled_alias {
    ($fn_name:ident, $alias:ident) => {
        #[doc = "Mangled alias for builds without `DYNAMIC_NAME_CLASH_OK`."]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $alias(m: Module) -> std::ffi::c_int {
            unsafe { $fn_name(m) }
        }
    };
}

mangled_alias!(setup_, setup_zshpulseQscore);
mangled_alias!(boot_, boot_zshpulseQscore);
mangled_alias!(cleanup_, cleanup_zshpulseQscore);
mangled_alias!(finish_, finish_zshpulseQscore);
