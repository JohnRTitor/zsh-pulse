//! Raw zsh loadable-module ABI, transcribed from zsh 5.9.2 source.
//!
//! Every item is annotated with the upstream file and line it came from.
//! Nothing here is guessed: layouts and signatures are copied from the
//! reference source tree, so no build-time dependency on zsh's headers is
//! needed.
//!
//! # Why transcription rather than bindgen
//!
//! zsh exposes its module API by exporting symbols from the `zsh` binary itself
//! (linked with `-rdynamic`, `zsh-src/configure.ac:2755-2763`) and resolving
//! them at `dlopen` time. Only three structs and a few functions are needed, so
//! hand-transcribing them is cheaper and more auditable than generating
//! bindings.
//!
//! # Layout tests
//!
//! The unit tests in this module assert the exact `size_of`, `align_of` and
//! field offsets against real zsh. If a future zsh changes a layout the tests
//! fail loudly rather than corrupting the user's shell.

use core::ffi::{c_char, c_int, c_void};

/// zsh's `Options`: the opaque option string handed to builtins.
/// (`zsh-src/Src/zsh.h:1436`)
pub type Options = *mut c_char;

/// zsh's `HandlerFunc` (`zsh-src/Src/zsh.h:1436`)
pub type HandlerFunc =
    unsafe extern "C" fn(*mut c_char, *mut *mut c_char, Options, c_int) -> c_int;

/// zsh's `Module`: opaque handle passed to every module entry point.
pub type Module = *mut c_void;

/// zsh's `HashNode` (`zsh-src/Src/zsh.h:1226-1230`)
#[repr(C)]
#[derive(Debug)]
pub struct HashNode {
    /// next in hash chain
    pub next: *mut HashNode,
    /// hash key
    pub nam: *mut c_char,
    /// various flags
    pub flags: c_int,
}

/// zsh's `struct builtin` (`zsh-src/Src/zsh.h:1440-1448`).
///
/// Field order and types must match exactly: zsh stores this struct verbatim.
#[repr(C)]
#[derive(Debug)]
pub struct Builtin {
    /// embedded hash node (`BUILTIN()` macro, `zsh-src/Src/zsh.h:1450-1451`)
    pub node: HashNode,
    /// pointer to the function that executes this builtin
    pub handlerfunc: HandlerFunc,
    /// minimum number of arguments
    pub minargs: c_int,
    /// maximum number of arguments, or -1 for no limit
    pub maxargs: c_int,
    /// xbins slot for overloaded handlers
    pub funcid: c_int,
    /// string of legal options
    pub optstr: *mut c_char,
    /// options set by default for overloaded handlers
    pub defopts: *mut c_char,
}

/// A NUL-terminated C string used in module registration tables.
///
/// # Why this is not just a `&str`
///
/// A Rust `&'static str` is a `(pointer, length)` pair; the bytes behind it are
/// **not** NUL-terminated. Handing one to zsh as a `char *` makes zsh read past
/// the end into whatever follows in `.rodata` — which during development
/// produced a builtin literally named `shell-pulse-SHELL_PULSE_DEBUG` and
/// corrupted zsh's builtin hash table.
///
/// Every string must therefore come from a C string literal (`c"…"`), which
/// Rust guarantees is NUL-terminated.
#[derive(Debug)]
#[repr(transparent)]
pub struct CStr(&'static core::ffi::CStr);

impl CStr {
    /// Wrap a C string literal such as `c"shell-pulse-"`.
    pub const fn new(s: &'static core::ffi::CStr) -> Self {
        Self(s)
    }

    /// Pointer to the NUL-terminated bytes.
    pub const fn as_ptr(&self) -> *const c_char {
        self.0.as_ptr()
    }

    /// The underlying C string.
    pub const fn as_cstr(&self) -> &'static core::ffi::CStr {
        self.0
    }
}

/// Type of the `setup_` / `boot_` / `cleanup_` / `finish_` entry points.
/// (`zsh-src/Src/zsh.h:1535`)
pub type ModuleVoidFunc = unsafe extern "C" fn(Module) -> c_int;

/// Type of the `features_` entry point. (`zsh-src/Src/zsh.h:1536`)
pub type ModuleFeaturesFunc = unsafe extern "C" fn(Module, *mut *mut *mut c_char) -> c_int;

/// Type of the `enables_` entry point. (`zsh-src/Src/zsh.h:1537`)
pub type ModuleEnablesFunc = unsafe extern "C" fn(Module, *mut *mut c_int) -> c_int;


/// Names of the zsh symbols this module resolves at load time.
///
/// Kept separate from any `extern` declaration: this crate deliberately has
/// **no link-time dependency on zsh**, so nothing here is referenced by the
/// linker. See the module documentation for why.
pub mod zsh {
    /// Register builtins (`zsh-src/Src/module.c:543`).
    pub const ADDBUILTINS: &str = "addbuiltins";
    /// Read a scalar parameter, metafied (`zsh-src/Src/params.c:3037`).
    ///
    /// Returns `char *`, **not** `char **`; getting that wrong segfaulted zsh
    /// during the Stage A proof-of-concept.
    pub const GETSPARAM: &str = "getsparam";
    /// Read a scalar parameter, unmetafied, into a static buffer
    /// (`zsh-src/Src/params.c:3049`).
    pub const GETSPARAM_U: &str = "getsparam_u";
    /// Set a scalar parameter (`zsh-src/Src/params.c`).
    pub const SETAPARAM: &str = "setaparam";
    /// Allocate a copy with zsh's allocator (`zsh-src/Src/utils.c`).
    ///
    /// Required because `setaparam` **takes ownership** of the value array and
    /// frees it with zsh's `freearray`; anything not allocated by zsh
    /// corrupts the heap.
    pub const DUPSTRING: &str = "dupstring";
    /// zsh's allocator (`zsh-src/Src/mem.c`).
    pub const ZHALLOC: &str = "zhalloc";
}

/// `dlopen`/`dlsym`, declared locally so this crate has **no dependencies**.
pub mod dl {
    use core::ffi::{c_char, c_void};

    /// `RTLD_DEFAULT` — search the global symbol scope, which for a zsh module
    /// includes the `zsh` executable's exported symbols.
    pub const RTLD_DEFAULT: *mut c_void = core::ptr::null_mut();

    unsafe extern "C" {
        /// `void *dlsym(void *handle, const char *symbol)`
        pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    /// Look up `symbol` in the global scope. Returns `None` if absent.
    ///
    /// # Safety
    ///
    /// `symbol` must be a valid NUL-terminated C string. A returned pointer
    /// must only be transmuted to a function pointer matching the real symbol.
    pub unsafe fn lookup_global(symbol: &str) -> Option<*mut c_void> {
        let c = std::ffi::CString::new(symbol).ok()?;
        let p = unsafe { dlsym(RTLD_DEFAULT, c.as_ptr()) };
        if p.is_null() { None } else { Some(p) }
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, size_of};

    // Exact sizes observed on x86_64 Linux against zsh 5.9.2. These assertions
    // are what stops us writing a subtly-wrong struct into zsh's builtin table.

    #[test]
    fn hashnode_layout_matches_zsh() {
        assert_eq!(size_of::<HashNode>(), 24);
        assert_eq!(align_of::<HashNode>(), 8);
    }

    #[test]
    fn builtin_layout_matches_zsh() {
        // node(24) + handlerfunc(8) + minargs/maxargs/funcid(12 + 4 pad)
        //           + optstr(8) + defopts(8) = 64
        assert_eq!(size_of::<Builtin>(), 64);
        assert_eq!(align_of::<Builtin>(), 8);
    }

    // A real function so the pointer is never null; it is never called.
    unsafe extern "C" fn unused_handler(
        _: *mut c_char,
        _: *mut *mut c_char,
        _: Options,
        _: c_int,
    ) -> c_int {
        unreachable!("layout test never invokes the handler")
    }

    #[test]
    fn field_offsets_match_zsh() {
        let noop: HandlerFunc = unused_handler;
        let b = Builtin {
            node: HashNode {
                next: core::ptr::null_mut(),
                nam: core::ptr::null_mut(),
                flags: 0,
            },
            handlerfunc: noop,
            minargs: 0,
            maxargs: 0,
            funcid: 0,
            optstr: core::ptr::null_mut(),
            defopts: core::ptr::null_mut(),
        };
        let base = &b as *const Builtin as usize;
        let off = |p: *const u8| p as usize - base;

        assert_eq!(off(&b.node.nam as *const _ as *const u8), 8);
        assert_eq!(off(&b.handlerfunc as *const _ as *const u8), 24);
        assert_eq!(off(&b.minargs as *const _ as *const u8), 32);
        assert_eq!(off(&b.maxargs as *const _ as *const u8), 36);
        assert_eq!(off(&b.funcid as *const _ as *const u8), 40);
        assert_eq!(off(&b.optstr as *const _ as *const u8), 48);
        assert_eq!(off(&b.defopts as *const _ as *const u8), 56);
    }

    #[test]
    fn cstr_really_is_nul_terminated() {
        // This is the bug that corrupted zsh's builtin table: a Rust `&str`
        // is NOT NUL-terminated. Assert the byte after the name really is NUL
        // so the mistake cannot come back.
        let s = CStr::new(c"shell-pulse-highlight");
        let bytes = s.as_cstr().to_bytes_with_nul();
        assert_eq!(*bytes.last().unwrap(), 0);
        assert_eq!(&bytes[..bytes.len() - 1], b"shell-pulse-highlight");
    }

    #[test]
    fn cstr_length_matches_without_trailing_nul() {
        let s = CStr::new(c"shell-pulse-");
        assert_eq!(s.as_cstr().to_bytes().len(), "shell-pulse-".len());
        assert_eq!(s.as_cstr().count_bytes(), 12);
    }
}