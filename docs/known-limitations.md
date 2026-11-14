# Known Limitations & Explicit Design Boundaries — `zsh-pulse`

An honest list of what this plugin does **not** do, what is measured but not optimized, and current architectural boundaries. Every entry details what the limitation is, how it is observed, and the trade-offs involved.

Companion documents: [architecture](architecture.md) ·
[performance](performance.md) ·
[ZLE integration](zle-integration.md) ·
[Rust↔Zsh boundary](rust-zsh-boundary.md) ·
[Features catalog](../FEATURES.md) ·
[Milestones archive](archive/tasks-milestones.md)

---

## 1. Correctness and Performance Characteristics

### 1.1 Keystroke Cost is Linear in Line Length
- **Observation**: The *scanning* phase of incremental lexing is sub-linear—the number of re-scanned bytes does not scale with total line length. However, the *end-to-end* re-lex is linear because token offsets after an insertion point shift and must be updated with absolute offsets.
- **Evidence**: On a 169-byte buffer, incremental lexing saves 57 ns; on a 1,389-byte buffer, it saves 809 ns ([performance report §3.2](performance.md)), while bytes re-scanned remain at 10–47 bytes.
- **Trade-off**: Storing token offsets relative to stable anchors would add complex coordinate arithmetic. At 1,389 bytes, total keystroke latency remains ~0.5 µs (5% of the 10 µs budget), so absolute offsets provide simplicity and provable correctness without human-perceptible delay.

### 1.2 PTY Harness Resolution
- **Observation**: The pseudo-terminal integration harness ([`crates/zsh-module/tests/zsh_pty.rs`](../crates/zsh-module/tests/zsh_pty.rs)) exercises real interactive Zsh under a PTY, verifying rendered terminal escape sequences.
- **Latency Resolution**: In [`bench/bench_zpty.zsh`](../bench/bench_zpty.zsh), the median interactive round-trip is **68.0 µs per keystroke** (versus 45.1 µs without the plugin). The 23 µs difference is dominated by Zsh line editor redraw routines and PTY IPC round-trips. The native Rust module execution itself accounts for ~0.86 µs.

### 1.3 `region_highlight` Merge and Clearing
- **Behavior**: `zsh-pulse` tags its regions with `memo=zsh-pulse`. On each redraw, it retains third-party highlight entries, drops only its own previous regions, and appends updated spans ([`crates/zsh-module/src/region.rs`](../crates/zsh-module/src/region.rs), [`crates/zsh-module/src/highlight.rs`](../crates/zsh-module/src/highlight.rs)).
- **Bailout/unpublishable recovery**: on lines exceeding limits/budget, parse errors, or unpublishable ZLE states, the module aims to clear only its own (`memo=zsh-pulse`) regions and preserve foreign entries when the parameter is readable/publishable; if `region_highlight` is unavailable or unpublishable, stale owned publish state is invalidated so recovery is clean on the next publishable update. (See coexistence/recovery coverage in `crates/zsh-module/tests/zsh_integration.rs`.)

### 1.4 Incremental Lexer Crossover Threshold
- **Threshold**: Commands shorter than 128 bytes (`INCREMENTAL_MIN_LEN`) bypass incremental checkpoint splicing and perform an allocation-free full re-parse ([`crates/lexer/src/incremental.rs`](../crates/lexer/src/incremental.rs)).
- **Rationale**: For short commands, diffing checkpoints and splicing vectors introduces more constant overhead than re-scanning 20–80 bytes from warm memory.

### 1.5 Whole-Line Styling Scope & Deliberate Edges
1. **Quoted Command Words**: Command words enclosed in quotes (e.g. `'ls'` or `"cat"`) are styled as literal quote tokens (`fg=green`), not executable commands.
2. **Compound Statements**: In constructs like `if true; then echo hi; fi`, `echo` is treated as an argument rather than a top-level command token.
3. **Process substitutions and `--` semantics (bounded):** `<(...)/>(...)` (including fd-prefixed whole-word forms) are lexed as substitution command contexts so inner commands/options can be highlighted in supported cases; adapter command discovery inside substitutions is best-effort for common whole-word forms. `--` end-of-options handling is lexical and scoped per command context. Wrapper/precommand command-position handling and subcommand detection are bounded heuristics (allowlisted wrappers/families, heuristic option-argument consumption, limited nested subcommands) and **not** command-schema or option-arity aware.
4. **Subcommand and CLI-grammar coverage (bounded):** subcommand detection is limited to five allowlisted families (`git`, `cargo`, `docker`, `kubectl`, `npm`), a fixed list of names per family, selected second-level groups, and a two-level depth cap. The adapter skips options it knows take a separate value (`git -C /tmp status`, `git --git-dir /tmp status`) and `cargo +toolchain`, but an unknown option is assumed to take no value, which can hide a subcommand behind that option's value. `--` ends the search, operands outside the lists remain ordinary arguments, and precommand discovery is separate — `sudo git status` is not guaranteed to style `status`. Answers are matched by literal word text, so a repeated word elsewhere on the line can receive the same style. There is no command-schema awareness, universal option-arity model, or general multi-level CLI grammar.
5. **Tilde Expansion**: `~` and `~/...` resolve against `$HOME`. User-specific expansion (`~username`) is not resolved to prevent synchronous `/etc/passwd` lookups on the interactive thread.
6. **Symlink Resolution**: Filesystem validation uses `stat`, following symlinks to target types.
7. **Cache Expiration**: Command resolution and filesystem `stat` caches expire after 2 seconds.
8. **Lookup Caps**: Bounded to at most 16 filesystem lookups per line (`MAX_PATH_LOOKUPS`) and 32 published regions (`MAX_ENTRIES`).

Syntax-highlighting parity with zsh-patina defaults remains incremental (no theme/config foundation): process-substitution inner command contexts, lexical `--` scoping, bounded wrapper/subcommand heuristics, and improved foreign `region_highlight` preservation are implemented in supported cases, while broader grammar/dynamic parity (keywords/reserved/functions/history expansion/$'...'/tilde/fds/heredoc delim-body/$(( ))/richer param expansion, AUTO_CD/named-dir/cdpath/dynamic taxonomy, exhaustive compositions) remains outstanding.

### 1.6 Line Length & Buffer Limits
- `ZSH_PULSE_MAX_LINE` (4 KiB): Input buffers exceeding 4,096 bytes are bypassed prior to word splitting.
- `MAX_ANSWER_BYTES` (4 KiB): Command resolution argument strings exceeding 4 KiB are truncated to protect scratch buffer reuse.

### 1.7 Native Compsys Typeahead Runs Synchronously
- **Behavior**: The typeahead preview runs Zsh's native completion system (`_main_complete` in a `list-choices` context) synchronously on the ZLE thread so the preview matches Tab exactly.
- **Cost**: Latency is determined by the active completion functions and tags, not by the Rust engine. Simple contexts may complete in milliseconds, while complex or tag-heavy completions can take much longer; the line-cap/truncation wrapper reduces rendered output but still executes completion logic and uses command substitution in places.
- **Forks**: Native completion functions may fork/subshell; zero-fork guarantees apply only to the Rust highlighting, autosuggest, and history paths.
- **Suppression**: To avoid queuing work behind rapid input, the preview is skipped while `PENDING` or `KEYS_QUEUED_COUNT` is non-zero and while menu selection/typeahead re-entrancy guards are active. Disable with `ZSH_PULSE_COMPLETION=0` if a specific completion context is too expensive interactively.

---

## 2. Measurement Context & Methodology

### 2.1 Zsh Parameter Machinery Overhead
On a typical 28-byte line, total module execution is ~0.86 µs, of which pure Rust lexing is ~0.08 µs. The remaining ~0.40 µs is consumed by Zsh internal parameter operations (`getsparam_u`, `setaparam`, `setarrvalue`, and `set_region_highlight` row parsing). Optimizing region counts yields greater latency reductions than micro-optimizing lexer loops.

### 2.2 Benchmark Sample Distributions
Harness measurements in `crates/lexer/examples/bench_incremental.rs` record best-of-50 runs to isolate pure computation from interactive OS scheduling jitter.

---

## 3. Platform & Target Environments

### 3.1 Architecture & OS
- Automated CI and benchmarks run against **Zsh 5.8+ and 5.9 on x86_64 Linux** with glibc.
- The capability probe dynamically resolves symbols at load time, providing portability across varying Zsh distributions without compile-time header linkage.
- Custom Zsh builds configured with `ZSH_MEM` (custom internal memory allocators overriding system malloc) are not officially tested.

### 3.2 Signal Handling with Live Background Workers
The background worker thread (`zshpulse-wrk`) runs without a Zsh API handle. While window resize signals (`SIGWINCH`) and job continuation (`SIGCONT`) have been verified safe across hundreds of automated deliveries, asynchronous signals delivered during intense job-control forks rely on standard OS signal masks.

---

## 4. Deliberate Non-Goals & Architectural Boundaries

The following features were evaluated during design and are explicitly **out of scope**:

| Feature / Proposal | Status | Rationale |
|---|---|---|
| **Replacing Native Zsh Completion** | **Out of Scope** | Zsh possesses over 1,000 highly mature tool completion functions (`git`, `cargo`, `docker`, `nix`). Reimplementing these in Rust would introduce maintenance burden without benefit. `zsh-pulse` instead accelerates path traversal and candidate ranking while preserving `_complete`. |
| **Bash / Fish Support** | **Out of Scope** | `zsh-pulse` is specifically engineered for Zsh's C loadable module ABI (`struct builtin`, `struct features`, `region_highlight`). Porting to Bash would require completely different architectures (e.g. `ble.sh` canvas model). |
| **External Background Daemon** | **Rejected** | A persistent daemon communicating over UNIX domain sockets introduces process supervision complexity and 20–60 µs IPC latency. In-process execution achieves 276 ns calls with zero socket overhead. |
| **Tokio / Heavy Async Runtimes** | **Rejected** | Standard library threads (`std::thread`) and lightweight channels (`mpsc`) provide bounded concurrency without the memory footprint and binary size of Tokio. |
| **Tree-sitter Parsing** | **Rejected** | Fish shell hand-writes its lexer for maximum speed. Full AST tree parsing incurs substantial per-keystroke allocation overhead unnecessary for interactive syntax highlighting. |

---

## 5. Summary Table

| Limitation | Category | Impact / Severity | Mitigation |
|---|---|---|---|
| Keystroke cost scales with line length | Performance | Low at typical command lengths | Sub-linear scanning; 0.5 µs at 1,389 bytes |
| PTY harness resolution limited by IPC | Testing | Medium | Validated with direct microsecond benchmarks |
| Third-party highlight coexistence on bailout/unpublishable paths | Correctness | Low | Own regions tagged `memo=zsh-pulse`; clear/merge paths preserve foreign entries where readable/publishable and invalidate stale owned state for recovery |
| Native compsys typeahead is synchronous | Completion / Performance | Context-dependent (ms to slow on heavy completers) | Capped rendered lines, queued-key/menu suppression, `ZSH_PULSE_COMPLETION=0` toggle |
| Bounded filesystem traversal (1,024 items) | Completion | Intended Design | Eliminates multi-second freezes on `/nix/store` |
| Hard-coded syntax color defaults | Configuration | Medium | Standard ANSI colors chosen for universal contrast |
| Zsh-specific loadable module ABI | Portability | Intended Design | Focuses entirely on first-class Zsh performance |

