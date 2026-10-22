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
use std::time::{Duration, Instant};

use engine::EngineState;
use highlight::{char_offsets_into, SpanSet};
use lexer::LexCache;
use zsh_abi::raw::{Builtin, CStr, Module};
use zsh_abi::{Api, CappedRead, guard, probe};

pub mod region;

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

/// The wall-clock budget for one keystroke.
///
/// The design target is ten microseconds per keystroke, so a millisecond is
/// a hundred times the budget: anything slower than that is not worth
/// showing, and pushing a late highlight makes the display flicker.
const BUDGET: Duration = Duration::from_millis(1);

/// The longest line that is worth highlighting.
///
/// The budget is a millisecond, and the measured cost of the highlighting
/// path is a few microseconds for a normal line, so a limit of a few
/// kilobytes leaves a very large margin. A command line longer than this is
/// almost always a paste, and highlighting it would cost more than the
/// result is worth.
const MAX_LINE: usize = 4 * 1024;

/// The state the highlighter keeps between keystrokes.
///
/// Everything in here is reused: an allocation would happen on the
/// interactive thread, between the user's keystroke and the redraw. The
/// buffers are swapped rather than copied, and the region entries are
/// built in one reused byte buffer, so a steady-state keystroke allocates
/// only the small vector of row pointers zsh is handed.
#[derive(Debug, Default)]
struct Highlighter {
    /// The incremental lexer.
    cache: LexCache,
    /// The spans of the line as it is now.
    spans: SpanSet,
    /// The spans that were last handed to zsh.
    ///
    /// The two are swapped rather than copied, so a keystroke does not
    /// allocate: the span buffer is reused whichever way the comparison goes.
    pushed_spans: SpanSet,
    /// The `region_highlight` entries of the last line.
    entries: region::Entries,
    /// The byte-to-character map of the last line.
    ///
    /// Only built for lines that are not pure ASCII, where byte offsets and
    /// character offsets differ.
    offsets: Vec<u32>,
    /// Whether zsh currently holds the entries.
    ///
    /// The ZLE special parameters are created fresh for every widget call
    /// (`zsh-src/Src/Zle/zle_main.c:1533`), so the *parameter* is always
    /// empty when the builtin runs. The C array that actually drives the
    /// display (`zsh-src/Src/Zle/zle_refresh.c:227`) is not — but it is
    /// emptied at the end of every line
    /// (`zsh-src/Src/Zle/zle_main.c:1398`), so the flag is reset when a new
    /// line starts.
    pushed: bool,
}

impl Highlighter {
    /// Highlight `buffer` and, if anything changed, hand the result to zsh.
    ///
    /// `publish` says whether the caller wants the highlights written to
    /// `region_highlight` at all. Only the ZLE hook asks for it: writing to
    /// that parameter outside a widget would either do nothing (the
    /// parameter does not exist) or, worse, overwrite a global array of the
    /// same name that the user or another plugin owns.
    ///
    /// `too_long` says the line in zsh is longer than [`MAX_LINE`]. Such a
    /// line is not highlighted, and its bytes were not copied, so `buffer`
    /// is empty.
    ///
    /// # Safety
    ///
    /// `api` must be the resolved zsh API and the call must happen inside
    /// zsh.
    unsafe fn update(&mut self, api: &Api, buffer: &[u8], too_long: bool, publish: bool) {
        if too_long {
            // Too long to be worth highlighting. The highlights that are on
            // screen describe the *previous* line, so they must go: leaving
            // them would show the styling of a line that is no longer there.
            // Safety: inside zsh.
            unsafe { self.clear(api, publish) };
            return;
        }

        let started = Instant::now();

        // Re-lex. A line the lexer cannot represent exactly (too deeply
        // nested, an impossible here-document) is left unstyled rather than
        // styled wrongly.
        let outcome = match self.cache.relex(buffer) {
            Ok(o) => o,
            Err(_) => {
                // Safety: inside zsh.
                unsafe { self.clear(api, publish) };
                return;
            }
        };
        if outcome.unchanged && self.pushed {
            // The line has not changed since the last redraw, and zsh
            // already has the highlights. Nothing to do.
            return;
        }

        self.spans.update_from_tokens(self.cache.tokens());
        if self.spans == self.pushed_spans && self.pushed {
            // The styling has not changed, so the regions zsh holds are
            // still correct. Leaving them alone is the whole point of the
            // "skip entirely when unchanged" rule.
            return;
        }

        if !publish || !unsafe { api.in_zle() } {
            // `WIDGET` is created by `makezleparams`
            // (`zsh-src/Src/Zle/zle_params.c:141-188`) and exists only while
            // a widget is running. It is a *scalar*, so testing it is O(1);
            // testing `region_highlight` instead would materialise the whole
            // region array on every keystroke.
            //
            // Checking it also proves that the `region_highlight` the module
            // writes to is the ZLE special parameter and not a global array
            // of that name that the user or another plugin owns.
            self.pushed = false;
            return;
        }

        // The byte-to-character map is only needed when the line is not
        // pure ASCII: for ASCII, byte offsets *are* character offsets, and
        // building the map would cost more than the whole lexer.
        let ascii = buffer.is_ascii();
        if !ascii {
            char_offsets_into(&mut self.offsets, buffer);
        }
        let offsets = if ascii { None } else { Some(self.offsets.as_slice()) };

        if *debug_enabled() {
            // The diff is the "what changed" report: which ranges of the
            // line are styled differently from the last version.
            let changes = highlight::diff(&self.pushed_spans, &self.spans);
            let ranges: Vec<String> = changes
                .iter()
                .map(|c| format!("{}..{}", c.start, c.end))
                .collect();
            debug(&format!("{} range(s) restyled: {}", changes.len(), ranges.join(" ")));
        }

        let truncated = region::Entries::build(&mut self.entries, self.spans.spans(), offsets);
        if truncated && *debug_enabled() {
            debug("too many highlighted regions; the highlighting was truncated");
        }

        if started.elapsed() > BUDGET {
            // The line is too expensive to highlight right now. The cache
            // has been updated, so the next keystroke will be cheap; this
            // one is simply left unhighlighted rather than highlighted late.
            //
            // The highlights on screen describe the previous line, so they
            // are cleared rather than left to lie about the current one.
            if *debug_enabled() {
                debug("highlighting exceeded the budget; skipping this redraw");
            }
            // Safety: inside zsh.
            unsafe { self.clear(api, publish) };
            return;
        }

        if *debug_enabled() {
            debug(&format!("published {} regions", self.entries.len()));
        }
        let rows = self.entries.rows();
        // Safety: inside zsh, and the rows are NUL-terminated.
        unsafe { api.set_param_array(cstr(b"region_highlight\0"), &rows) };

        // The spans that were just published become the reference for the
        // next comparison; the old ones become the scratch.
        core::mem::swap(&mut self.spans, &mut self.pushed_spans);
        self.pushed = true;
    }

    /// Forget what was published, without touching the display.
    ///
    /// zsh empties the display array at the end of every line
    /// (`zsh-src/Src/Zle/zle_main.c:1398`), so the module must not believe
    /// it still holds the regions it published for the previous line.
    fn forget(&mut self) {
        self.pushed = false;
        self.spans.clear();
        self.pushed_spans.clear();
    }

    /// Remove every highlight from the display.
    ///
    /// # Safety
    ///
    /// Must be called inside zsh.
    unsafe fn clear(&mut self, api: &Api, publish: bool) {
        // The buffers are emptied, not replaced: a bail-out is usually
        // followed by an ordinary keystroke, and that keystroke must not have
        // to reallocate.
        self.spans.clear();
        self.pushed_spans.clear();
        self.entries.clear();
        if !self.pushed {
            // Nothing was ever published, so there is nothing to clear.
            return;
        }
        self.pushed = false;
        if !publish || !unsafe { api.in_zle() } {
            return;
        }
        // Safety: inside zsh; an empty array clears the highlights.
        unsafe { api.set_param_array(cstr(b"region_highlight\0"), &[]) };
    }
}

/// The process-wide highlighter.
static HIGHLIGHTER: std::sync::LazyLock<std::sync::Mutex<Highlighter>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(Highlighter::default()));

/// Reusable scratch buffers for the keystroke path.
///
/// Allocated once, then reused. This is what removed the per-keystroke
/// allocations that made the first implementation 10x slower than the C
/// proof-of-concept (1.2 us vs 112 ns per call).
///
/// Reused, but not for ever: the buffer only ever holds a line that the
/// module is willing to highlight — at most [`MAX_LINE`] bytes — and
/// capacity beyond that is handed back (see
/// [`Scratch::release_unused_capacity`]).
struct Scratch {
    /// Unmetafied `BUFFER` bytes.
    ///
    /// Empty when the line is longer than [`MAX_LINE`]: such a line is not
    /// highlighted, so its bytes are not needed and are not copied.
    buffer: Vec<u8>,
    /// Raw bytes of `CURSOR`, kept only for diagnostics.
    ///
    /// Bounded by the number of decimal digits in a cursor position, so
    /// this one cannot grow without bound.
    cursor_bytes: Vec<u8>,
    /// The length of the line in zsh.
    ///
    /// Known even when the line was too long to copy, so the diagnostics
    /// can report the real size of the line.
    line_len: usize,
    /// Whether the line is longer than [`MAX_LINE`].
    ///
    /// The module does not highlight such a line, and does not copy it
    /// either: copying it is what made the scratch grow to the largest
    /// line the user had ever pasted.
    too_long: bool,
}

impl Scratch {
    const fn new() -> Self {
        Self {
            buffer: Vec::new(),
            cursor_bytes: Vec::new(),
            line_len: 0,
            too_long: false,
        }
    }

    /// Take a line into the scratch, if it is short enough to highlight.
    ///
    /// Record the line that was read from zsh.
    ///
    /// `too_long` says the line was longer than [`MAX_LINE`] and was
    /// therefore not copied: the length is still known, but the bytes are
    /// not kept, so the scratch cannot grow to the size of a paste.
    fn set_line(&mut self, len: usize, too_long: bool) {
        self.line_len = len;
        self.too_long = too_long;
        if too_long {
            self.buffer.clear();
        }
        self.release_unused_capacity();
    }

    /// Release the capacity the scratch no longer needs.
    ///
    /// `Vec::clear` keeps the capacity, so without this the shell would
    /// hold the largest line the user has ever pasted for the rest of the
    /// session.
    ///
    /// Shrinking on every keystroke would be worse than the problem it
    /// solves: `shrink_to` reallocates, and an allocation on the
    /// interactive thread is exactly what the scratch exists to avoid. The
    /// rule is therefore hysteretic — the capacity is released only when it
    /// is large in absolute terms *and* much larger than the line the
    /// scratch currently holds — so a line that grows and shrinks within
    /// the retained capacity never triggers a reallocation.
    fn release_unused_capacity(&mut self) {
        let capacity = self.buffer.capacity();
        // Keep enough room for the line itself and for one a few times
        // longer, so that ordinary editing never reallocates.
        let keep = SCRATCH_KEEP.max(self.buffer.len().saturating_mul(SCRATCH_KEEP_RATIO));
        if capacity > keep {
            self.buffer.shrink_to(SCRATCH_KEEP);
        }
    }

    /// Read `BUFFER` out of zsh into the scratch.
    ///
    /// This is `zsh_abi::Api::read_param_bytes` with a cap. That one copies
    /// the whole parameter, and the whole parameter of a pasted line is
    /// precisely what must not be copied: the module does not highlight a
    /// line longer than [`MAX_LINE`], so it needs the *length* of such a
    /// line and nothing else.
    ///
    /// `want_whole` asks for the whole line anyway, for the diagnostic
    /// modes (`SHELL_PULSE_DEBUG` and `SHELL_PULSE_ECHO`), which show the
    /// user exactly what is on the line. The copy is returned to the
    /// caller, who drops it after printing: the scratch stays small.
    ///
    /// # Safety
    ///
    /// Must be called inside zsh.
    unsafe fn read_buffer(&mut self, api: &Api, want_whole: bool) -> Option<Vec<u8>> {
        // The line is copied only if it is short enough to highlight; a
        // pasted megabyte must not be copied into the shell's memory.
        //
        // Safety: we are inside zsh, and the name is NUL-terminated.
        let read =
            unsafe { api.read_param_bytes_capped(cstr(b"BUFFER\0"), &mut self.buffer, MAX_LINE) };
        match read {
            CappedRead::Missing => {
                self.set_line(0, false);
                None
            }
            CappedRead::TooLong(len) => {
                self.set_line(len, true);
                if want_whole {
                    // The diagnostic modes want to show the whole line, but
                    // the scratch must stay small, so the copy is made into a
                    // buffer the caller throws away.
                    //
                    // Safety: we are inside zsh, and the name is NUL-terminated.
                    let p = unsafe {
                        (api.getsparam_u)(cstr(b"BUFFER\0").as_ptr() as *mut std::ffi::c_char)
                    };
                    if !p.is_null() {
                        // Safety: zsh NUL-terminates the static unmetafied buffer.
                        let len = unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes().len();
                        // Safety: `p` points at `len` readable bytes, just measured.
                        let bytes = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), len) };
                        return Some(bytes.to_vec());
                    }
                }
                None
            }
            CappedRead::Read(len) => {
                self.set_line(len, false);
                None
            }
        }
    }
}

/// The capacity the scratch keeps for itself.
///
/// A kilobyte is more than any command line worth highlighting needs: the
/// module refuses to highlight anything longer than [`MAX_LINE`], which is
/// four times this.
const SCRATCH_KEEP: usize = 1024;

/// How much bigger than the line the capacity may be before it is released.
///
/// Four times the line is comfortably more than a command line needs, so
/// the scratch does not reallocate while the user is editing one.
const SCRATCH_KEEP_RATIO: usize = 4;

static SCRATCH: std::sync::Mutex<Scratch> = std::sync::Mutex::new(Scratch::new());

/// `shell-pulse-highlight` — invoked once per redraw by the zsh adapter.
///
/// It reads the live ZLE state, highlights the line and hands the result to
/// zsh through `region_highlight`.
unsafe extern "C" fn highlight_builtin(
    _nam: *mut std::ffi::c_char,
    args: *mut *mut std::ffi::c_char,
    _ops: zsh_abi::Options,
    _len: std::ffi::c_int,
) -> std::ffi::c_int {
    guard(0, || {
        // A panic has already been caught at this boundary. Continuing would
        // panic again on the same input, and the default panic hook would
        // print a backtrace into the user's terminal on every keystroke.
        if zsh_abi::is_degraded() {
            return 0;
        }

        // Rule 1: never touch zsh before the API has been resolved.
        let Some(api) = zsh() else {
            return 0;
        };

        // Only the ZLE hook asks for the highlights to be published.
        let publish = wants_regions(args);

        if wants_reset(args) {
            // A new line is starting: zsh has emptied the display array, so
            // the module must forget that it published anything.
            let mut highlighter = match HIGHLIGHTER.lock() {
                Ok(h) => h,
                Err(poisoned) => poisoned.into_inner(),
            };
            highlighter.forget();
            return 0;
        }

        // Read live ZLE state into reused buffers: no allocation once warm.
        let mut guard = match SCRATCH.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        // The diagnostic modes show the user the whole line, even one that
        // is too long to highlight, so they are the only ones that pay for
        // a copy of it.
        let want_whole = *debug_enabled() || *echo_enabled();
        // Safety: `api` exists only because we are inside zsh.
        let (cursor, whole) = unsafe {
            let bytes = api.read_param_i64(cstr(b"CURSOR\0"), &mut guard.cursor_bytes);
            // CURSOR must be read *before* BUFFER, because both share zsh's
            // single static unmetafied buffer.
            (bytes, guard.read_buffer(api, want_whole))
        };
        let buffer_len = guard.line_len;
        let too_long = guard.too_long;
        // The line as zsh has it: the whole of it for a line that is too
        // long to highlight, the scratch's copy otherwise.
        let line: &[u8] = whole.as_deref().unwrap_or(&guard.buffer);

        if *debug_enabled() {
            debug(&format!(
                "read BUFFER={:?} ({buffer_len} bytes) CURSOR={cursor:?}",
                String::from_utf8_lossy(line)
            ));
        }

        // Each keystroke advances the line revision; asynchronous results
        // tagged with an older revision are refused.
        let revision = engine().record_highlight();

        observe(revision, cursor.unwrap_or_default());

        // Highlight the line and hand the result to zsh.
        {
            let mut highlighter = match HIGHLIGHTER.lock() {
                Ok(h) => h,
                Err(poisoned) => poisoned.into_inner(),
            };
            // Safety: `api` is resolved and we are inside zsh.
            unsafe { highlighter.update(api, &guard.buffer, too_long, publish) };
        }

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
                    String::from_utf8_lossy(line)
                ))
                .unwrap_or_default();
                api.set_param_string(cstr(b"SHELL_PULSE_STATE\0"), &value);
            }
        }
        0
    })
}

/// The argument that asks the module to publish the highlights.
const REGION_ARGUMENT: &[u8] = b"--region-highlight";

/// The argument that asks the module to forget what it published.
///
/// zsh empties the display array at the end of every line
/// (`zsh-src/Src/Zle/zle_main.c:1398`), so the module has to forget too —
/// otherwise a recalled command, whose text matches the cached one, would be
/// drawn with no highlighting at all.
const RESET_ARGUMENT: &[u8] = b"--reset";

/// The most arguments the builtin will look at.
///
/// The argument vector is NUL-terminated, but a bound is kept anyway: a
/// runaway loop inside the user's shell is not worth the risk.
const MAX_ARGUMENTS: usize = 64;

/// Whether the caller asked for the highlights to be published.
///
/// Only the ZLE hook passes the flag. Writing to `region_highlight` from
/// anywhere else would either do nothing (the parameter only exists while
/// a widget is running) or overwrite a global array of that name that the
/// user or another plugin owns.
fn wants_reset(args: *mut *mut std::ffi::c_char) -> bool {
    has_argument(args, RESET_ARGUMENT)
}

fn wants_regions(args: *mut *mut std::ffi::c_char) -> bool {
    has_argument(args, REGION_ARGUMENT)
}

/// Whether the argument vector contains `wanted`.
fn has_argument(args: *mut *mut std::ffi::c_char, wanted: &[u8]) -> bool {
    if args.is_null() {
        return false;
    }
    for i in 0..MAX_ARGUMENTS {
        // Safety: the argument vector is NUL-terminated, and the loop is
        // bounded.
        let p = unsafe { *args.add(i) };
        if p.is_null() {
            return false;
        }
        // Safety: zsh passes NUL-terminated argument strings.
        let s = unsafe { std::ffi::CStr::from_ptr(p) };
        if s.to_bytes() == wanted {
            return true;
        }
    }
    false
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

    // --- the scratch buffer -------------------------------------------------

    #[test]
    fn a_line_at_the_limit_is_copied() {
        let mut s = Scratch::new();
        // The read fills the buffer; the scratch records what was read.
        s.buffer.extend_from_slice(&vec![b'x'; MAX_LINE]);
        s.set_line(MAX_LINE, false);
        assert_eq!(s.buffer.len(), MAX_LINE);
        assert!(!s.too_long);
        assert_eq!(s.line_len, MAX_LINE);
    }

    #[test]
    fn a_line_that_is_too_long_is_not_copied() {
        // The whole point: a line the module will not highlight must not be
        // copied into the scratch, or the scratch grows to the largest line
        // the user has ever pasted and never gives it back.
        let mut s = Scratch::new();
        let line = vec![b'x'; MAX_LINE * 4];
        s.set_line(line.len(), true);
        assert_eq!(s.buffer.len(), 0, "the line must not be copied");
        assert!(s.too_long);
        // The length is still known: the module needs it to know that the
        // line must be skipped, and the diagnostics report it.
        assert_eq!(s.line_len, line.len());
        assert!(
            s.buffer.capacity() < line.len(),
            "the scratch must not have grown to the size of the line"
        );
    }

    #[test]
    fn a_short_line_after_a_long_one_is_highlighted_again() {
        let mut s = Scratch::new();
        s.set_line(MAX_LINE * 2, true);
        assert!(s.too_long);
        s.buffer.extend_from_slice(b"ls");
        s.set_line(2, false);
        assert!(!s.too_long, "the flag must be cleared once the line fits again");
        assert_eq!(s.buffer.len(), 2);
    }

    #[test]
    fn the_scratch_releases_capacity_it_no_longer_needs() {
        let mut s = Scratch::new();
        s.buffer.extend_from_slice(&vec![b'x'; MAX_LINE]);
        s.set_line(MAX_LINE, false);
        let grown = s.buffer.capacity();
        assert!(grown >= MAX_LINE);

        // The line goes away; the capacity must not.
        s.buffer.clear();
        s.buffer.extend_from_slice(b"ls");
        s.set_line(2, false);
        assert_eq!(s.buffer.len(), 2);
        assert!(
            s.buffer.capacity() < grown,
            "the scratch must give back the capacity of a line it no longer holds"
        );
        assert!(
            s.buffer.capacity() >= s.buffer.len(),
            "the scratch must keep enough room for the line it holds"
        );
    }

    #[test]
    fn the_scratch_does_not_reallocate_for_every_keystroke() {
        // The shrink must be rare: a reallocation on the interactive thread
        // is exactly what the scratch exists to avoid.
        let mut s = Scratch::new();
        s.buffer.clear();
        s.buffer.extend_from_slice(&vec![b'x'; 512]);
        s.set_line(512, false);
        let capacity = s.buffer.capacity();
        for _ in 0..100 {
            s.buffer.clear();
            s.buffer.extend_from_slice(b"echo hi");
            s.set_line(7, false);
        }
        assert_eq!(
            s.buffer.capacity(),
            capacity,
            "a short line must not make the scratch reallocate"
        );
    }

    #[test]
    fn the_scratch_keeps_the_capacity_a_line_needs() {
        let mut s = Scratch::new();
        s.buffer.extend_from_slice(&vec![b'x'; 2048]);
        s.set_line(2048, false);
        let capacity = s.buffer.capacity();
        // The same line again: nothing changed, so nothing may be
        // reallocated.
        s.set_line(2048, false);
        assert_eq!(s.buffer.capacity(), capacity);
        // A line that is still within the retained capacity is fine too.
        s.buffer.clear();
        s.buffer.extend_from_slice(&vec![b'x'; 1024]);
        s.set_line(1024, false);
        assert_eq!(
            s.buffer.capacity(),
            capacity,
            "a line that fits in the retained capacity must not reallocate"
        );
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
    guard(0, || {
        // Stage B7: can we run a worker thread inside a real zsh process?
        //
        // Started lazily here rather than at load time so a shell that never
        // invokes the engine pays nothing, and started only when the module
        // actually loads successfully.
        if let Some(w) = engine::worker::shared() {
            // Round-trip once immediately, so a broken worker is discovered at
            // load time rather than on a user's first completion request.
            if let Ok(reply) = w.submit(
                0,
                b"shell-pulse-probe".to_vec(),
                std::time::Duration::from_millis(200),
            ) {
                debug(&format!("worker ready: rev={} checksum={}", reply.revision, reply.checksum));
            }
        } else {
            // A supported outcome: the engine runs single-threaded.
            debug("worker unavailable; continuing single-threaded");
        }
        0
    })
}

/// Module cleanup: the unload hook.
///
/// zsh calls this when the module is unloaded (`zmodload -u`).
///
/// The worker thread must not outlive the module. The module is loaded into
/// the user's *interactive* shell, which keeps running long after the plugin
/// is unloaded, so a thread left behind would run for the rest of the
/// session — and a reloaded module would find a dead worker where it
/// expected a live one.
///
/// [`engine::worker::shutdown_shared`] stops the thread *and* joins it, so
/// the thread is gone by the time this returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cleanup_(_m: Module) -> std::ffi::c_int {
    guard(0, || {
        debug("cleanup_: stopping the worker");
        engine::worker::shutdown_shared();
        0
    })
}

/// Module finish: the finalisation hook.
///
/// zsh calls this when the module is finalised, which includes the shell
/// exiting. The worker is stopped here as well as in [`cleanup_`]: either
/// hook may run without the other, and
/// [`engine::worker::shutdown_shared`] is idempotent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finish_(_m: Module) -> std::ffi::c_int {
    guard(0, || {
        debug("finish_: stopping the worker");
        engine::worker::shutdown_shared();
        0
    })
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