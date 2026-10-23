//! Resolved entry points into zsh.
//!
//!
//! # Why resolve at runtime instead of linking directly
//!
//! A first version declared `extern "C" { fn getsparam(..); }` and called it.
//! That compiles for a `cdylib` but is fragile in two ways:
//!
//! 1. the symbol is an *undefined* dynamic reference, so if the running zsh
//!    does not export it the process dies at the first call — arbitrarily
//!    late, inside the user's shell;
//! 2. it makes the crate unlinkable in any non-zsh process, which removes the
//!    ability to unit-test anything around it.
//!
//! Resolving once through `dlsym` fixes both: there is no link-time dependency
//! on zsh at all, the [`crate::probe`] result becomes load-bearing rather than
//! advisory, and the crate links normally in tests.
//!
//! The cost is one indirect call per operation, measured at no more than a
//! nanosecond — irrelevant next to the ~112 ns the call itself costs.

use core::ffi::{c_char, c_int};

use crate::raw::{Builtin, Options};
use crate::probe::Capabilities;

/// zsh's `String` (`char *`), as produced by `dupstring`/`ztrdup`.
pub type ZshString = *mut c_char;

/// `void addbuiltins(char const *nam, Builtin binl, int size)`
pub type AddBuiltinsFn = unsafe extern "C" fn(*const c_char, *mut Builtin, c_int);

/// `char *getsparam(char *s)`
pub type GetSparamFn = unsafe extern "C" fn(*mut c_char) -> *mut c_char;

/// `int setaparam(char *nam, char **val)`
pub type SetaparamFn = unsafe extern "C" fn(*mut c_char, *mut *mut c_char) -> c_int;

/// `char **getaparam(char *s)` (`zsh-src/Src/params.c:3057-3070`).
///
/// Returns the elements of an array parameter as a NUL-terminated `char **`,
/// or `NULL` when the parameter does not exist or is not an array.
///
/// The caller does **not** own the returned array, and it may live in zsh's
/// *pool* memory — see [`Api::get_param_array`] for why that forces a copy.
pub type GetAparamFn = unsafe extern "C" fn(*mut c_char) -> *mut *mut c_char;

/// The optional zsh symbol that reads an array parameter.
///
/// This name belongs in `crate::raw::zsh` next to the other symbol names, but
/// that module is outside the file scope of this change, so it is declared
/// here instead.
///
/// It is deliberately **not** in [`crate::probe::REQUIRED_SYMBOLS`]: a zsh
/// that does not export it must still be able to load the module, which then
/// falls back to replacing the array wholesale instead of merging into it.
/// See [`Api::get_param_array`].
pub const GETAPARAM: &str = crate::raw::zsh::GETAPARAM;

/// The outcome of a capped read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CappedRead {
    /// The parameter does not exist.
    Missing,
    /// The value is longer than the cap. Nothing was copied.
    TooLong(usize),
    /// The value was copied. This is its length in bytes.
    Read(usize),
}

/// `void *zalloc(size_t)` — zsh's **permanent** allocator (`zsh-src/Src/mem.c:959`).
///
/// This is `malloc` with zsh's out-of-memory handling. It is the only
/// allocator whose memory survives zsh's `pushheap`/`popheap` cycle.
pub type ZAllocFn = unsafe extern "C" fn(usize) -> *mut core::ffi::c_void;

/// `char *ztrdup(char const *)` — duplicate a string with zsh's permanent
/// allocator (`zsh-src/Src/string.c:62`).
pub type ZTrdupFn = unsafe extern "C" fn(*const c_char) -> ZshString;

/// Look up a zsh symbol and reinterpret it as `F`.
///
/// # Safety
///
/// `name` must be a zsh symbol whose real signature is exactly `F`. Getting
/// this wrong is undefined behaviour, which is why every symbol we resolve is
/// enumerated in one place inside [`Api::resolve`].
#[inline]
unsafe fn fn_ptr<F: Copy>(name: &str) -> Option<F> {
    // Safety: caller guarantees `F` matches the symbol's true signature.
    let p = unsafe { crate::raw::dl::lookup_global(name)? };
    // Safety: `p` is a function address from `dlsym`. `F: Copy` makes this a
    // bit-copy of the pointer rather than a read of uninitialised memory.
    Some(unsafe { std::mem::transmute_copy::<*mut core::ffi::c_void, F>(&p) })
}

/// Parse a decimal integer, rejecting anything that is not one.
///
/// Split out from [`Api::read_param_i64`] so the parsing is unit-testable
/// without a live zsh. Deliberately strict: `CURSOR` is always a plain integer,
/// and anything else means the parameter is not what we think it is.
fn parse_i64(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }
    let (negative, digits) = match bytes[0] {
        b'-' => (true, &bytes[1..]),
        _ => (false, bytes),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut acc: i64 = 0;
    for &b in digits {
        acc = acc.checked_mul(10)?.checked_add(i64::from(b - b'0'))?;
    }
    Some(if negative { -acc } else { acc })
}

/// Read an array parameter through `getaparam` into `out`.
///
/// Split out of [`Api::get_param_array`] so the copying can be unit-tested
/// without a live zsh.
///
/// Returns `false` when the symbol is unavailable, when the parameter does not
/// exist, or when it is not an array. An array with no elements is *not* a
/// failure: zsh returns a one-element array whose only element is `NULL`.
///
/// # Safety
///
/// `getaparam` must be zsh's real `getaparam` — or a faithful stand-in — and
/// the call must happen inside zsh with a NUL-terminated name.
unsafe fn read_array(
    getaparam: Option<GetAparamFn>,
    name: &std::ffi::CStr,
    out: &mut Vec<String>,
) -> bool {
    out.clear();
    let Some(getaparam) = getaparam else {
        // The symbol is optional: a zsh without it must still load.
        return false;
    };
    // Safety: caller guarantees zsh context and a valid name.
    let array = unsafe { getaparam(name.as_ptr() as *mut c_char) };
    if array.is_null() {
        // No such parameter, or it is not an array.
        return false;
    }
    let mut i = 0usize;
    loop {
        // Safety: `array` is a NUL-terminated `char **`; while the previous
        // element was non-NULL, this one is readable.
        let element = unsafe { *array.add(i) };
        if element.is_null() {
            break;
        }
        // Safety: zsh NUL-terminates every element.
        let bytes = unsafe { std::ffi::CStr::from_ptr(element) }.to_bytes();
        // The rows are plain ASCII, but a lossy conversion must never panic:
        // this runs inside the user's interactive shell.
        out.push(String::from_utf8_lossy(bytes).into_owned());
        i += 1;
    }
    true
}

#[derive(Debug, Clone, Copy)]
pub struct Api {
    /// `addbuiltins`
    pub addbuiltins: AddBuiltinsFn,
    /// `getsparam`
    pub getsparam: GetSparamFn,
    /// `getsparam_u`
    pub getsparam_u: GetSparamFn,
    /// `setaparam`
    pub setaparam: SetaparamFn,
    /// `getaparam` — read an array parameter's elements.
    ///
    /// **Optional**: `None` when the running zsh does not export the symbol.
    /// The module still loads in that case; it just cannot read an array back.
    /// See [`Api::get_param_array`].
    pub getaparam: Option<GetAparamFn>,
    /// `zalloc` — permanent allocator.
    pub zalloc: ZAllocFn,
    /// `ztrdup` — permanent string duplication.
    pub ztrdup: ZTrdupFn,
}

impl Api {
    /// Resolve every entry point. Returns `None` if a **required** symbol is
    /// absent.
    ///
    /// `getaparam` is resolved *optionally*: it is deliberately not a required
    /// symbol (see [`crate::probe::REQUIRED_SYMBOLS`]), so a zsh that does not
    /// export it still resolves, and [`Api::get_param_array`] reports that it
    /// cannot read arrays.
    ///
    /// # Safety
    ///
    /// The caller must have established that it is running inside zsh (the
    /// symbols resolved from the global scope belong to the `zsh` executable).
    /// The returned function pointers carry zsh's documented signatures.
    pub unsafe fn resolve(caps: &Capabilities) -> Option<Self> {
        if !caps.is_complete() {
            return None;
        }
        // SAFETY: each lookup is non-null because `caps` is Complete, and each
        // pointer is cast to the signature zsh documents for that symbol.
        unsafe {
            Some(Self {
                addbuiltins: fn_ptr(crate::raw::zsh::ADDBUILTINS)?,
                getsparam: fn_ptr(crate::raw::zsh::GETSPARAM)?,
                getsparam_u: fn_ptr(crate::raw::zsh::GETSPARAM_U)?,
                setaparam: fn_ptr(crate::raw::zsh::SETAPARAM)?,
                zalloc: fn_ptr(crate::raw::zsh::ZALLOC)?,
                ztrdup: fn_ptr(crate::raw::zsh::ZTRDUP)?,
                // Optional: a missing `getaparam` must not stop the module
                // from loading, so this one is not unwrapped with `?`.
                getaparam: fn_ptr(GETAPARAM),
            })
        }
    }

    /// Whether a parameter with this name exists.
    ///
    /// `getsparam` returns `NULL` for a parameter that does not exist and a
    /// non-null pointer for one that does — including an array parameter,
    /// whose first element is returned (`zsh-src/Src/params.c:2344-2378`).
    ///
    /// **This is not cheap for an array**: zsh materialises the whole array
    /// to answer it. Call it at most once per line, not once per keystroke.
    ///
    /// # Safety
    ///
    /// Must only be called from inside zsh with a NUL-terminated name.
    pub unsafe fn param_exists(&self, name: &std::ffi::CStr) -> bool {
        // Safety: caller guarantees zsh context and a valid name.
        let p = unsafe { (self.getsparam)(name.as_ptr() as *mut c_char) };
        !p.is_null()
    }

    /// Whether the shell is currently inside a ZLE widget.
    ///
    /// `WIDGET` is one of the parameters `makezleparams` creates for the
    /// duration of a widget (`zsh-src/Src/Zle/zle_params.c:141-188`), and
    /// it is a *scalar*, so reading it costs nothing. It is the cheapest
    /// reliable test for "am I in the line editor?".
    ///
    /// The module uses it for two reasons: the ZLE special parameters only
    /// exist inside a widget, and the existence of `region_highlight` alone
    /// cannot be told apart from a global array of that name.
    ///
    /// # Safety
    ///
    /// Must only be called from inside zsh.
    pub unsafe fn in_zle(&self) -> bool {
        // Safety: caller guarantees zsh context and a valid name.
        let p = unsafe { (self.getsparam)(c"WIDGET".as_ptr() as *mut c_char) };
        !p.is_null()
    }
    /// Whether menu selection or completion is currently active in ZLE.
    ///
    /// # Safety
    ///
    /// Must only be called from inside zsh.
    pub unsafe fn is_menu_active(&self) -> bool {
        // SAFETY: Caller guarantees we are executing inside the zsh process.
        unsafe {
            if let Some(p) = crate::raw::dl::lookup_global("localkeymap") {
                let map = *(p as *const *const core::ffi::c_void);
                if !map.is_null() {
                    return true;
                }
            }
            if let Some(p) = crate::raw::dl::lookup_global("menucmp") {
                let val = *(p as *const core::ffi::c_int);
                if val != 0 {
                    return true;
                }
            }
        }
        false
    }

    /// Read a scalar parameter as a lossily-converted `String`.
    ///
    /// Returns `None` if the parameter is unset or null.
    ///
    /// **This allocates.** On the keystroke path prefer
    /// [`Api::read_param_bytes`], which reuses a caller-owned buffer and does
    /// not allocate at all once warm.
    ///
    /// # Safety
    ///
    /// Must only be called from inside zsh with a NUL-terminated name.
    pub unsafe fn get_param_string(&self, name: &std::ffi::CStr) -> Option<String> {
        // SAFETY: caller guarantees zsh context and a valid name.
        let p = unsafe { (self.getsparam)(name.as_ptr() as *mut c_char) };
        if p.is_null() {
            return None;
        }
        // SAFETY: zsh NUL-terminates parameter values.
        Some(unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }

    /// Read an array parameter's elements into `out`.
    ///
    /// The elements are **copied**: zsh's array may live in pool memory that is
    /// recycled at the end of the command, and the caller usually needs the
    /// strings to outlive the call.
    ///
    /// Returns `false` when the parameter does not exist, or when it is not an
    /// array.
    ///
    /// # Ownership — why the copy is not optional
    ///
    /// `getaparam` (`zsh-src/Src/params.c:3057-3070`) hands back the array
    /// through the parameter's own getter. For the ZLE special parameter
    /// `region_highlight` that getter is `get_region_highlight`
    /// (`zsh-src/Src/Zle/zle_refresh.c:429-480`), which builds a **fresh**
    /// array — and every one of its strings — with `zhalloc`, zsh's *pool*
    /// allocator. Pool memory is rewound and recycled by `popheap()`
    /// (`zsh-src/Src/mem.c:1425-1500`), which zsh calls around every command
    /// (`zsh-src/Src/exec.c:1275`). A string kept past the end of the command
    /// would therefore point at recycled memory.
    ///
    /// Copying into `out` is what makes the result safe to keep: from then on
    /// the strings are owned by Rust. `out` is cleared first, so a failed
    /// read never leaves stale rows behind.
    ///
    /// # The symbol is optional
    ///
    /// `getaparam` is resolved *optionally* — it is deliberately **not** in
    /// [`crate::probe::REQUIRED_SYMBOLS`]. A zsh that does not export it must
    /// still be able to load the module: the module then falls back to
    /// replacing the array instead of merging into it. When the symbol is
    /// missing this returns `false`.
    ///
    /// # Safety
    ///
    /// Must only be called from inside zsh with a NUL-terminated name.
    pub unsafe fn get_param_array(&self, name: &std::ffi::CStr, out: &mut Vec<String>) -> bool {
        // Safety: caller guarantees zsh context and a valid name.
        unsafe { read_array(self.getaparam, name, out) }
    }

    /// Copy a scalar parameter's **unmetafied** bytes into a caller-owned buffer.
    ///
    /// This is the hot-path reader. It performs no allocation once `out` has
    /// spare capacity, because it only truncates and overwrites.
    ///
    /// `getsparam_u` returns the unmetafied value in a **static buffer owned by
    /// zsh**, so the bytes must be copied before the next call — which this
    /// does immediately. Callers must therefore not hold the result across
    /// another parameter read.
    ///
    /// Returns the number of bytes written.
    ///
    /// # Safety
    ///
    /// Must only be called from inside zsh with a NUL-terminated name.
    pub unsafe fn read_param_bytes(
        &self,
        name: &std::ffi::CStr,
        out: &mut Vec<u8>,
    ) -> Option<usize> {
        // SAFETY: caller guarantees zsh context and a valid name.
        let p = unsafe { (self.getsparam_u)(name.as_ptr() as *mut c_char) };
        out.clear();
        if p.is_null() {
            return None;
        }
        // SAFETY: zsh NUL-terminates the static unmetafied buffer.
        let len = unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes().len();
        // SAFETY: `p` points at `len` readable bytes, just measured.
        out.extend_from_slice(unsafe { std::slice::from_raw_parts(p.cast::<u8>(), len) });
        Some(len)
    }

    /// Read a scalar parameter's bytes, refusing to copy more than `max`.
    ///
    /// The command line is read this way: a user can paste an arbitrarily
    /// long line, and the module must not copy it into its own memory just
    /// to decide that it is too long to highlight.
    ///
    /// # Safety
    ///
    /// Must only be called from inside zsh with a NUL-terminated name.
    pub unsafe fn read_param_bytes_capped(
        &self,
        name: &std::ffi::CStr,
        out: &mut Vec<u8>,
        max: usize,
    ) -> CappedRead {
        // Safety: caller guarantees zsh context and a valid name.
        let p = unsafe { (self.getsparam_u)(name.as_ptr() as *mut c_char) };
        if p.is_null() {
            out.clear();
            return CappedRead::Missing;
        }
        // Safety: zsh NUL-terminates the static unmetafied buffer.
        let len = unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes().len();
        if len > max {
            out.clear();
            return CappedRead::TooLong(len);
        }
        out.clear();
        // Safety: `p` points at `len` readable bytes, just measured.
        out.extend_from_slice(unsafe { std::slice::from_raw_parts(p.cast::<u8>(), len) });
        CappedRead::Read(len)
    }

    /// Read a scalar parameter as an integer without building a `String`.
    ///
    /// Used for `CURSOR`, where the previous implementation allocated a `String`
    /// per keystroke purely to throw it away.
    ///
    /// `scratch` is reused to hold the raw bytes for diagnostics; pass a
    /// zero-capacity vector and ignore it if that is not needed.
    ///
    /// # Safety
    ///
    /// Must only be called from inside zsh with a NUL-terminated name.
    pub unsafe fn read_param_i64(
        &self,
        name: &std::ffi::CStr,
        scratch: &mut Vec<u8>,
    ) -> Option<i64> {
        // SAFETY: caller guarantees zsh context and a valid name.
        let p = unsafe { (self.getsparam_u)(name.as_ptr() as *mut c_char) };
        if p.is_null() {
            scratch.clear();
            return None;
        }
        // SAFETY: zsh NUL-terminates the static unmetafied buffer.
        let bytes = unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes();
        let parsed = parse_i64(bytes);
        scratch.clear();
        if parsed.is_some() {
            scratch.extend_from_slice(bytes);
        }
        parsed
    }

    /// Set a scalar parameter from a C string.
    ///
    /// # Ownership — read this before calling
    ///
    /// `setaparam` takes **ownership** of the value array. `assignaparam`
    /// (`zsh-src/Src/params.c:3318`) hands the array straight to
    /// `setarrvalue` -> `arrsetfn` (`zsh-src/Src/params.c:2903`), which
    /// stores it in the parameter and releases the previous value with
    /// `freearray` (`zsh-src/Src/utils.c:4122`):
    ///
    /// ```c
    /// void freearray(char **s) {
    ///     while (*s) zsfree(*s++);   /* frees each element */
    ///     free(t);                   /* frees the array itself */
    /// }
    /// ```
    ///
    /// Two separate constraints follow, and getting either wrong corrupts the
    /// user's shell:
    ///
    /// 1. **The array must come from `zalloc`.** `freearray` releases the
    ///    array with `free()`, so the array must be plain `malloc` memory.
    /// 2. **The elements must come from `ztrdup`.** `freearray` releases each
    ///    element with `zsfree()`, which is `free()` in a zsh built without
    ///    `ZSH_MEM` — so the elements must be plain `malloc` memory too.
    ///
    /// The superficially equivalent symbols `zhalloc` and `dupstring` are
    /// **wrong**: they allocate from zsh's *pool* allocator, and pool memory
    /// is rewound and recycled by `popheap()` (`zsh-src/Src/mem.c:1425`),
    /// which zsh calls around every command (`zsh-src/Src/exec.c:1275`).
    /// A parameter holding pool memory therefore points at recycled memory
    /// the moment the next command runs — observed as a `SIGSEGV` inside
    /// `sepjoin` while reading the parameter back.
    ///
    /// zsh's own idiom matches this exactly: `zalloc` for the array and
    /// `ztrdup` for the elements (`zsh-src/Src/params.c:1013-1017`).
    ///
    /// The return value is not meaningful: `setaparam` is declared to return
    /// `int` but returns a `Param` pointer truncated to `int`
    /// (`zsh-src/Src/params.c:3556-3559`), so it always looks like garbage.
    ///
    /// # Safety
    ///
    /// Must only be called from inside zsh with NUL-terminated arguments.
    pub unsafe fn set_param_string(&self, name: &std::ffi::CStr, value: &std::ffi::CStr) {
        // Two slots, NULL-terminated: the value and the terminator.
        let slots = core::mem::size_of::<*mut c_char>() * 2;
        // Safety: zalloc is resolved and we are inside zsh.
        let array = unsafe {
            let p = (self.zalloc)(slots).cast::<*mut c_char>();
            if p.is_null() {
                return;
            }
            // The array is NULL-terminated before anything is copied into
            // it, so a failure half way through cannot leave zsh reading
            // uninitialised memory.
            (*p) = core::ptr::null_mut();
            (*p.add(1)) = core::ptr::null_mut();
            let copy = (self.ztrdup)(value.as_ptr());
            if copy.is_null() {
                return;
            }
            (*p) = copy;
            p
        };
        // Safety: inside zsh; `array` is a NULL-terminated, zsh-allocated array.
        unsafe { (self.setaparam)(name.as_ptr() as *mut c_char, array) };
    }

    /// Set an array parameter from already-`ztrdup`-allocated, NUL-terminated
    /// rows.
    ///
    /// The rows themselves must come from `ztrdup` and the row array from
    /// `zalloc`; zsh frees both. See [`Api::set_param_string`] for the
    /// ownership rules and why `zhalloc`/`dupstring` are wrong.
    ///
    /// # Safety
    ///
    /// Must only be called from inside zsh.
    pub unsafe fn set_param_array(&self, name: &std::ffi::CStr, rows: &[&std::ffi::CStr]) {
        let bytes = core::mem::size_of::<*mut c_char>() * (rows.len() + 1);
        // Safety: zalloc is resolved and we are inside zsh.
        unsafe {
            let array = (self.zalloc)(bytes).cast::<*mut c_char>();
            if array.is_null() {
                return;
            }
            // NULL-terminate the whole array first: if a copy fails half
            // way through, zsh must not read uninitialised slots.
            for i in 0..=rows.len() {
                *array.add(i) = core::ptr::null_mut();
            }
            for (i, row) in rows.iter().enumerate() {
                let copy = (self.ztrdup)(row.as_ptr());
                if copy.is_null() {
                    return;
                }
                (*array.add(i)) = copy;
            }
            (self.setaparam)(name.as_ptr() as *mut c_char, array);
        }
    }
}

/// Signature of a builtin handler, re-exported for convenience.
pub type BuiltinHandler =
    unsafe extern "C" fn(*mut c_char, *mut *mut c_char, Options, c_int) -> c_int;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_refuses_when_probe_failed() {
        // The safety contract is "we are inside zsh"; here we deliberately are
        // not, and the probe reports that, so resolution must refuse rather than
        // hand back garbage pointers.
        let caps = Capabilities::Missing { symbols: vec!["getsparam"] };
        assert!(unsafe { Api::resolve(&caps) }.is_none());
    }

    #[test]
    fn api_is_debug_and_copy() {
        fn assert_traits<T: std::fmt::Debug + Copy>() {}
        assert_traits::<Api>();
    }

    #[test]
    fn parse_i64_reads_plain_integers() {
        assert_eq!(parse_i64(b"0"), Some(0));
        assert_eq!(parse_i64(b"24"), Some(24));
        assert_eq!(parse_i64(b"7"), Some(7));
        assert_eq!(parse_i64(b"-1"), Some(-1));
        assert_eq!(parse_i64(b"-12345"), Some(-12345));
        // Leading zeros, which zsh can produce.
        assert_eq!(parse_i64(b"007"), Some(7));
    }

    #[test]
    fn parse_i64_rejects_anything_that_is_not_an_integer() {
        assert_eq!(parse_i64(b""), None);
        assert_eq!(parse_i64(b"-"), None);
        assert_eq!(parse_i64(b"12a"), None);
        assert_eq!(parse_i64(b"a12"), None);
        assert_eq!(parse_i64(b" 12"), None);
        assert_eq!(parse_i64(b"12 "), None);
        assert_eq!(parse_i64(b"+12"), None, "a leading + is not produced by zsh");
        assert_eq!(parse_i64(b"1.0"), None);
    }

    #[test]
    fn parse_i64_refuses_to_overflow() {
        // Must not wrap silently: a wrapped CURSOR would be a correctness bug
        // far worse than a slow one.
        assert_eq!(parse_i64(b"99999999999999999999"), None);
        assert_eq!(parse_i64(&b"9".repeat(18)), Some(999_999_999_999_999_999));
    }

    // The rows the fake `getaparam` returns.
    //
    // `None` stands for "no such parameter, or not an array" — zsh's own
    // answer in that case is a null pointer. `Some(vec![])` is an *empty*
    // array, which zsh answers with a one-element array whose only element is
    // `NULL`.
    thread_local! {
        static FAKE_ROWS: std::cell::RefCell<Option<Vec<&'static std::ffi::CStr>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// A stand-in for zsh's `getaparam`.
    ///
    /// The array is heap-allocated and deliberately leaked: zsh's real
    /// `getaparam` hands back memory the caller does not own either, and the
    /// code under test must copy out of it before returning.
    unsafe extern "C" fn fake_getaparam(_name: *mut c_char) -> *mut *mut c_char {
        let rows = FAKE_ROWS.with(|rows| rows.borrow().clone());
        let Some(rows) = rows else {
            return std::ptr::null_mut();
        };
        let mut array: Vec<*mut c_char> = rows
            .iter()
            .map(|row| row.as_ptr() as *mut c_char)
            .collect();
        array.push(std::ptr::null_mut());
        Box::into_raw(array.into_boxed_slice()) as *mut *mut c_char
    }

    /// A `CStr` with bytes that are not valid UTF-8.
    fn invalid_utf8_cstr() -> &'static std::ffi::CStr {
        let bytes: &'static mut [u8] =
            Box::leak(b"\xff\xfe region\0".to_vec().into_boxed_slice());
        std::ffi::CStr::from_bytes_with_nul(bytes).expect("fixture ends with a NUL")
    }

    #[test]
    fn get_param_array_copies_every_element() {
        FAKE_ROWS.with(|rows| {
            *rows.borrow_mut() = Some(vec![c"0 12 red", c"5 20 underline"]);
        });
        let mut out = Vec::new();
        // Safety: the fake stands in for zsh's `getaparam`.
        let read = unsafe { read_array(Some(fake_getaparam), c"region_highlight", &mut out) };
        assert!(read, "an existing array must be reported as read");
        assert_eq!(out, vec!["0 12 red".to_string(), "5 20 underline".to_string()]);
    }

    #[test]
    fn get_param_array_clears_the_output_before_reading() {
        FAKE_ROWS.with(|rows| {
            *rows.borrow_mut() = Some(vec![c"0 12 red"]);
        });
        let mut out = vec!["stale".to_string()];
        // Safety: the fake stands in for zsh's `getaparam`.
        let read = unsafe { read_array(Some(fake_getaparam), c"region_highlight", &mut out) };
        assert!(read);
        assert_eq!(out, vec!["0 12 red".to_string()]);
    }

    #[test]
    fn get_param_array_reports_a_parameter_that_is_not_an_array() {
        FAKE_ROWS.with(|rows| {
            *rows.borrow_mut() = None;
        });
        let mut out = vec!["stale".to_string()];
        // Safety: the fake stands in for zsh's `getaparam`.
        let read = unsafe { read_array(Some(fake_getaparam), c"region_highlight", &mut out) };
        assert!(!read, "a missing parameter is not a successful read");
        assert!(out.is_empty(), "a failed read must not leave stale rows");
    }

    #[test]
    fn get_param_array_reports_an_empty_array_as_a_successful_read() {
        // zsh answers an empty array with a one-element array whose only
        // element is NULL. That is a *successful* read of zero rows, and it
        // must be distinguishable from "not an array".
        FAKE_ROWS.with(|rows| {
            *rows.borrow_mut() = Some(Vec::new());
        });
        let mut out = Vec::new();
        // Safety: the fake stands in for zsh's `getaparam`.
        let read = unsafe { read_array(Some(fake_getaparam), c"region_highlight", &mut out) };
        assert!(read);
        assert!(out.is_empty());
    }

    #[test]
    fn get_param_array_fails_when_the_symbol_is_missing() {
        // `getaparam` is optional: a zsh without it must still load the
        // module, and the module falls back to replacing the array.
        let mut out = vec!["stale".to_string()];
        // Safety: there is no zsh call to make.
        let read = unsafe { read_array(None, c"region_highlight", &mut out) };
        assert!(!read);
        assert!(out.is_empty());
    }

    #[test]
    fn get_param_array_never_panics_on_invalid_utf8() {
        let bad = invalid_utf8_cstr();
        FAKE_ROWS.with(|rows| {
            *rows.borrow_mut() = Some(vec![bad]);
        });
        let mut out = Vec::new();
        // Safety: the fake stands in for zsh's `getaparam`.
        let read = unsafe { read_array(Some(fake_getaparam), c"region_highlight", &mut out) };
        assert!(read);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].contains('\u{fffd}'),
            "invalid bytes must become the replacement character, not a panic"
        );
    }

    #[test]
    fn getaparam_is_not_a_required_symbol() {
        // The module must load on a zsh that does not export `getaparam`;
        // it only loses the ability to read an array back.
        assert!(!crate::probe::REQUIRED_SYMBOLS.contains(&GETAPARAM));
        assert_eq!(GETAPARAM, "getaparam");
    }
}
