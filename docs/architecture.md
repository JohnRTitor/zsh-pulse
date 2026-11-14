# Architecture — `zsh-pulse`

Status: Describes the current production architecture of the `zsh-pulse` engine.  
Scope: The crates, layering, data flow, threading model, and architectural decisions.

Companion documents:

- [Performance report](performance.md) — The measured benchmarks and microsecond latencies.
- [ZLE integration design](zle-integration.md) — How the plugin hooks into the Zsh Line Editor.
- [Rust↔Zsh boundary and ownership](rust-zsh-boundary.md) — Every symbol, every allocation rule.
- [Known limitations](known-limitations.md) — Current boundaries and edge cases.
- [Features catalog](../FEATURES.md) — Authoritative list of user-facing features and keybindings.
- [Attribution](../ATTRIBUTION.md) — Provenance of borrowed upstream algorithms and constants.
- [Development Milestones](archive/tasks-milestones.md) — Historical milestone logs (Stages B–E, Phases 1–14).

---

## 1. Executive Summary

`zsh-pulse` is a high-performance shell enhancement engine for Zsh. It delivers real-time syntax highlighting, bounded filesystem path completions, Fish-style inline autosuggestions, interactive history navigation, and a native-compsys typeahead preview.

The core constraints that shaped this architecture:

| Constraint | Consequence |
|---|---|
| **Runs inside interactive shell** | A crash terminates the user's session. Every FFI boundary is probed, guarded with `catch_unwind`, and validated via isolated subprocess tests. |
| **Zero forks on Rust keystroke paths** | Highlighting, autosuggest, and history matching execute in-process in pure Rust with no `fork`, `clone`, or `execve`. The native-compsys typeahead preview is a deliberate exception: it runs Zsh completion synchronously and native completion functions may fork. |
| **Sub-microsecond budget (Rust paths)** | Rust highlighting/autosuggest paths reuse buffers; steady-state allocations on typing are bounded to ≤ 1 per edit. Native typeahead latency is governed by the completion system. |
| **Zsh distribution variance** | Zero Zsh C symbols are linked at compile time. All internal symbols (`localkeymap`, `menucmp`, `getsparam_u`, `setaparam`) are resolved at load time via `dlsym`. |
| **Isolated testability** | Lexing, styling, fuzzy ranking, and history crates have zero Zsh dependencies and forbid `unsafe`. |

---

## 2. Workspace Crates

The codebase is split into 8 crates across distinct architectural layers:

```
                         ┌──────────────────────────────┐
                         │          zsh-module          │  cdylib: zshpulse/core.so
                         │  (C ABI entry & builtins)    │  Builtins: zsh-pulse-highlight
                         └──────────────┬───────────────┘            zsh-pulse-complete
                                        │
             ┌──────────────────────────┼──────────────────────────┐
             ▼                          ▼                          ▼
    ┌─────────────────┐        ┌─────────────────┐        ┌─────────────────┐
    │     zsh-abi     │        │     engine      │        │   completion    │
    │ (all unsafe FFI,│        │ (Worker thread, │        │ (Bounded FS,    │
    │  dlsym, guards) │        │  rev counters)  │        │  dispatcher)    │
    └─────────────────┘        └─────────────────┘        └───┬─────────┬───┘
                                                              │         │
                     ┌────────────────────────────────────────┘         ▼
                     ▼                                         ┌─────────────────┐
            ┌─────────────────┐                                │     ranking     │
            │     history     │                                │ (fzf algorithm, │
            │ (In-memory log, │                                │  top-K sort)    │
            │  fast parser)   │                                └─────────────────┘
            └─────────────────┘
                     ▲
                     │
    ┌────────────────┴────────────────┐
    │              lexer              │  Pure Rust incremental lexer
    │ (Checkpoint states, tail-splice)│  Rules modeled on Zsh lx1/lx2
    └────────────────┬────────────────┘
                     ▼
            ┌─────────────────┐
            │    highlight    │  Spans, byte↔char offsets,
            │ (Bracket engine,│  overlay slicing, style diffs
            │  pairing rules) │
            └─────────────────┘
```

| Crate | Path | Depends on | `unsafe` | Responsibility |
|---|---|---|---|---|
| `zsh-abi` | [`crates/zsh-abi/`](../crates/zsh-abi) | *none* | **All FFI `unsafe`** | C ABI structs (`struct builtin`, `struct features`), `dlsym` dynamic symbol resolution, capability probe, panic isolation (`catch_unwind`), allocator protocol (`zalloc`/`ztrdup`). |
| `lexer` | [`crates/lexer/`](../crates/lexer) | *none* | Forbidden | Incremental command-line lexer: token states, checkpoint serialization (`LexState`, 45 bytes), and convergence-based tail splicing. |
| `highlight` | [`crates/highlight/`](../crates/highlight) | `lexer` | Forbidden | Styled spans: run-merging, byte↔character offset conversion, bracket and quote pair matching, and span diffing. |
| `ranking` | [`crates/ranking/`](../crates/ranking) | *none* | Forbidden | Pure Rust implementation of `fzf`'s Smith-Waterman fuzzy scoring algorithm with boundary/consecutive bonuses and $O(N)$ `select_nth_unstable` partitioning. |
| `history` | [`crates/history/`](../crates/history) | *none* | Forbidden | In-memory parser for plain and extended Zsh history timestamps (`: <timestamp>:<duration>;<cmd>`) with sub-microsecond prefix lookups. |
| `completion` | [`crates/completion/`](../crates/completion) | `history`, `ranking` | Forbidden | Bounded filesystem scanner (`complete_paths`) and completion dispatcher. Enforces iteration limits to eliminate `/nix/store` directory lockups. |
| `engine` | [`crates/engine/`](../crates/engine) | `zsh-abi` | None | Engine coordination: monotonic line revisions, highlight invocation counters, and optional background worker thread (`zshpulse-wrk`). |
| `zsh-module` | [`crates/zsh-module/`](../crates/zsh-module) | all crates | Minimal | The dynamic C module (`cdylib`): exports builtins `zsh-pulse-highlight` and `zsh-pulse-complete`, manages C memory for `region_highlight`, and inspects menu state. |
| *(adapter)* | [`zsh/init.zsh`](../zsh/init.zsh), [`zsh/widgets.zsh`](../zsh/widgets.zsh), [`zsh/completion.zsh`](../zsh/completion.zsh) | — | — | Zsh-side hooks: installs `zle-line-pre-redraw`, binds `menuselect` widgets, configures completers, and manages shell-side command validation caching. |

---

## 3. Data Flow: Keystroke to Display

### 3.1 Syntax Highlighting Pipeline
Executed on every redraw via `zle-line-pre-redraw`:

```
  User types a character
         │
         ▼
  ZLE redraws  ──  zsh-src/Src/Zle/zle_main.c:1167
         │
         ▼
  redrawhook() looks up "zle-line-pre-redraw" and invokes chained widgets
         │
         ▼
  zsh-pulse-highlight-redraw()  [zsh/widgets.zsh]
         │
         ├─ Check menu active state (KEYMAP == menuselect || is_menu_active)
         │    If active: unset POSTDISPLAY and return early (prevents complist corruption)
         │
         ├─ Query shell command tables ($aliases, $functions, $builtins, $commands)
         │    Cached in ZSH_PULSE_COMMAND_CACHE (TTL = 2 seconds)
         │
         ├─ Answer bounded CLI subcommands from the adapter's family/group tables
         │    `--subcommand=<word>` for `git`, `cargo`, `docker`, `kubectl`, `npm`
         │
         ▼
  zsh-pulse-highlight  [crates/zsh-module/src/highlight.rs]
         │
         ├─ guard(...)  [crates/zsh-abi/src/guard.rs]
         │    Isolates panics via catch_unwind
         │
         ├─ Read BUFFER & CURSOR via Api::read_param_bytes / parse_i64
         │    Reuses pre-allocated buffer; 0 allocations once warm
         │
         ├─ LexCache::relex(buffer)  [crates/lexer/src/incremental.rs]
         │    Length < 128 bytes: fast full re-parse
         │    Length ≥ 128 bytes: resume from token checkpoint, splice tail
         │
         ├─ Build and style spans:
         │    Command known / subcommand / option -> fg=cyan; unknown -> fg=red,bold
         │    Literal word naming a directory or file -> underline
         │    Bracket / quote pairs -> fg=yellow,bold on hover; fg=red,bold on mismatch
         │
         ├─ If spans unchanged: return immediately (0 Zsh calls)
         │
         ├─ Merge with third-party region_highlight entries (preserving others)
         │
         └─ Api::set_param_array("region_highlight", rows)  [crates/zsh-abi/src/api.rs]
              Allocates via zalloc / ztrdup; hands ownership to ZLE
         │
         ▼
  Zsh line editor draws highlighted text on terminal screen
```

### 3.2 Autosuggestion Pipeline
Executed immediately following syntax highlighting on the redraw hook:
1. `zsh-pulse-complete --autosuggest` is invoked if `$CURSOR` is at `$#BUFFER`.
2. The engine scans the in-memory history log for the most recent matching prefix.
3. If a candidate is found, the completion suffix is published into `$POSTDISPLAY`.
4. If `$CURSOR` moves back or buffer changes, dangling ghost text is immediately unset.

### 3.3 Typeahead Completion Preview Pipeline
1. `_zsh_pulse_typeahead_widget` runs under `zle-line-pre-redraw` when completion is enabled, the buffer changed, and guards allow it (`_ZSH_PULSE_IN_TYPEAHEAD == 0`, `! PENDING`, `! KEYS_QUEUED_COUNT`, no active menu).
2. Runs Zsh's native completion system synchronously via `_main_complete` in `curcontext="list-choices:::"` with `compstate[insert]=''`/`compstate[list]='list force packed'`, so candidates, tags, and headings match Tab exactly without modifying `$BUFFER`.
3. A `compadd`/`_describe` truncation wrapper (adapted from zsh-autocomplete) caps the preview to `min(ZSH_PULSE_TYPEAHEAD_LINES, LINES - BUFFERLINES - 1)` rendered lines and appends `(MORE)` when truncated.
4. If a list is shown, `_ZSH_PULSE_TYPEAHEAD_VISIBLE` is set for Down-Arrow menu entry; the first Tab clears `_lastcomp` and runs `expand-or-complete` to show the full, uncapped native list. This path is synchronous and may incur completion-function forks/subshell cost.

---

## 4. Concurrency & Threading Model

### 4.1 Single-Threaded Interaction Guarantees
- The interactive shell main thread is the **only** thread that executes Zsh code or touches Zsh C structures.
- Rust builtins execute synchronously on the main thread during redraw and completion widgets. Highlighting/autosuggest builtins take sub-microsecond slices; the native-compsys typeahead widget is also synchronous on this thread but runs `_main_complete`, so its duration is completion-function dependent.

### 4.2 Background Worker Thread (`zshpulse-wrk`)
- **Location**: [`crates/engine/src/worker.rs`](../crates/engine/src/worker.rs)
- **Role**: An optional background thread spawned lazily for expensive or asynchronous background computations.
- **Safety Invariant**: The worker thread **never** holds a Zsh pointer or invokes Zsh C APIs. All communication across the boundary occurs over Rust `mpsc` channels using plain data types (`Vec<u8>`, integers).
- **Graceful Degradation**: If `Worker::spawn` fails, `worker::shared()` returns `None`, and the entire engine continues running seamlessly in single-threaded mode.

### 4.3 Monotonic Revision Guard
- Keystrokes advance a monotonic atomic revision counter: `revision: u64`.
- Any background task returning a revision unequal to the current line revision is automatically discarded, preventing stale background results from clobbering active input.

---

## 5. Architectural Decisions & Lessons Learned

### 5.1 Native Loadable Module vs Process Spawns
- **Decision**: In-process Rust `cdylib` loaded via `zmodload zshpulse/core`.
- **Alternative**: Spawning external CLI helpers (e.g. `$(my-highlighter)`).
- **Rationale**: Forking a child process costs ~1–3 ms; a native module call costs **~276 ns** (over 3,600× faster). The Rust highlighting, autosuggest, and history paths spawn no child processes on keystrokes. The native-compsys typeahead preview is a separate synchronous path and may trigger forks/subshells inside Zsh's completion functions or wrappers.

### 5.2 Dynamic Symbol Probe (`dlsym`) vs Static Linkage
- **Decision**: All Zsh symbols are resolved at runtime via `dlsym(RTLD_DEFAULT, ...)`.
- **Alternative**: Linking against `libzsh.so` or exporting undefined C externs.
- **Rationale**: Zsh builds vary wildly across Linux distributions and macOS (symbol visibility, export prefixes, `DYNAMIC_NAME_CLASH_OK`). Compile-time linking creates brittle binaries that fail to link in cargo unit tests. With `dlsym`, crates link and test cleanly in standard `cargo test` environments without requiring Zsh headers.

### 5.3 Safe Allocator Protocol (`zalloc`/`ztrdup`) vs Pool Allocators
- **Decision**: Use `zalloc` (plain `malloc`) and `ztrdup` for data handed to Zsh.
- **Alternative**: Using Zsh pool allocators (`zhalloc`, `dupstring`).
- **Rationale**: Zsh's pool memory is rewound and invalidated by `popheap()` around every command execution. Pointers to pool memory stored in shell parameters (`setaparam`, `region_highlight`) cause heap corruption and segmentation faults. `zalloc` memory is safely owned by ZLE and released via `freearray`/`free`.

### 5.4 Bounded Filesystem Traversal vs Unbounded Globs
- **Decision**: Impose strict ceilings (`MAX_FILTERED_DIR_SCAN = 1024`, `MAX_FS_MATCHES = 32`) on path traversals.
- **Alternative**: Unbounded directory reading (standard Zsh completion behavior).
- **Rationale**: Huge system directories like `/nix/store` freeze Zsh for seconds. Enforcing hard traversal limits guarantees deterministic response times (< 20 ms) even on multi-thousand entry folders.

### 5.5 C-Level Menu-Active State Inspection
- **Decision**: Directly inspect `localkeymap` and `menucmp` via `Api::is_menu_active`.
- **Alternative**: Relying solely on the shell variable `$KEYMAP`.
- **Rationale**: Zsh's `$KEYMAP` variable does not reliably report `"menuselect"` during initial menu population. Invoking redraw hooks or completion widgets during an active menu selection crashes `complist.so`. Direct C-level pointer inspection eliminates re-entrant crashes.

---

## 6. Subsystem Capabilities & Modularity

### 6.1 Completed Subsystems
1. **Incremental Lexer**: Full POSIX/Zsh word splitting, checkpoint resumption, delimiter handling.
2. **Syntax Highlighter**: Whole-line semantic styling, option parsing, bracket pairing, live path checks.
3. **Autosuggestions**: Sub-microsecond ghost text via `$POSTDISPLAY`, word-by-word accept (`Alt-Right`).
4. **History Engine**: Dual plain/extended timestamp parser, Up/Down prefix search, `Ctrl-R` multi-line menu.
5. **Completion Engine**: Preserved native completion pipeline (`_complete`), bounded path scanner, `fzf` fuzzy ranking.
6. **Menu Navigation**: Native-compsys typeahead preview with first-Tab full-list expansion, Down-Arrow direct selection, single-keystroke grid navigation, and safe accept/abort lifecycle handlers.

### 6.2 Feature Modularity
The engine supports granular compile-time and runtime toggles:
- **Compile-time**: `Cargo.toml` features (`highlight`, `autocomplete`, `history`, `history-navigation`).
- **Runtime**: Shell environment variables (`ZSH_PULSE_HIGHLIGHT`, `ZSH_PULSE_COMPLETION`, `ZSH_PULSE_HISTORY`, `ZSH_PULSE_AUTOSUGGEST`).

---

## 7. Documentation Navigation

- [Features Catalog](../FEATURES.md) — Exhaustive feature descriptions and keybindings.
- [Upstream Attribution](../ATTRIBUTION.md) — Sources, licenses, and borrowed algorithms.
- [Performance Report](performance.md) — Microsecond benchmarks and profiling records.
- [ZLE Integration Design](zle-integration.md) — Detailed hook lifecycles and widget wrappers.
- [Rust↔Zsh Boundary](rust-zsh-boundary.md) — Memory ownership and FFI symbol contracts.
- [Known Limitations](known-limitations.md) — Architectural limits and non-goals.
- [Milestones Archive](archive/tasks-milestones.md) — Completed development task logs.

