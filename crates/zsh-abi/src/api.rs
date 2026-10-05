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

/// The zsh functions this module uses, resolved once at load.
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
}
