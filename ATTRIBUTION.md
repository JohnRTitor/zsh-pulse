# Upstream Attribution & Acknowledgments

`zsh-pulse` builds upon years of research, design patterns, and algorithmic breakthroughs developed across the open-source shell and terminal emulator ecosystem.

We believe in rigorous and transparent attribution. This document formally attributes every upstream project, algorithm, constant, architectural concept, and design pattern that informed or was incorporated into `zsh-pulse`.

---

## Upstream Repositories Under Study

During the research and design phases of `zsh-pulse`, the following upstream projects were cloned, inspected, and profiled at specific revisions:

| Project | Remote Repository | Upstream License | Commit SHA | Primary Role in zsh-pulse |
|---|---|---|---|---|
| **`fish-shell`** | [github.com/fish-shell/fish-shell](https://github.com/fish-shell/fish-shell) | GPL-2.0 / Dual | `35c290272cc9` | Semantic syntax roles, `--` option delimiters, ghost text autosuggestions, word-by-word accept ergonomics. |
| **`fzf`** | [github.com/junegunn/fzf](https://github.com/junegunn/fzf) | MIT | `b1be3a8be1b8` | Pure Rust Smith-Waterman fuzzy scoring algorithm and exact scoring constants. |
| **`ble.sh`** | [github.com/akinomyoga/ble.sh](https://github.com/akinomyoga/ble.sh) | BSD-3-Clause | `d81fd54feb0d` | Incremental syntax highlighting state checkpoints and convergence-based tail splicing. |
| **`zsh-autocomplete`** | [github.com/marlonrichert/zsh-autocomplete](https://github.com/marlonrichert/zsh-autocomplete) | MIT | `77706d4cf248` | Completion menu layouts, `list-choices` typeahead widgets, complist safety, and real-time listing truncation/line-cap patterns. |
| **`zasync`** | [github.com/marlonrichert/zasync](https://github.com/marlonrichert/zasync) | MIT | `dcdc53e2ab17` | Safe file descriptor lifecycle and `zle -F` handler patterns. |
| **`zsh-autosuggestions`** | [github.com/zsh-users/zsh-autosuggestions](https://github.com/zsh-users/zsh-autosuggestions) | MIT | `85919cd1ffa7` | Ghost text rendering via `$POSTDISPLAY` and cursor retreat handling. |
| **`fast-syntax-highlighting`** | [github.com/zdharma-continuum/fast-syntax-highlighting](https://github.com/zdharma-continuum/fast-syntax-highlighting) | BSD-3-Clause | `4672ad5dd9ad` | Command-first/filesystem-second lookup precedence and ZLE `region_highlight` mechanics. |
| **`zsh-syntax-highlighting`** | [github.com/zsh-users/zsh-syntax-highlighting](https://github.com/zsh-users/zsh-syntax-highlighting) | BSD-3-Clause | `0bfcb582e71d` | Idiomatic Zsh token styling and highlight memoization (`memo=zsh-pulse`). |
| **`atuin`** | [github.com/atuinsh/atuin](https://github.com/atuinsh/atuin) | MIT | `d440a2e61682` | In-memory history indexing and $O(N)$ top-$K$ candidate selection via `select_nth_unstable`. |
| **`mcfly`** | [github.com/cantino/mcfly](https://github.com/cantino/mcfly) | MIT | `6eed8ebd879c` | Command frecency and recency weighting models. |
| **`reedline`** | [github.com/nushell/reedline](https://github.com/nushell/reedline) | MIT | `b72fd34f54ab` | Highlighter trait ergonomics and whole-line buffer contracts. |
| **`rustyline`** | [github.com/kkawakam/rustyline](https://github.com/kkawakam/rustyline) | MIT | `05eb9200f178` | Line validator trait design and paired bracket matching. |
| **`zsh-src`** | [github.com/zsh-users/zsh](https://github.com/zsh-users/zsh) | MIT-like | `8cc5eade5ff8` | Authoritative Zsh C loadable module ABI, internal structs, and dynamic memory semantics. |
| **`zshrs`** | [github.com/MenkeTechnologies/zshrs](https://github.com/MenkeTechnologies/zshrs) | MIT | `3ee933213282` | Analysis of Rust-native shell architectures and plugin boundaries. |
| **`carapace-bin`** | [github.com/carapace-sh/carapace-bin](https://github.com/carapace-sh/carapace-bin) | MIT | `c9cac841afb9` | Multi-shell completion schema models (evaluated; rejected out-of-process IPC). |

---

## Detailed Algorithmic Provenance & Implementations

### 1. `fzf` — Fuzzy Scoring Algorithm & Constants
- **Implementation**: [`crates/ranking/src/lib.rs`](crates/ranking/src/lib.rs)
- **Borrowed Elements**:
  - Pure Rust port of `fzf`'s core Smith-Waterman matching algorithm with linear memory optimization.
  - **Scoring Constants**:
    ```rust
    pub const SCORE_MATCH: i32 = 16;
    pub const SCORE_GAP_START: i32 = -3;
    pub const SCORE_GAP_EXTENSION: i32 = -1;
    pub const BONUS_BOUNDARY: i32 = 8;
    pub const BONUS_NON_WORD: i32 = 7;
    pub const BONUS_CAMEL: i32 = 7;
    pub const BONUS_CONSECUTIVE: i32 = 8;
    pub const BONUS_FIRST_CHAR_MULTIPLIER: i32 = 2;
    ```
  - Bonus heuristics for characters following delimiters (`/`, `-`, `_`, `.`, `\`), camelCase uppercase transitions, and consecutive matches.

### 2. `ble.sh` — Incremental Lexing with Checkpoint Splicing
- **Implementation**: [`crates/lexer/src/incremental.rs`](crates/lexer/src/incremental.rs), [`crates/lexer/src/state.rs`](crates/lexer/src/state.rs)
- **Borrowed Elements**:
  - Adapted from `ble.sh`'s `core-syntax.sh:5540-5550` incremental parser.
  - Serializing parser state at token boundaries (`LexState`, 45 bytes) with resumption guards.
  - Convergence detection: halting the scan as soon as the scanner reaches a state identical to a previously recorded checkpoint (`same_parse`), allowing the unmodified tail of tokens to be spliced directly.

### 3. `fish-shell` — Syntax Roles, Autosuggestions, and Pager Ergonomics
- **Implementation**: [`crates/highlight`](crates/highlight), [`crates/completion`](crates/completion), [`zsh/completion.zsh`](zsh/completion.zsh)
- **Borrowed Elements**:
  - **Syntax Roles**: Semantic token classifications (`HighlightRole::Command`, `Option`, `Param`, `Quote`, `Escape`, `Redirection`, `Terminator`).
  - **Option Parsing**: `--` treated as the definitive end-of-options delimiter.
  - **Autosuggestion Ergonomics**: Inline ghost text display model (`$POSTDISPLAY`).
  - **Word-by-Word Acceptance**: `Alt-Right` / `Alt-F` accepting individual words from suggestions rather than forcing all-or-nothing line completion.
  - **Prefix Search Philosophy**: Up/Down arrows performing beginning-of-buffer prefix matching against command history.

### 4. `atuin` — In-Memory History & Top-$K$ Partitioning
- **Implementation**: [`crates/history/src/lib.rs`](crates/history/src/lib.rs), [`crates/ranking/src/lib.rs`](crates/ranking/src/lib.rs)
- **Borrowed Elements**:
  - $O(N)$ candidate selection using `select_nth_unstable_by` before sorting only the top-$K$ candidates, avoiding expensive $O(N \log N)$ full-dataset sorting on interactive search paths.
  - In-memory history caching model to eliminate synchronous disk I/O on the hot keystroke path.

### 5. `fast-syntax-highlighting` (FSH) & `zsh-syntax-highlighting` — Command Resolution & ZLE Pipeline
- **Implementation**: [`zsh/widgets.zsh`](zsh/widgets.zsh), [`crates/zsh-module/src/highlight.rs`](crates/zsh-module/src/highlight.rs), [`crates/zsh-module/src/region.rs`](crates/zsh-module/src/region.rs)
- **Borrowed Elements**:
  - Two-stage lookup order: Consulting the shell's internal command hash tables first (`$aliases`, `$functions`, `$builtins`, `$commands`, `$reswords`) before falling back to filesystem `stat` probes.
  - Highlighting memoization (`memo=zsh-pulse`) to cleanly coexist with other ZLE highlighting plugins without clobbering their `region_highlight` entries.
  - Color styling palette conventions, adapted to the current patina-aligned defaults (`fg=cyan` for valid commands, subcommands, and options; `fg=red,bold` for unknown commands; `underline` for valid paths).

### 6. `zsh-autocomplete` & `zsh-src` — Menu Safety & Native Builtin Calling
- **Implementation**: [`zsh/completion.zsh`](zsh/completion.zsh), [`crates/zsh-abi/src/api.rs`](crates/zsh-abi/src/api.rs)
- **Borrowed Elements**:
  - `list-choices` widget architecture for displaying native compsys typeahead completion previews below the prompt.
  - Real-time listing line-cap and truncation logic adapted from `Functions/Init/.autocomplete__async` (MIT): the `min(16, LINES - BUFFERLINES - 1)` rendered-line cap, `_describe` group/heading accounting, `compadd` wrapping/trimming (`_displ_`/`_matches_` arrays), early tag cutoff once the preview is full, and `(MORE)` marker behavior.
  - Interfacing with `complist`'s internal `domenu()` dispatcher via standard builtins (`up-line-or-history`, `down-line-or-history`, etc.) and delegating to dot-prefixed `.accept-line` on Enter to bypass user hooks and prevent premature menu cancellation.
  - Direct inspection of C global symbols (`localkeymap` and `menucmp`) to accurately detect active `complist` menu selection states and prevent re-entrant crashes.

### 7. `zsh-src` — Memory Allocation Contracts
- **Implementation**: [`crates/zsh-abi/src/probe.rs`](crates/zsh-abi/src/probe.rs), [`crates/zsh-abi/src/api.rs`](crates/zsh-abi/src/api.rs)
- **Borrowed Elements**:
  - Memory handoff rules: using `zalloc` (plain `malloc`) and `ztrdup` for memory transferred to Zsh parameters (`setaparam`, `region_highlight`), avoiding Zsh's pool allocators (`zhalloc`, `dupstring`) whose contents are recycled by `popheap()` around every command.

### 8. `zasync` — Asynchronous File Descriptor & Worker Lifecycle
- **Implementation**: [`crates/engine/src/worker.rs`](crates/engine/src/worker.rs)
- **Borrowed Elements**:
  - Evaluated asynchronous worker patterns and file descriptor management.
  - Guaranteed single-owner lifecycle for Unix file descriptors across subshells and signal deliveries (`SIGWINCH`/`SIGCONT`), preventing descriptor leaks on worker thread termination.

### 9. `mcfly` — History Recency & Context Weighting
- **Implementation**: [`crates/history/src/lib.rs`](crates/history/src/lib.rs)
- **Borrowed Elements**:
  - Timestamp-weighted scoring models evaluating command age and frequency decay during history ranking.

### 10. `reedline` & `rustyline` — Highlighter Trait Architecture & Bracket Pairing
- **Implementation**: [`crates/highlight/src/bracket.rs`](crates/highlight/src/bracket.rs), [`crates/highlight/src/lib.rs`](crates/highlight/src/lib.rs)
- **Borrowed Elements**:
  - Multi-pass highlighter trait design patterns separating lexical tokenization from visual overlay slicing.
  - Bracket pair validation state machine and cursor-touch boundary checks.

---

## Reconnaissance & Architecture Lessons

During technical reconnaissance, two architectural approaches were carefully analyzed and deliberately rejected:

1. **Bash / Readline Canvas Architectures (`ble.sh`)**:
   `ble.sh` creates its own terminal line editor canvas over Bash's `bind -x`, bypassing readline entirely. While its incremental parsing algorithm was adapted into Rust, its rendering pipeline cannot transfer directly to Zsh, which provides its own native line editor (ZLE) and `region_highlight` interfaces.

2. **Full Shell Reimplementations (`zshrs`)**:
   `zshrs` attempts a complete from-scratch Rust reimplementation of Zsh with a custom plugin ABI (`zmodload -R`). It does not run against stock Zsh C source trees. `zsh-pulse` instead preserves 100% compatibility with standard Zsh by targeting the official C loadable module ABI (`struct builtin`, `struct features`).

---

## Copyright & License Notices

Where code or algorithms have been ported or adapted:
- Ports of `fzf` algorithms: Copyright (c) Junegunn Choi, licensed under the MIT License.
- Incremental lexer algorithms adapted from `ble.sh`: Copyright (c) Koichi Murase, licensed under the BSD 3-Clause License.
- Highlighting concepts from `fast-syntax-highlighting`: Copyright (c) Sebastian Gniazdowski, licensed under the BSD 3-Clause License.
- Highlighting concepts from `zsh-syntax-highlighting`: Copyright (c) zsh-users, licensed under the BSD 3-Clause License.
- Completion concepts from `zsh-autocomplete`: Copyright (c) Marlon Richert, licensed under the MIT License.
- Autosuggestion concepts from `zsh-autosuggestions`: Copyright (c) Eric Freese, licensed under the MIT License.
- Zsh module ABI interfaces: Copyright (c) The Zsh Development Group, licensed under the MIT-like Zsh License.

