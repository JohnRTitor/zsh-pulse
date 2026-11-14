//! Completion and autosuggestion builtin: `zsh-pulse-complete`.

use zsh_abi::{guard, raw::CStr};

use crate::{char_offsets_into, engine, zsh};

/// The completion builtin name: `zsh-pulse-complete`.
pub static BUILTIN_COMPLETE: CStr = CStr::new(c"zsh-pulse-complete");

/// `zsh-pulse-complete` — computes completion candidates and ghost text.
///
/// It reads live `BUFFER` and `CURSOR` from ZLE, queries the engine,
/// and writes the results to Zsh parameters:
/// - `_zsh_pulse_matches`: candidate values to insert
/// - `_zsh_pulse_descriptions`: candidate formatted descriptions for menus
/// - `_zsh_pulse_autosuggestion`: inline ghost text suffix
pub unsafe extern "C" fn complete_builtin(
    _nam: *mut std::ffi::c_char,
    args: *mut *mut std::ffi::c_char,
    _ops: zsh_abi::Options,
    _len: std::ffi::c_int,
) -> std::ffi::c_int {
    guard(0, || {
        if zsh_abi::is_degraded() {
            return 0;
        }

        let Some(api) = zsh() else {
            return 0;
        };

        // Parse optional arguments
        let mut max_candidates = 16usize;
        let mut histfile_arg: Option<std::path::PathBuf> = None;
        let mut autosuggest_only = false;
        let mut history_only = false;
        let mut paths_only = false;

        if !args.is_null() {
            // Safety: args is handed directly by zsh to the builtin handler.
            let iter = unsafe { crate::RawArgIter::new(args) };
            for arg in iter {
                let bytes = arg.to_bytes();
                if let Some(feat) = bytes.strip_prefix(b"--has-feature=") {
                    let has = (feat == b"highlight" && cfg!(feature = "highlight"))
                        || (feat == b"autocomplete" && cfg!(feature = "autocomplete"))
                        || ((feat == b"history" || feat == b"history-navigation")
                            && cfg!(any(feature = "history", feature = "history-navigation")));
                    return if has { 0 } else { 1 };
                } else if let Some(num) = bytes.strip_prefix(b"--max=") {
                    if let Ok(s) = std::str::from_utf8(num) {
                        if let Ok(n) = s.parse::<usize>() {
                            max_candidates = n.clamp(1, 128);
                        }
                    }
                } else if let Some(path) = bytes.strip_prefix(b"--histfile=") {
                    if let Ok(s) = std::str::from_utf8(path) {
                        histfile_arg = Some(std::path::PathBuf::from(s));
                    }
                } else if bytes == b"--is-menu-active" {
                    return if api.is_menu_active() { 0 } else { 1 };
                } else if bytes == b"--autosuggest" {
                    autosuggest_only = true;
                } else if bytes == b"--history-only" {
                    history_only = true;
                } else if bytes == b"--paths-only" {
                    paths_only = true;
                }
            }
        }

        let mut buffer = Vec::new();
        let _ = api.read_param_bytes(c"BUFFER", &mut buffer);

        let mut scratch = Vec::new();
        let cursor = api.read_param_i64(c"CURSOR", &mut scratch).unwrap_or(0);
        let c_val = cursor.max(0);
        let cursor_idx = if buffer.is_ascii() {
            usize::try_from(c_val).unwrap_or(0).min(buffer.len())
        } else {
            let mut offsets = Vec::new();
            char_offsets_into(&mut offsets, &buffer);
            offsets
                .partition_point(|&chars| (chars as i64) < c_val)
                .min(buffer.len())
        };

        let cwd = api
            .get_param_string(c"PWD")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"))
            });
        let home = api
            .get_param_string(c"HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/home"));

        #[cfg(any(feature = "history", feature = "history-navigation"))]
        if let Some(hf) = histfile_arg {
            engine().ensure_history_loaded(&hf);
        } else if let Some(hf_str) = api.get_param_string(c"HISTFILE") {
            engine().ensure_history_loaded(std::path::Path::new(&hf_str));
        }

        #[cfg(not(any(feature = "history", feature = "history-navigation")))]
        let _ = histfile_arg;

        #[cfg(feature = "autocomplete")]
        if autosuggest_only {
            let ghost = engine().autosuggest(&buffer, cursor_idx, cwd, home);
            let ghost_cstr = match ghost {
                Some(ref s) => std::ffi::CString::new(s.as_bytes()).unwrap_or_default(),
                None => std::ffi::CString::default(),
            };
            api.set_param_string(c"_zsh_pulse_autosuggestion", &ghost_cstr);
            return 0;
        }

        #[cfg(not(feature = "autocomplete"))]
        if autosuggest_only {
            return 0;
        }

        let outcome = {
            #[cfg(any(feature = "history", feature = "history-navigation"))]
            if history_only {
                engine().complete_history(&buffer, cursor_idx, cwd, home, max_candidates)
            } else {
                #[cfg(feature = "autocomplete")]
                {
                    if paths_only {
                        engine().complete_paths(&buffer, cursor_idx, cwd, home, max_candidates)
                    } else {
                        engine().complete(&buffer, cursor_idx, cwd, home, max_candidates)
                    }
                }
                #[cfg(not(feature = "autocomplete"))]
                {
                    let _ = (paths_only, max_candidates);
                    completion::CompletionOutcome {
                        candidates: Vec::new(),
                        autosuggestion: None,
                        word_start: 0,
                        word_end: 0,
                    }
                }
            }

            #[cfg(not(any(feature = "history", feature = "history-navigation")))]
            {
                let _ = history_only;
                if paths_only {
                    engine().complete_paths(&buffer, cursor_idx, cwd, home, max_candidates)
                } else {
                    engine().complete(&buffer, cursor_idx, cwd, home, max_candidates)
                }
            }
        };

        let mut match_cstrs = Vec::with_capacity(outcome.candidates.len());
        let mut desc_cstrs = Vec::with_capacity(outcome.candidates.len());
        let mut kind_cstrs = Vec::with_capacity(outcome.candidates.len());

        for c in &outcome.candidates {
            if let Ok(m) = std::ffi::CString::new(c.value.as_bytes()) {
                let desc_fmt = format!("{:<20} -- {}", c.display, c.description);
                let d = std::ffi::CString::new(desc_fmt.as_bytes()).unwrap_or_default();
                let k = std::ffi::CString::new(c.kind.as_str()).unwrap_or_default();
                match_cstrs.push(m);
                desc_cstrs.push(d);
                kind_cstrs.push(k);
            }
        }

        let match_refs: Vec<&std::ffi::CStr> = match_cstrs.iter().map(|c| c.as_c_str()).collect();
        let desc_refs: Vec<&std::ffi::CStr> = desc_cstrs.iter().map(|c| c.as_c_str()).collect();
        let kind_refs: Vec<&std::ffi::CStr> = kind_cstrs.iter().map(|c| c.as_c_str()).collect();

        api.set_param_array(c"_zsh_pulse_matches", &match_refs);
        api.set_param_array(c"_zsh_pulse_descriptions", &desc_refs);
        api.set_param_array(c"_zsh_pulse_kinds", &kind_refs);

        let ghost_cstr = match outcome.autosuggestion {
            Some(ref s) => std::ffi::CString::new(s.as_bytes()).unwrap_or_default(),
            None => std::ffi::CString::default(),
        };
        api.set_param_string(c"_zsh_pulse_autosuggestion", &ghost_cstr);

        0
    })
}
