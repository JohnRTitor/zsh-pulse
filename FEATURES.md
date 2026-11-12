# zsh-pulse: Feature Catalog & Technical Specifications

This document provides the authoritative, complete catalog of all features implemented in `zsh-pulse`. Each feature is documented with its technical architecture, relevant crates and files, configuration environment toggles, and keybindings.

---

## Table of Contents

1. [Real-Time Syntax Highlighting](#1-real-time-syntax-highlighting)
2. [Fish-Style Autosuggestions](#2-fish-style-autosuggestions)
3. [History Navigation & Interactive Search](#3-history-navigation-interactive-search)
4. [Hybrid Completion Engine](#4-hybrid-completion-engine)
5. [Menu Navigation & Real-Time Typeahead](#5-menu-navigation-real-time-typeahead)
6. [Configuration, Compilation & Modularity](#6-configuration-compilation-modularity)

---

## 1. Real-Time Syntax Highlighting

`zsh-pulse` provides high-performance, real-time syntax highlighting for the Zsh command line editor (ZLE), running entirely in-process on the redraw hook with sub-microsecond latency.

### 1.1 Incremental Lexer with Checkpoint Serialization
- **Implementation**: [`crates/lexer`](crates/lexer), [`crates/lexer/src/incremental.rs`](crates/lexer/src/incremental.rs)
- **Design**: Modeled on the incremental parsing algorithms pioneered by `ble.sh`. Instead of re-parsing the entire command line on every keystroke, the lexer caches parse state checkpoints (`LexState`, 45 bytes) at token boundaries.
- **Tail Splicing**: On an edit at cursor offset $K$, the lexer resumes scanning from the nearest prior checkpoint. Once the scanner reaches a state identical to a previous checkpoint (`same_parse`), scanning halts and the unmodified tail of tokens is spliced in with shifted byte offsets.
- **Adaptive Fallback**: For short command lines (< 128 bytes), benchmarks demonstrate that full re-parsing is faster than the diffing overhead of incremental splicing. The engine automatically switches between incremental splicing and scratch-buffered re-parsing via `LexCache::relex`.
- **Latency**: ~0.08 µs full re-parse for typical commands; ~0.21 µs incremental re-lex on 169-byte lines (1.7× faster than re-parse).

### 1.2 Semantic Token Roles & Syntax Palette
- **Implementation**: [`crates/highlight/src/lib.rs`](crates/highlight/src/lib.rs), [`crates/zsh-module/src/highlight.rs`](crates/zsh-module/src/highlight.rs)
- **Token Classification**:
  - `Command`: Known commands, functions, builtins, and aliases (`fg=green`); unknown commands (`fg=red,bold`).
  - `Option`: Command-line switches (e.g. `-a`, `--long-flag`) rendered in `fg=cyan`.
  - `Param` / `Argument`: Standard command arguments.
  - `Quote`: Single quotes (`'...'`), double quotes (`"..."`), and backticks (`` `...` ``) styled in `fg=yellow`.
  - `Escape`: ANSI escape sequences and backslash escapes (`\n`, `\t`, `\x1b`).
  - `Redirection`: File and descriptor redirections (`>`, `<`, `>>`, `2>&1`, `&>`).
  - `Terminator`: Command separators and pipeline operators (`;`, `|`, `||`, `&&`, `&`).
  - `Assignment`: Environment variable assignments preceding commands (`VAR=val`, `PATH+=:/bin`).

### 1.3 Option Delimiter Semantics (`--`)
- **Implementation**: [`crates/lexer/src/scanner.rs`](crates/lexer/src/scanner.rs)
- Following POSIX and Fish shell conventions, any token exactly matching `--` is treated as the end-of-options delimiter. All subsequent tokens starting with `-` (e.g. `rm -- -rf`) are treated as literal arguments/operands rather than flags.

### 1.4 Live Filesystem Path Validation
- **Implementation**: [`crates/zsh-module/src/highlight.rs`](crates/zsh-module/src/highlight.rs)
- **Behavior**: Every literal word on the command line is evaluated as a potential filesystem target (relative to `$PWD` or `$HOME`).
  - Valid existing directories: Highlighted and underlined (`fg=magenta,underline`).
  - Valid existing regular files / executables: Highlighted in `fg=magenta`.
- **Bounded Probing**: Path lookups are bounded to a maximum of 16 words per line (`MAX_PATH_LOOKUPS`) to guarantee consistent responsiveness.
- **LRU Caching**: Results from `stat` calls are cached in a 64-entry cache with a 2-second expiration TTL.

### 1.5 Robust Bracket & Quote Pairing Engine
- **Implementation**: [`crates/highlight/src/bracket.rs`](crates/highlight/src/bracket.rs)
- **Features**:
  - **Matching Pairs**: Identifies paired delimiters: `()`, `{}`, `[]`, `""`, `''`.
  - **Cursor Hover Highlighting**: Touching or resting immediately adjacent to either side of an open/close bracket or quote highlights both elements of the pair in `bg=blue` (or bold).
  - **Syntax Error Highlighting**: Unclosed brackets, mismatched brackets (e.g. `(]`), and unclosed string quotes are instantly styled with error highlighting (`fg=red,bold`).
  - **Context Immunity**: Brackets inside comments (`# (foo)`) or double-quoted strings (`"hello (world)"`) are excluded from syntax errors and bracket pairing stacks. Nested command substitutions (`$(...)`, `${...}`) maintain independent nested stacks.

### 1.6 Direct C `region_highlight` Integration
- **Implementation**: [`crates/zsh-module/src/region.rs`](crates/zsh-module/src/region.rs)
- **Zero Subshells**: Directly writes to Zsh's C `region_highlight` special array using `zalloc` and `ztrdup` pointers owned by ZLE. No `$(...)` forks or inter-process communication occurs.
- **Coexistence**: Preserves entries created by other plugins by attaching `memo=zsh-pulse`. On each redraw, `zsh-pulse` strips only its own memoized spans and appends updated highlights.
- **Guard Rails**: Capped at 32 highlight regions per line (`MAX_ENTRIES`) and 4 KiB buffer length (`MAX_LINE`) to prevent pathological input freezes.

---

## 2. Fish-Style Autosuggestions

`zsh-pulse` brings the signature Fish shell inline suggestion experience to Zsh, operating directly from an in-memory history index with zero disk I/O.

### 2.1 In-Memory History Matching
- **Implementation**: [`crates/history`](crates/history), [`crates/completion/src/lib.rs`](crates/completion/src/lib.rs)
- Command history is read into memory on shell startup and updated incrementally. Keystroke lookups query the in-memory log via prefix scanning, executing in under 10 µs without reading `.zsh_history` from disk.

### 2.2 Inline Ghost Text Rendering
- **Implementation**: [`zsh/widgets.zsh`](zsh/widgets.zsh), [`crates/zsh-module/src/complete.rs`](crates/zsh-module/src/complete.rs)
- Matches are rendered directly after the cursor using ZLE's native `$POSTDISPLAY` variable. The ghost text adapts in real time as characters are typed or deleted.

### 2.3 Full-Line Acceptance
- **Keybindings**: `Right-Arrow` (`^[[C`, `^[OC`), `End` (`^E`), `Ctrl-F` (`^F`)
- **Behavior**: Appends the entire ghost text string into the active `$BUFFER`, moves `$CURSOR` to the end, and clears `$POSTDISPLAY`.

### 2.4 Word-by-Word Acceptance
- **Keybindings**: `Alt-Right` (`^[^[[C`, `^[[1;3C`, `^[[1;5C`), `Alt-F` (`^[f`)
- **Implementation**: `_zsh_pulse_forward_word_or_accept` in [`zsh/completion.zsh`](zsh/completion.zsh)
- **Behavior**: Extracts the next whitespace-delimited word from `$POSTDISPLAY` using regular expression boundary matching (`^([[:space:]]*[^[:space:]]+[[:space:]]*)`). The extracted word is appended to `$BUFFER`, and the remaining tail remains in `$POSTDISPLAY`.

### 2.5 Recent Directory Integration (`cdr`)
- Hooks into Zsh's `chpwd_recent_dirs` and `cdr` subsystem. When typing `cd `, recently visited directories are offered as priority suggestions ahead of older history records.

### 2.6 Cursor Retreat & Menu Suppression
- If the cursor retreats into the middle of the buffer (via `Left-Arrow`, `Home`, etc.), dangling `$POSTDISPLAY` text is immediately unset to avoid editing confusion.
- Whenever an interactive completion menu is active, autosuggestions are suppressed to prevent visual overlap with the completion list.

---

## 3. History Navigation & Interactive Search

### 3.1 Dual-Format History Parser
- **Implementation**: [`crates/history/src/lib.rs`](crates/history/src/lib.rs)
- Supports both standard plain-text history files and Zsh extended history timestamps:
  ```zsh
  : 1729000000:0;cargo test --all
  ```
- Gracefully handles multiline commands, escaped newlines, and trailing continuations.

### 3.2 Fish-Parity Prefix History Search (Up / Down)
- **Keybindings**: `Up-Arrow` (`^[[A`, `^[OA`), `Ctrl-P`, Vi `k` / `Down-Arrow` (`^[[B`, `^[OB`), `Ctrl-N`, Vi `j`
- **Widgets**: `up-line-or-search`, `down-line-or-select`
- **Behavior**:
  - If `$BUFFER` contains text, searches backward or forward through history for commands matching the current prefix.
  - If multiline editing is in progress, navigates lines within the multiline buffer.
  - If `$BUFFER` is empty, navigates chronological history entries.

### 3.3 Non-Destructive History Search & `fzf` Preservation (`Ctrl-R`)
- **Keybinding**: `Ctrl-R`
- **Widget**: `fzf-history-widget` (when `fzf` is available) / `zsh-pulse-history-search` (native fallback)
- **`fzf` Preservation**:
  - Automatically detects if `fzf-history-widget` is already defined (e.g. from `fzf --zsh` in `.zshrc`) or if `fzf` is available in `$PATH`.
  - When detected, `zsh-pulse` preserves and binds the official `fzf-history-widget` without clobbering the user's `fzf` setup. This provides exact `fzf` semantics: interactive query filtering, `$BUFFER` untouched upon opening, selection on Enter, and clean cancellation on `Esc` / `Ctrl-C`.
  - Can be toggled via `export ZSH_PULSE_USE_FZF=0` to force the native widget.
- **Native Fallback Widget**:
  - For environments without `fzf`, provides an in-memory history search menu directly under the prompt containing the top 32 fuzzy-ranked matches (<10 µs latency).
  - Preserves `$BUFFER` and `$CURSOR` state on opening; pressing `Esc`, `Ctrl-C`, or `Ctrl-G` cleanly aborts the menu and restores the original command line without leaving dangling state.
  - Arrow keys (`Up`/`Down`/`Left`/`Right`, Vim `hjkl`) navigate smoothly without dismissing the menu.
  - Pressing `Enter` cleanly accepts and executes the selected command.

### 3.4 Literal Command Unescaping (`compadd -Q`)
- **Fix**: Standard Zsh completion escapes spaces and special characters when inserting history entries into the line (e.g. turning `git diff HEAD~1` into `git\ diff\ HEAD\~1`).
- `zsh-pulse` passes the `-Q` quote-inhibition flag to `compadd`, guaranteeing that commands are inserted with pristine literal whitespace and quoting.

### 3.5 Display Sanitization & Single-Line Bounding
- Multiline history items have embedded newlines replaced with spaces during menu display to avoid breaking pager layout.
- Descriptions are clamped to `COLUMNS - 2` characters to prevent accidental terminal line wrapping.

---

## 4. Hybrid Completion Engine

`zsh-pulse` combines a high-speed native Rust completion core with full fallthrough preservation of native Zsh completers.

### 4.1 Native Completer Preservation (1,000+ Tools)
- **Configuration**:
  ```zsh
  zstyle ':completion:*' completer _zsh_pulse_completer _complete
  ```
- **Behavior**: `_zsh_pulse_completer` handles path, command, and history completions via Rust. If the context represents tool-specific subcommands or flags (e.g. `git checkout <tab>`, `cargo build --<tab>`, `docker run <tab>`, `nix flake <tab>`), completion automatically falls through to Zsh's native `_complete` and standard completion functions.
- **Relationship to Typeahead**: The typeahead preview does not call this completer. It runs `_main_complete` directly in the `list-choices` context (see §5.1), so the preview and Tab both use native compsys candidates.

### 4.2 Bounded Filesystem Traversal
- **Implementation**: [`crates/completion/src/lib.rs`](crates/completion/src/lib.rs)
- **The Problem**: Traditional Zsh completers scan entire directories synchronously. On large directories like `/nix/store` (80,000+ entries) or deep NFS mounts, typing `ls /nix/store/<tab>` freezes the shell for 1.6 to 3.3 seconds.
- **The Solution**: `complete_paths` enforces strict limits:
  - `MAX_FILTERED_DIR_SCAN`: 1,024 entries scanned.
  - `MAX_FS_MATCHES`: 32 candidates returned.
- **Performance**:
  - Scanning `/nix/store` completes in **20 ms** (160× faster than `zsh-autocomplete`).
  - Traversing `/bin` completes in **47 µs** (34,000× faster).

### 4.3 Categorized Candidate Grouping
- Completions are tagged with clean, formatted group headings:
  - `%B%F{blue}[directories]%f%b`
  - `%B%F{cyan}[files]%f%b`
  - `%B%F{magenta}[history search]%f%b`
  - `%B%F{yellow}[commands]%f%b`
  - `%B%F{green}[options]%f%b`

### 4.4 Fuzzy Candidate Ranking (`fzf` Algorithm)
- **Implementation**: [`crates/ranking/src/lib.rs`](crates/ranking/src/lib.rs)
- Implements the exact Smith-Waterman fuzzy scoring algorithm used by `fzf`:
  - `SCORE_MATCH = 16`
  - `SCORE_GAP_START = -3`, `SCORE_GAP_EXTENSION = -1`
  - Boundary bonuses (camelCase, kebab-case, slash boundaries, word beginnings).
  - Consecutive match bonuses.
- Partitioning: Uses $O(N)$ `select_nth_unstable_by` for candidate sets exceeding 10,000 items before sorting only the top $K$ results.

### 4.5 Suffix Management & Tilde Handling
- Directories append a slash without a trailing space (`compadd -S ''`), allowing seamless subsequent path navigation.
- Regular files append an auto-removable trailing space (`compadd -q -S ' '`).
- Path completions beginning with `~/` preserve the tilde prefix instead of expanding to absolute `$HOME` paths on the command line.

---

## 5. Menu Navigation & Real-Time Typeahead

### 5.1 Native Compsys Typeahead Preview
- **Implementation**: `_zsh_pulse_typeahead_widget` in [`zsh/completion.zsh`](zsh/completion.zsh), registered with `zle -C zsh-pulse-typeahead list-choices`.
- **Candidate Source**: Zsh's own completion system. The widget runs `_main_complete` synchronously in the `list-choices` context (`curcontext="list-choices:::"`), so the preview shows the same commands, options, subcommands, descriptions, and group headings that Tab would show. The Rust candidate engine is not on this path.
- **No Insertion**: The widget sets `compstate[insert]=''`, `compstate[old_list]=''`, `compstate[pattern_insert]=''`, `compstate[list]='list force packed'`, and `compstate[list_max]=0`, guaranteeing that a preview never modifies `$BUFFER`.
- **Line Cap**: The preview is capped by rendered lines, not candidate count: `min(ZSH_PULSE_TYPEAHEAD_LINES (default 16), LINES - BUFFERLINES - 1)`. Group headings and the `(MORE)` marker count toward the cap. The `compadd`/`_describe` truncation wrapper is adapted from zsh-autocomplete (see [ATTRIBUTION.md](ATTRIBUTION.md)).
- **Cost**: The preview runs native completion synchronously on the ZLE thread. Its latency is therefore the native completion function's latency—milliseconds for simple contexts, potentially much longer for completion functions that evaluate many tags—not the Rust engine's sub-microsecond budget. Native completion functions may also fork. See [docs/performance.md](docs/performance.md) and [docs/known-limitations.md](docs/known-limitations.md).
- **Suppression**: The preview is skipped while `PENDING` or `KEYS_QUEUED_COUNT` is non-zero, while menu selection is active, and while the typeahead widget is already running.

### 5.2 First-Tab Full-List Expansion (`_zsh_pulse_tab`)
- **Keybinding**: `Tab` (`^I`)
- **Behavior**:
  - The typeahead preview is a capped preview only. The first `Tab` clears the previous completion state (`_lastcomp=()`, `_ZSH_PULSE_LAST_TYPEAHEAD=""`, `_ZSH_PULSE_TYPEAHEAD_VISIBLE=0`) and runs `expand-or-complete`, immediately showing the full, uncapped native completion list.
  - There is no hide/show toggle: `Tab` never spends a press dismissing the preview first.
  - When navigating inside an active `menuselect` menu, `Tab` immediately breaks out of the menu (`_zsh_pulse_menuselect_abort`).

### 5.3 Down-Arrow Menu Entry (`down-line-or-select`)
- **Keybinding**: `Down-Arrow` (`^[[B`, `^[OB`, `^N`, Vi `j`)
- If `_ZSH_PULSE_TYPEAHEAD_VISIBLE=1` and the buffer still matches the previewed buffer, pressing `Down-Arrow` immediately activates the `menuselect` cursor on the first candidate, enabling rapid arrow-key selection.
- If no preview is visible, performs prefix history search forward.

### 5.4 Menu Navigation & Safe Lifecycle Handlers
- **Grid Navigation**: In `menuselect`, single-keystroke navigation bindings (`^[[A`, `^[OA`, `^P`, `k` for Up; `^[[B`, `^[OB`, `^N`, `j` for Down; `^[[C`, `^[OC`, `^F`, `l` for Forward; `^[[D`, `^[OD`, `^B`, `h` for Backward) are bound directly to standard movement builtins (`up-line-or-history`, `down-line-or-history`, `forward-char`, `backward-char`). This ensures that Zsh's `complist` internal `domenu()` loop recognizes and intercepts candidate stepping without exiting menu mode.
- **Safe Acceptance (`_zsh_pulse_menuselect_accept`)**: Bound to `Enter` (`^M`, `^J`, `\r`). Automatically resets `_ZSH_PULSE_MENU_ACTIVE=0` and delegates execution directly to the internal dot-prefixed builtin `.accept-line`, bypassing third-party user hook wrappers that could cause re-entrant redraw crashes.
- **Safe Cancellation (`_zsh_pulse_menuselect_abort`)**: Bound to `Tab` (`^I`), `Esc` (`\e`), `Ctrl-C` (`^C`), and `Ctrl-G` (`^G`). Restores the original pre-search `$BUFFER` and `$CURSOR` state, resets `_ZSH_PULSE_MENU_ACTIVE=0`, and invokes `zle send-break` to cleanly exit menu selection without leaving dangling command fragments.

### 5.5 C-Level Menu-Active State Detection
- **Implementation**: `Api::is_menu_active` in [`crates/zsh-abi/src/api.rs`](crates/zsh-abi/src/api.rs)
- **The Problem**: Zsh's `$KEYMAP` variable does not always update to `"menuselect"` during certain completion states, causing redraw hooks to re-enter complist routines and trigger `SIGSEGV`.
- **The Solution**: Direct dynamic inspection of internal Zsh C globals:
  - `localkeymap`: verifies if local keymap is set.
  - `menucmp`: checks if active menu completion struct is non-null.
- The `--is-menu-active` CLI flag enables `zsh-pulse-highlight-redraw` and typeahead widgets to immediately bail out whenever Zsh is actively displaying a completion menu.

---

## 6. Configuration, Compilation & Modularity

### 6.1 Runtime Environment Toggles
Users can independently toggle features on or off in `.zshrc` without recompiling:

```zsh
# Syntax Highlighting
export ZSH_PULSE_HIGHLIGHT=1      # 1/0, true/false, on/off

# Completion: native compsys typeahead preview & bounded path scanner
export ZSH_PULSE_COMPLETION=1     # 1/0, true/false, on/off

# Maximum rendered lines in the typeahead preview (default: 16)
export ZSH_PULSE_TYPEAHEAD_LINES=16

# History Prefix & Menu Search
export ZSH_PULSE_HISTORY=1        # 1/0, true/false, on/off

# Inline Autosuggestions
export ZSH_PULSE_AUTOSUGGEST=1    # 1/0, true/false, on/off
```

### 6.2 Cargo Compile-Time Feature Flags
The Rust engine supports fine-grained compile-time modularity in [`crates/zsh-module/Cargo.toml`](crates/zsh-module/Cargo.toml):

| Feature Flag | Default | Description |
|---|---|---|
| `highlight` | Yes | Includes incremental lexer, span diffing, and `zsh-pulse-highlight` builtin. |
| `autocomplete` | Yes | Includes bounded filesystem scanner, ranking engine, and `zsh-pulse-complete`. |
| `history` | Yes | Includes in-memory history log parsing and fuzzy matching. |
| `history-navigation`| No | Enables extended history navigation widgets. |

### 6.3 Wordcode Compilation (`.zwc`)
- All shell scripts (`init.zsh`, `widgets.zsh`, `completion.zsh`) are pre-compiled into Zsh wordcode (`.zwc`) during installation via `zcompile`.
- **Benefits**:
  - Strips shell parsing latency and comment tokenization on startup.
  - Reduces total plugin initialization time to **0.17 ms**.
  - Allows Zsh to directly memory-map (`mmap`) the bytecode into process memory.

