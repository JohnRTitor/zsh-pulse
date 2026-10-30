# Historical Development Milestones & Task Archive

> **Archive Notice**: This document consolidates the completed task logs and development milestones from the initial research spikes (Stages B–E) through the parity hardening phases (Phases 1–14). It is preserved here for architectural provenance and historical reference.

---

## Part I: Parity Hardening & Completion Milestones (Phases 1–14)

### Phase 1: Hybrid Completer Integration (Full Tool Support)
- [x] Configure Zsh completer pipeline: prepend `_zsh_pulse_completer` before `_complete` and `_approximate`
- [x] Bounded filesystem paths scanner in Rust (`complete_paths`) to prevent freezes on massive directories (e.g., `/nix/store`)
- [x] Category grouping & descriptions (`[directories]`, `[files]`, `[history search]`, `[commands]`, `[options]`)
- [x] Fallthrough preservation of 1,000+ native Zsh tool completers (`git`, `cargo`, `docker`, `nix`, flags)

### Phase 2: Menu Navigation & Real-time Type-Ahead
- [x] Implement `down-line-or-select` widget for `↓` and `Ctrl-N` (enter menu selection immediately)
- [x] Implement `up-line-or-search` widget for `↑` and `Ctrl-P` (open interactive multi-line history search menu)
- [x] Tab & Shift-Tab bindings (`expand-or-complete`, `reverse-menu-complete`)
- [x] Real-time Type-Ahead Auto-Listing: debounced hook in `line-pre-redraw` that displays up to 16 lines of completions below the prompt without requiring Tab
- [x] Tab unambiguous common prefix insertion during type-ahead

### Phase 3: Fish-Style Autosuggestions
- [x] Full-line autosuggestion acceptance (`Right-Arrow`, `End`, `Ctrl-F`, `Ctrl-E`)
- [x] Fish-style word-by-word autosuggestion acceptance (`Alt-Right`, `Alt-F`, `Ctrl-Right`)
- [x] Recent directories (`cdr` / `chpwd_recent_dirs`) integration for `cd` navigation

### Phase 4: Fish & FSH Syntax Highlighting Parity
- [x] Option highlighting semantics: recognize `--` as end-of-options delimiter (tokens following `--` starting with `-` are treated as words/arguments, not options)
- [x] Live filesystem path validation: valid directories highlighted and underlined (`fg=magenta,underline`), valid files highlighted (`fg=magenta`)
- [x] Bracket and quote matching:
  - [x] Recognize matching pairs of `()`, `{}`, `[]`, and quotes `""`, `''`
  - [x] Highlight paired bracket/quote when cursor touches it (`bg=blue` / `bold`)
  - [x] Highlight unclosed or mismatched brackets/quotes in red error (`fg=red,bold`)

### Phase 5: Verification & Microbenchmarks
- [x] Keystroke latency verification (sub-10µs microbenchmark)
- [x] Large directory (`/nix/store`) non-blocking completion verification
- [x] Full workspace test suite pass (`cargo test --all` and `cargo clippy --all -- -D warnings`)

### Phase 6: Edge Case Hardening & Parity Bugfixes
- [x] Robust stack-based bracket & quote matching (comments immunity, nested quotes & command substitutions `$()`, `${}`)
- [x] Non-overlapping span overlay slicing in highlight diff engine
- [x] Preserve `~/` path prefix during tilde filesystem completion
- [x] Prevent trailing space insertion after directory completions (`compadd -S ''`) and auto-remove space on files (`-q -S ' '`)
- [x] Whole-line buffer replacement for interactive history search widget (`PREFIX="$BUFFER"`)
- [x] Universal keymap bindings supporting both Emacs and Vi modal states (`main`, `emacs`, `viins`, `vicmd`, `menuselect`)
- [x] Unset dangling `POSTDISPLAY` autosuggestions when cursor retreats into line

### Phase 7: Input Hijacking & Clean Typing Fixes
- [x] Remove intrusive real-time typeahead completion widget from `line-pre-redraw` hook (prevented typing words like `hello` from being hijacked or auto-corrected)
- [x] Remove `_approximate` from completer list to eliminate aggressive auto-mutation of typed words
- [x] Remove `automenu` from completion style to prevent automatic candidate insertion while typing
- [x] Implement Fish-parity history search (`up-line-or-beginning-search` / `down-line-or-beginning-search`) on Up/Down arrows
- [x] Bind interactive multi-line history search menu widget to `Ctrl-R`
- [x] Add PTY end-to-end regression test for typing `hello` cleanly without corruption

### Phase 8: Safe Real-Time Auto-Listing & Tab Menu Dismissal
- [x] Fast Rust-powered typeahead auto-listing (`zsh-pulse-complete --max=16`) running in sub-10µs without compsys script overhead
- [x] Tab toggle widget (`_zsh_pulse_tab`): pressing Tab hides/clears the completion menu when visible, expands completions when hidden
- [x] Pressing Tab inside menu selection (`menuselect`) immediately dismisses and hides the menu (`send-break`)
- [x] Down-Arrow (`_zsh_pulse_down_line_or_select`) enters visible typeahead menu or performs prefix history search

### Phase 9: Zsh Wordcode (`zcompile`) Compilation & Zero Parse Overhead
- [x] Configure `contrib/package.nix` with `nativeBuildInputs = [ zsh ]` and `zcompile` all installed scripts (`init.zsh`, `widgets.zsh`, `completion.zsh`)
- [x] Pre-compile `.zwc` wordcode in `bench/install.sh` for development and benchmark workflows
- [x] Add `*.zwc` to `.gitignore` to keep repository clean

### Phase 10: Completion / Highlighting Coexistence & region_highlight Protection
- [x] Protect `region_highlight` from being erased by Zsh's `list-choices` completion widget by running typeahead before highlighting and saving/restoring display attributes
- [x] Enable `compinit` in PTY test harness to ensure tests always exercise real-world interactive shell conditions

### Phase 11: History Completion Quoting Fix
- [x] Pass `-Q` to `compadd` in `_zsh_pulse_history_widget` and `_zsh_pulse_typeahead_widget` to prevent Zsh from backslash-escaping spaces and quotes in history commands (e.g. `git diff` instead of `git\ diff`)
- [x] Add end-to-end PTY regression test `history_search_does_not_escape_spaces_in_command`

### Phase 12: Menu-Selection Re-Entrancy & Segmentation Fault Fix
- [x] Guard `zsh-pulse-highlight-redraw` and `_zsh_pulse_typeahead_widget` against re-entrant execution during active menu selection (`KEYMAP == menuselect`, `MENUSELECT`, `MENUMODE`, or completion widgets)
- [x] Bind Enter in `menuselect` (`^M`, `^J`, `\r`) to `.accept-line` so lines exit cleanly without triggering wrapped hook cascades in `complist.so`
- [x] Add `zle-line-finish` hook to reset typeahead state when command is submitted
- [x] Add end-to-end PTY regression test `menu_select_down_arrow_accept_line_does_not_segfault`

### Phase 13: Single-Keystroke Menu Selection & Navigation Fix
- [x] Pass `-l` to `compadd` in `_zsh_pulse_history_widget` and truncate descriptions to `COLUMNS - 2` so each candidate occupies exactly 1 line and never wraps onto a second row
- [x] Bind `menuselect` keymap navigation explicitly for `^[[A`, `^[OA`, `^P`, `k` (`up-line-or-history`), `^[[B`, `^[OB`, `^N`, `j` (`down-line-or-history`), `^[[C`, `^[OC`, `^F`, `l` (`forward-char`), `^[[D`, `^[OD`, `^B`, `h` (`backward-char`), and `^R` (`up-line-or-history`)
- [x] Guard `zsh-pulse-highlight-redraw` to skip during active menu selection (`KEYMAP == menuselect || -n $MENUSELECT || -n $MENUMODE`) and unset `POSTDISPLAY` to prevent duplicate redraws and cursor desynchronization
- [x] Sanitize multi-line commands in `complete_history` to replace newlines with spaces for clean single-line display
- [x] Add end-to-end PTY regression test `history_search_single_down_arrow_selects_next_candidate` verifying single Up/Down presses immediately advance the selection highlight in history search

### Phase 14: Menu Selection State Integrity & C-Level Global Detection
- [x] Query Zsh C global symbols `localkeymap` and `menucmp` via `Api::is_menu_active(&self)` to detect active menu selection accurately even when `$KEYMAP` remains `"main"` / `"emacs"`
- [x] Expose `--is-menu-active` flag in both `zsh-pulse-complete` and `zsh-pulse-highlight` builtins (0 = active, 1 = inactive)
- [x] Guard `zsh-pulse-highlight-redraw` and `_zsh_pulse_typeahead_widget` with `is_menu_active` and `_ZSH_PULSE_MENU_ACTIVE` flags to prevent typeahead from executing during menu selection and invalidating `complist`
- [x] Prefix `menuselect` movement bindings with dot (`.up-line-or-history`, `.down-line-or-history`, etc.) to invoke internal C built-ins directly and prevent user-wrapped widgets (e.g. from `zsh-autosuggestions`) from aborting menu selection
- [x] Fix race condition in `zsh_integration` path tests using atomic counters

---

## Part II: Research Spikes & Foundational Stages (Stages B–E)

### Stage B — Boundary Spike
*Gate: all three open risks retired before any product code is written.*

- [x] **B1** Cargo workspace + `zsh-abi` crate: `#[repr(C)]` structs, `extern "C"` decls, `#[unsafe(no_mangle)]` entry points. (*Stronger than planned: zero zsh symbols are linked at build time; all are resolved via `dlsym`, so the crate links and unit-tests normally.*)
- [x] **B2** Capability probe — `dlsym` every required symbol at load; fail the load cleanly and select the fallback on any miss. (*Retires export variance: verified working; all symbols resolve on target Zsh.*)
- [x] **B3** Export both symbol forms (`setup_` and `setup_zshpulseQscore`); unused one ignored. (*Retires `DYNAMIC_NAME_CLASH_OK` variance.*)
- [x] **B4** `catch_unwind` guard at every entry point — a Rust panic must never unwind into zsh. (*Verified by 1,000-panic test.*)
- [x] **B5** `$module_path` packaging (`zshpulse/core.so`) + `zmodload zshpulse/core`.
- [x] **B6** Per-call cost measured with a reproducible harness (`bench/bench_module_call.zsh` + `bench/install.sh`).
  - First implementation: **641 ns**
  - After removing per-call allocations: **276 ns** (budget: ≤1 µs).
- [x] **B7** Thread probe: a std worker thread inside real zsh, answering work over a channel without ever calling into zsh.
  - Thread starts inside live zsh and round-trips a job: Verified.
  - zsh thread count goes 1 → ≥2: Verified.
  - 600 signal deliveries (SIGWINCH/SIGCONT) while the worker lives: Zsh survives, builtin works.
  - Per-call cost after adding the worker: 290 ns (within noise).
  - Engine works with zero workers: `Worker::spawn` returns `Result`; `shared()` yields `None`.

#### Boundary Findings That Changed the Design
1. **A Rust `&str` is not NUL-terminated.** Handing one to zsh as `char *` produced a builtin named `zsh-pulse-ZSH_PULSE_DEBUG` and corrupted zsh's builtin table. `CStr` now only accepts `c"…"` literals, and tests assert the trailing NUL exists.
2. **`setaparam` takes ownership** of the value array (`assignaparam` calls `freearray` on every path). Values must be allocated by `zalloc`/`ztrdup`, not Rust stack or pool allocators.
3. **`setaparam`'s return value is meaningless** — it returns a `Param` pointer truncated to `int`.
4. **Writing a parameter twice corrupted the heap with `zhalloc`** (`free(): invalid size`). Solved by allocating array via `zalloc` (plain `malloc`) and elements via `ztrdup`. Pool memory (`zhalloc`/`dupstring`) is recyled around commands by `popheap()`.
5. **Reading `BUFFER`/`CURSOR` does not allocate**: `read_param_bytes` fills a reused buffer via `getsparam_u`, and `CURSOR` is parsed directly to `i64`.

---

### Stage C — Lexer, Spans, and ZLE Integration

- [x] **C0** Pure-Rust crates: `lexer`, `highlight` (no zsh dependency; 64 unit tests).
- [x] **C1** Incremental lexer with serializable checkpoints; splice on state convergence.
- [x] **C2** `HighlightSpan` + run-merged storage + two-pointer diff; `size_of` and allocation-count budget on the hot path (steady-state re-lex allocates at most once).
- [x] **C3** `region_highlight` boundary — one `setaparam`, reuse scratch buffer, skip entirely when unchanged.
- [x] **C4** `zle-line-pre-redraw` integration: one hook, paste guard, wall-clock budget, clean bail to unstyled on unparseable input.
- [x] **C5** Zsh adapter (`zsh/init.zsh`, `zsh/widgets.zsh`) + loadable fallback path.
- [x] **C6** Whole-line coloring: options, paths, and unknown commands.
  - `TokenKind::Option`: Words starting with `-` (longer than 1 char, not `--`).
  - Command-word protocol: shell answers whether command words are known (`ZSH_PULSE_COMMAND_CACHE`, TTL=2s).
  - Path lookup: Literal words resolved against `$PWD`/`$HOME` with `stat` (bounded at 16 lookups).
  - Coexistence: Entries carrying `memo=zsh-pulse` replaced; third-party highlights preserved.

#### Incremental Lexer Performance (C1 Acceptance)
| Line Length | Full Re-parse | Incremental Splicing | Policy Selected | Speed-up | Bytes Re-scanned |
|---|---|---|---|---|---|
| 17 bytes | 0.08 µs | 0.06 µs | 0.06 µs | 1.33× | 18 |
| 46 bytes | 0.12 µs | 0.12 µs | 0.12 µs | - | 47 |
| 86 bytes | 0.19 µs | 0.16 µs | 0.15 µs | 1.27× | 21 |
| 169 bytes | 0.36 µs | 0.21 µs | 0.21 µs | 1.71× | 10 |
| 354 bytes | 0.63 µs | 0.34 µs | 0.34 µs | 1.86× | 14 |
| 1,389 bytes | 1.94 µs | 1.08 µs | 1.08 µs | 1.80× | 10 |

---

### Stage D — Async, Completion, Ranking, History

- [x] **D1** Async core: bounded channel + std threads, single fd owner closed on every path, monotonic revision guard (`crates/engine/src/worker.rs`).
- [x] **D2** Completion engine (`crates/completion`): bounded filesystem traversal (`complete_paths`) with `MAX_FILTERED_DIR_SCAN=1024` and `MAX_FS_MATCHES=32`. Resolves `/nix/store` with 81,141 directory entries in **20 ms** vs 1.6s–3.3s lockup in `zsh-autocomplete`, and `/bin` in **47 µs**.
- [x] **D3** Ranking engine (`crates/ranking`): pure Rust `fzf` algorithm implementation with boundary and consecutive bonuses and $O(N)$ `select_nth_unstable_by` top-$K$ partitioning.
- [x] **D4** History engine (`crates/history`): in-memory parser for plain and extended timestamp formats, providing sub-microsecond prefix and fuzzy lookups with zero hot-path disk I/O.

---

### Stage E — Benchmarks and Integration Verification

- [x] **E2** `zpty` interactive latency harness (`bench/bench_zpty.zsh` + `crates/zsh-module/tests/zsh_pty.rs`): drives real interactive Zsh through PTY, verifying actual terminal screen escape sequences and colors. Round trip measured at **68.9 µs per keystroke** (dominated by PTY/ZLE terminal overhead).
- [x] **E3** Startup cost and syscall verification (`bench/bench_startup.zsh`): Native module loads in **0.12 ms**; full plugin with `.zwc` wordcode loads in **0.17 ms**; zero subshell forks (`clone`/`fork`/`execve`) on keystroke paths.
- [x] **E7** Architectural and technical documentation deliverables: consolidated into `docs/` and project root.

