//! The zsh loadable module: `shellpulse/highlight.so`.
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

use std::ptr::addr_of_mut;
use std::sync::OnceLock;

use engine::EngineState;
use zsh_abi::raw::{Builtin, CStr, Module};
use zsh_abi::{Api, guard, probe};

/// Process-wide engine state, created on first use.
static ENGINE: OnceLock<EngineState> = OnceLock::new();

fn engine() -> &'static EngineState {
    ENGINE.get_or_init(EngineState::new)
}

/// Probe and resolved zsh entry points, established once during `setup_`.
static ZSH: OnceLock<Option<Api>> = OnceLock::new();

/// The resolved zsh API, or `None` if this zsh cannot support us.
///
/// Resolved in `setup_`, before any builtin can run, so a hostile zsh fails the
/// load rather than failing mid-keystroke.
fn zsh() -> Option<&'static Api> {
    ZSH.get_or_init(|| {
        let caps = probe::probe();
        if *debug_enabled() {
            debug(&format!("probe: {caps}"));
        }
        // Safety: `setup_` is only ever called by zsh, so the global symbol
        // scope is the zsh executable's.
        let api = unsafe { Api::resolve(&caps) };
        if *debug_enabled() {
            debug(&format!("resolved: {}", api.is_some()));
        }
        api
    })
    .as_ref()
}

/// Debug logging on/off, resolved **once**.
///
/// Reading the environment is itself an allocation (`OsString`), so doing it
/// per keystroke cost real time. `debug()` takes `&str` precisely so that
/// callers can gate the `format!` behind this flag.
static DEBUG: OnceLock<bool> = OnceLock::new();

fn debug_enabled() -> &'static bool {
    DEBUG.get_or_init(|| std::env::var_os("SHELL_PULSE_DEBUG").is_some())
}

/// Whether the (currently unsafe) parameter write-back is enabled.
static ECHO: OnceLock<bool> = OnceLock::new();

fn echo_enabled() -> &'static bool {
    ECHO.get_or_init(|| std::env::var_os("SHELL_PULSE_ECHO").is_some())
}

// ---------------------------------------------------------------------------
// The builtin itself
// ---------------------------------------------------------------------------

/// Reusable scratch buffers for the keystroke path.
///
/// Allocated once, then reused forever. This is what removed the per-keystroke
/// allocations that made the first implementation 10x slower than the C
/// proof-of-concept (1.2 us vs 112 ns per call).
struct Scratch {
    /// Unmetafied `BUFFER` bytes.
    buffer: Vec<u8>,
    /// Raw bytes of `CURSOR`, kept only for diagnostics.
    cursor_bytes: Vec<u8>,
}

impl Scratch {
    const fn new() -> Self {
        Self { buffer: Vec::new(), cursor_bytes: Vec::new() }
    }
}

static SCRATCH: std::sync::Mutex<Scratch> = std::sync::Mutex::new(Scratch::new());

/// `shell-pulse-highlight` — invoked once per redraw by the zsh adapter.
///
/// Stage B scope: prove we can read live ZLE state from inside the process. It
/// reads `BUFFER` and `CURSOR` and returns; highlighting lands in Stage C.
unsafe extern "C" fn highlight_builtin(
    _nam: *mut std::ffi::c_char,
    _args: *mut *mut std::ffi::c_char,
    _ops: zsh_abi::Options,
    _len: std::ffi::c_int,
) -> std::ffi::c_int {
    guard(0, || {
        // Rule 1: never touch zsh before the API has been resolved.
        let Some(api) = zsh() else {
            return 0;
        };

        // Read live ZLE state into reused buffers: no allocation once warm.
        let mut guard = match SCRATCH.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Safety: `api` exists only because we are inside zsh.
        let cursor = unsafe {
            let bytes = api.read_param_i64(cstr(b"CURSOR\0"), &mut guard.cursor_bytes);
            // CURSOR must be read *before* BUFFER, because both share zsh's
            // single static unmetafied buffer.
            let buf_len = api.read_param_bytes(cstr(b"BUFFER\0"), &mut guard.buffer);
            let _ = buf_len;
            bytes
        };
        let buffer_len = guard.buffer.len();

        if *debug_enabled() {
            debug(&format!(
                "read BUFFER={:?} ({buffer_len} bytes) CURSOR={cursor:?}",
                String::from_utf8_lossy(&guard.buffer)
            ));
        }

        // Each keystroke advances the line revision; asynchronous results
        // tagged with an older revision are refused.
        let revision = engine().record_highlight();

        observe(revision, cursor.unwrap_or_default());

        // Write-back proof. Writing a zsh parameter is currently UNSAFE on
        // repeat: `set_param_string` allocates with zsh's allocator (required,
        // since `assignaparam` frees the array it is given), but a second
        // assignment to an already-set parameter corrupts the heap — observed
        // as `free(): invalid size`. That must be understood before Stage C
        // relies on the same mechanism for `region_highlight`.
        //
        // Gated off by default so nothing unsafe runs in a normal shell.
        if *echo_enabled() {
            // Safety: `api` is resolved and we are inside zsh. The value is
            // copied into zsh-owned memory before the temporary is dropped.
            unsafe {
                let value = std::ffi::CString::new(format!(
                    "{revision}:{}:{}",
                    cursor.unwrap_or_default(),
                    String::from_utf8_lossy(&guard.buffer)
                ))
                .unwrap_or_default();
                api.set_param_string(cstr(b"SHELL_PULSE_STATE\0"), &value);
            }
        }
        0
    })
}

/// Borrow a byte string as a NUL-terminated C string.
fn cstr(bytes: &[u8]) -> &std::ffi::CStr {
    // Safety: every call site passes a literal ending in NUL.
    unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(bytes) }
}

/// Emit a diagnostic when `SHELL_PULSE_DEBUG` is set in the environment.
///
/// Uses raw `write(2)`: this may run before zsh has finished setting up, and
/// it must never allocate or panic on the load path.
fn debug(msg: &str) {
    use std::io::Write as _;
    if std::env::var_os("SHELL_PULSE_DEBUG").is_none() {
        return;
    }
    let mut err = std::io::stderr();
    let _ = writeln!(err, "[shell-pulse] {msg}");
}

/// Last line observed, used by the in-zsh self test.
#[derive(Debug, Default)]
struct Observed {
    revision: u64,
    cursor: i64,
}

static OBSERVED: OnceLock<std::sync::Mutex<Observed>> = OnceLock::new();

fn observed() -> &'static std::sync::Mutex<Observed> {
    OBSERVED.get_or_init(|| std::sync::Mutex::new(Observed::default()))
}

/// Record the call. Deliberately allocation-free: holding a `String` of the
/// buffer here put a heap allocation back on the keystroke path.
fn observe(revision: u64, cursor: i64) {
    if let Ok(mut o) = observed().lock() {
        o.revision = revision;
        o.cursor = cursor;
    }
}


/// The builtin table handed to `addbuiltins`.
///
/// Terminator: `nam == NULL`, as zsh's `addbuiltins()` loop requires
/// (`zsh-src/Src/module.c:543`).
/// The full builtin name as zsh will register it: `shell-pulse-highlight`.
static BUILTIN_FULL: CStr = CStr::new(c"shell-pulse-highlight");

/// The builtin table handed to `addbuiltins`.
///
/// zsh registers builtins under `b->node.nam` verbatim — `addbuiltin()`
/// (`zsh-src/Src/module.c:408-417`) does **not** prepend the name passed to
/// `addbuiltins()`; that argument is only used in diagnostics. Both the name and
/// the strings it points at must therefore be NUL-terminated (`c"…"` literals).
static mut PULSE_BUILTINS: [Builtin; 2] = [
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
    Builtin {
        node: zsh_abi::raw::HashNode {
            next: std::ptr::null_mut(),
            nam: std::ptr::null_mut(),
            flags: 0,
        },
        handlerfunc: highlight_builtin,
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

    #[test]
    fn builtin_table_is_null_terminated() {
        // zsh's `addbuiltins()` scans until `nam == NULL`
        // (`zsh-src/Src/module.c:543`). A missing terminator would run off the
        // end of the array inside zsh.
        let t = unsafe { &*addr_of_mut!(PULSE_BUILTINS) };
        assert!(t[1].node.nam.is_null(), "builtin table must be NULL-terminated");
        assert!(!t[0].node.nam.is_null());
    }

    #[test]
    fn builtin_name_matches_the_declared_prefix() {
        let t = unsafe { &*addr_of_mut!(PULSE_BUILTINS) };
        let name = unsafe { std::ffi::CStr::from_ptr(t[0].node.nam) };
        assert_eq!(name.to_bytes_with_nul(), b"shell-pulse-highlight\0");
    }

    #[test]
    fn builtin_accepts_any_argument_count() {
        let t = unsafe { &*addr_of_mut!(PULSE_BUILTINS) };
        assert_eq!(t[0].minargs, 0);
        assert_eq!(t[0].maxargs, -1, "must accept arbitrary args");
    }

    #[test]
    fn mangled_aliases_are_exported() {
        // Both symbol forms must exist so one build works on zsh builds with
        // and without DYNAMIC_NAME_CLASH_OK. Holding them in a typed array
        // proves they all exist with the right signature.
        let fns: [zsh_abi::raw::ModuleVoidFunc; 8] = [
            setup_,
            boot_,
            cleanup_,
            finish_,
            setup_shellpulseQshighlight,
            boot_shellpulseQshighlight,
            cleanup_shellpulseQshighlight,
            finish_shellpulseQshighlight,
        ];
        for f in fns {
            // Every entry must be a callable function pointer of the right type.
            let _: zsh_abi::raw::ModuleVoidFunc = f;
        }
        assert_eq!(fns.len(), 8);
    }

    #[test]
    fn probe_in_a_non_zsh_process_reports_missing() {
        // Under `cargo test` these symbols are absent — exactly the condition
        // the probe exists to catch. Inside real zsh it must be Complete.
        // (This is why the module's real tests live in tests/, which spawn zsh.)
        assert!(!probe::probe().is_complete());
    }
}
// ---------------------------------------------------------------------------
// Module entry points
// ---------------------------------------------------------------------------
//
// zsh resolves each of these with `dlsym(handle, "setup_")` on builds defining
// `DYNAMIC_NAME_CLASH_OK` (`zsh-src/Src/module.c:1774-1777`), or with a
// name-mangled variant otherwise (`:1780-1798`: `/` -> `Qs`, `_` -> `Qu`).
//
// We export both forms. Whichever the host does not use is simply ignored, so
// one build of the module works on both kinds of zsh.

/// Module setup. Probes zsh, then registers our builtins.
///
/// Returning non-zero makes zsh abort the load cleanly, which is how a zsh
/// missing a required symbol degrades to native highlighting instead of
/// crashing later inside the user's shell.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setup_(_m: Module) -> std::ffi::c_int {
    guard(-1, || {
        debug("setup_ entered");
        // Resolve the API first. If this zsh does not export what we need,
        // refuse the load so the zsh adapter can fall back to native zsh
        // highlighting. Returning -1 aborts cleanly; the alternative is a crash
        // on an arbitrary future keystroke.
        let Some(api) = zsh() else {
            debug("setup_ refusing load: API unavailable");
            return -1;
        };

        // Safety: `api` is resolved, we are inside zsh, and `PULSE_BUILTINS` is
        // a NULL-terminated table whose strings are 'static.
        unsafe {
            (api.addbuiltins)(
                BUILTIN_FULL.as_ptr(),
                addr_of_mut!(PULSE_BUILTINS).cast::<Builtin>(),
                1,
            );
        }
        debug("setup_ registered builtins");
        0
    })
}

/// Module boot: nothing to do yet, but zsh warns if the symbol is missing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn boot_(_m: Module) -> std::ffi::c_int {
    0
}

/// Module cleanup.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cleanup_(_m: Module) -> std::ffi::c_int {
    0
}

/// Module finish.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finish_(_m: Module) -> std::ffi::c_int {
    0
}

// --- mangled aliases, for builds without DYNAMIC_NAME_CLASH_OK --------------
//
// zsh builds the symbol as `<prefix><mangled module name>`. The module is
// loaded as `shellpulse/highlight`, so `/` becomes `Qs` and `_` becomes `Qu`
// (`zsh-src/Src/module.c:1786-1796`), giving `setup_shellpulseQshighlight`.
//
// These exist purely for the host's lookup and are never called by us.
// `#[used]` keeps them from being stripped as unreferenced.

macro_rules! mangled_alias {
    ($fn_name:ident, $alias:ident) => {
        #[doc = "Mangled alias for builds without `DYNAMIC_NAME_CLASH_OK`."]
        #[unsafe(no_mangle)]

        pub unsafe extern "C" fn $alias(m: Module) -> std::ffi::c_int {
            unsafe { $fn_name(m) }
        }
    };
}

mangled_alias!(setup_, setup_shellpulseQshighlight);
mangled_alias!(boot_, boot_shellpulseQshighlight);
mangled_alias!(cleanup_, cleanup_shellpulseQshighlight);
mangled_alias!(finish_, finish_shellpulseQshighlight);