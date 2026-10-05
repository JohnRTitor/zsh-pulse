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

/// `char *dupstring(char const *)` — allocate with zsh's allocator.
/// (`zsh-src/Src/params.c`, `zsh-src/Src/utils.c`)
pub type DupStringFn = unsafe extern "C" fn(*const c_char) -> ZshString;

/// `void *zhalloc(size_t)` — zsh's allocator (`zsh-src/Src/mem.c`).
pub type ZhAllocFn = unsafe extern "C" fn(usize) -> *mut core::ffi::c_void;

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
    /// `dupstring`
    pub dupstring: DupStringFn,
    /// `zhalloc`
    pub zhalloc: ZhAllocFn,
}

impl Api {
    /// Resolve every entry point. Returns `None` if any symbol is absent.
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
                dupstring: fn_ptr(crate::raw::zsh::DUPSTRING)?,
                zhalloc: fn_ptr(crate::raw::zsh::ZHALLOC)?,
            })
        }
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
    /// (`zsh-src/Src/params.c:3318`) calls `freearray(val)` on every exit path,
    /// including the success path. Passing a Rust stack array therefore makes
    /// zsh `free()` a stack pointer: this was observed to **segfault zsh**
    /// during Stage B.
    ///
    /// Both the array and its strings must therefore be allocated by zsh:
    /// `zhalloc` for the array, `dupstring` for each element.
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
        // SAFETY: zhalloc is resolved and we are inside zsh.
        let array = unsafe {
            let p = (self.zhalloc)(slots).cast::<*mut c_char>();
            if p.is_null() {
                return;
            }
            (*p) = (self.dupstring)(value.as_ptr());
            (*p.add(1)) = core::ptr::null_mut();
            p
        };
        // Safety: inside zsh; `array` is a NULL-terminated, zsh-allocated array.
        unsafe { (self.setaparam)(name.as_ptr() as *mut c_char, array) };
    }

    /// Set an array parameter from already-zsh-allocated, NUL-terminated rows.
    ///
    /// Each row must come from `dupstring`, and `rows` itself from `zhalloc`;
    /// zsh frees both. See [`Api::set_param_string`] for the ownership rules.
    ///
    /// # Safety
    ///
    /// Must only be called from inside zsh.
    pub unsafe fn set_param_array(&self, name: &std::ffi::CStr, rows: &[&std::ffi::CStr]) {
        let bytes = core::mem::size_of::<*mut c_char>() * (rows.len() + 1);
        // Safety: zhalloc is resolved and we are inside zsh.
        unsafe {
            let array = (self.zhalloc)(bytes).cast::<*mut c_char>();
            if array.is_null() {
                return;
            }
            for (i, row) in rows.iter().enumerate() {
                (*array.add(i)) = (self.dupstring)(row.as_ptr());
            }
            (*array.add(rows.len())) = core::ptr::null_mut();
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
}
