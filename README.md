# zsh-pulse

[![CI](https://github.com/JohnRTitor/zsh-pulse/actions/workflows/ci.yml/badge.svg)](https://github.com/JohnRTitor/zsh-pulse/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust: 1.85+](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)
[![Zsh: 5.8+](https://img.shields.io/badge/zsh-5.8%2B-lightgrey.svg)](https://www.zsh.org)

**zsh-pulse** is a fast, tiny, high-performance shell enhancement engine for [Zsh](https://www.zsh.org), implemented in Rust with native Zsh completion integration.

It provides real-time incremental syntax highlighting, bounded filesystem completions, Fish-style inline autosuggestions, interactive history navigation, and a typeahead preview of Zsh's own completion candidates. The Rust highlighting and autosuggest paths run in-process with **sub-microsecond keystroke latency** (~276 ns module call, ~0.8 µs re-lexing) and **zero process forks**. The typeahead preview deliberately runs Zsh's native completion system synchronously, so it pays that system's cost rather than the Rust path's (see [performance](docs/performance.md)).

**Zero dependencies:** no third-party compile-time crates and no external runtime binaries, background daemons, or plugin frameworks required—just Zsh and the native module (optional `fzf` integration is supported when installed).

---

## Why zsh-pulse?

Traditional Zsh plugins (such as `fast-syntax-highlighting`, `zsh-autocomplete`, and `zsh-autosuggestions`) rely heavily on shell-script loops, frequent subshell forks (`$(...)`), and unconstrained filesystem walks. As your shell configuration and history grow, this results in noticeable typing stutter, cursor desynchronization, and multi-second freezes when navigating large directories like `/nix/store` or `/usr`.

`zsh-pulse` solves this by executing natively inside Zsh as a loadable C-compatible dynamic module (`zshpulse/core.so`), backed by modular Rust crates.

| Metric / Capability | `zsh-pulse` | Traditional Zsh Plugins (`fsh`, `zsh-autocomplete`, `zsh-autosuggestions`) |
|---|---|---|
| **Keystroke Highlighting Latency** | **~0.86 µs** (incremental Rust lexer) | **~40–200 µs** (Zsh script AST walk) |
| **Native Module Invocation** | **~276 ns** (direct C ABI call via `dlsym`) | N/A (runs shell functions) |
| **Subshells on the Rust Keystroke Path** | **0** (strictly zero forks / `clone` / `execve`) | 1 or more per keystroke or completion |
| **Typeahead Completion Preview** | Native `compsys` candidates, synchronously listed while typing | Async worker subprocess or no preview |
| **Large Directory Scan (`/nix/store`)** | **20 ms** (bounded Rust scanner) | **1.6 s – 3.3 s lockup** (unbounded shell glob) |
| **Ghost Text Autosuggestions** | In-memory history matching (< 10 µs) | Linear disk/history scan in shell script |
| **Suggestion Word-Accept** | `Alt-Right` / `Alt-F` word-by-word accept | Line-only accept or complex shell hooks |
| **History Menu Search** | Instant multi-line fuzzy menu on `Ctrl-R` | External binary spawn (e.g. `fzf`, `fzy`) |
| **Memory Allocation on Keystroke** | Single reused buffer, ≤ 1 allocation/edit | Significant heap churn in Zsh parameter tables |
| **Shell Startup Overhead** | **0.17 ms** (with `.zwc` wordcode compilation) | Often 50–200 ms |

The zero-fork and sub-microsecond figures describe the Rust highlighting, autosuggest, and history paths. The typeahead preview is intentionally a different path: it runs Zsh's own completion system synchronously so that the preview matches Tab exactly. Native completion functions may fork, and their latency is measured in milliseconds; the numbers are in [docs/performance.md](docs/performance.md).

---

## Quickstart & Installation

### Option 1: Nix Flake / Home Manager (Recommended)

`zsh-pulse` provides a complete Nix Flake with pre-compiled `.zwc` wordcode and automatic module path wiring.

#### Try without installing:
```bash
nix run github:JohnRTitor/zsh-pulse
```

#### Home Manager configuration:
```nix
# flake.nix inputs:
inputs.zsh-pulse.url = "github:JohnRTitor/zsh-pulse";

# In your home-manager configuration:
{ inputs, pkgs, ... }: {
  home.packages = [
    inputs.zsh-pulse.packages.${pkgs.system}.default
  ];

  programs.zsh = {
    enable = true;
    initExtra = ''
      source ${inputs.zsh-pulse.packages.${pkgs.system}.default}/share/zsh-pulse/init.zsh
    '';
  };
}
```

### Option 2: Manual Build & Installation

Prerequisites: Rust 1.85+ (`cargo`), Zsh 5.8+, and a C compiler (`clang` or `gcc`).

```bash
# 1. Clone the repository
git clone https://github.com/JohnRTitor/zsh-pulse.git
cd zsh-pulse

# 2. Build release module and compile Zsh wordcode
cargo build --release
./bench/install.sh

# 3. Add to ~/.zshrc
echo 'source "'$(pwd)'/zsh/init.zsh"' >> ~/.zshrc
```

---

## Keybindings Reference

`zsh-pulse` integrates smoothly into both Emacs and Vi keymaps (`main`, `emacs`, `viins`, `vicmd`, `menuselect`).

| Keybinding | Context | Action | Behavior |
|---|---|---|---|
| `Tab` (`^I`) | Buffer editing | `zsh-pulse-tab` | Expands the full, uncapped native completion list on the first press. The list shown while typing is a capped preview. |
| `Tab` (`^I`), `Esc`, `^C`, `^G` | `menuselect` | `_zsh_pulse_menuselect_abort` | Restores original buffer and cursor, resets menu active state, and aborts menu via `send-break`. |
| `Shift-Tab` / `^[[Z` | Any | `reverse-menu-complete` | Cycles backwards through completion candidates. |
| `Down` (`↓`, `^N`, `j`) | Buffer editing | `down-line-or-select` | Enters menu selection when a typeahead preview is visible; otherwise prefix history search forward. |
| `Up` (`↑`, `^P`, `k`) | Buffer editing | `up-line-or-search` | Fish-style prefix history search backward for matching commands. |
| `Ctrl-R` | Any | `fzf-history-widget` / `zsh-pulse-history-search` | Non-destructive interactive history search. Preserves and invokes `fzf` when installed; falls back to native sub-microsecond in-memory menu. |
| `Right` (`→`, `^F`), `End` (`^E`) | End of buffer | `forward-char` | Accepts the entire autosuggestion ghost text (`$POSTDISPLAY`). |
| `Alt-Right`, `Alt-F`, `Ctrl-Right` | End of buffer | `forward-word` | Accepts the next single word of ghost text (Fish parity). |
| `hjkl` / Arrow Keys | `menuselect` | Grid Navigation | Single-keystroke candidate navigation across grid rows and columns (`up/down/forward/backward-char`). |
| `Enter` (`^M`, `^J`, `\r`) | `menuselect` | `_zsh_pulse_menuselect_accept` | Resets menu state and executes `.accept-line` directly, cleanly submitting selected candidate. |

---

## Configuration & Environment Toggles

Configure `zsh-pulse` by setting environment variables in your `.zshrc` **before** sourcing `init.zsh`.

### Feature Toggles
Disable specific subsystems without breaking other features (accepts `0`, `false`, `off`, or `no`):

```zsh
# Disable syntax highlighting (default: 1)
export ZSH_PULSE_HIGHLIGHT=1

# Disable native-compsys typeahead preview & bounded path completion (default: 1)
export ZSH_PULSE_COMPLETION=1

# Disable Up/Down prefix history search & Ctrl-R menu (default: 1)
export ZSH_PULSE_HISTORY=1

# Disable Fish-style inline autosuggestions (default: 1)
export ZSH_PULSE_AUTOSUGGEST=1

# Disable automatic fzf preservation/integration on Ctrl-R (default: 1)
# (When set to 0, zsh-pulse always uses its built-in native menu search)
export ZSH_PULSE_USE_FZF=1
```

### Advanced Performance & Cache Tuning

| Environment Variable | Default | Purpose |
|---|---|---|
| `ZSH_PULSE_USE_FZF` | `1` | Detects `fzf-history-widget` or `fzf` command and preserves official fzf on `Ctrl-R`. Set to `0` to force native menu. |
| `ZSH_PULSE_MODULE_PATH` | `<repo>/bench/mp` | Directory containing `zshpulse/core.so`. |
| `ZSH_PULSE_COMMAND_CACHE_TTL` | `2` | Cache TTL in seconds for shell command lookups (aliases, functions, builtins). |
| `ZSH_PULSE_COMMAND_CACHE_MAX` | `256` | Maximum entries retained in the command validation cache. |
| `ZSH_PULSE_TYPEAHEAD_LINES` | `16` | Maximum rendered lines in the typeahead preview. The effective cap is `min(value, LINES - BUFFERLINES - 1)`; headings and `(MORE)` count toward it. |
| `ZSH_PULSE_MAX_SUBSTITUTION_DEPTH`| `4` | Maximum recursion depth for nested command substitutions `$(...)`. |
| `ZSH_PULSE_MAX_LINE` | `4096` | Byte length ceiling; lines beyond this bypass highlighting to prevent editor lag. |

---

## Architecture Overview

`zsh-pulse` is organized as an 8-crate Cargo workspace designed for strict memory safety, zero build-time C header dependencies, and modular reuse:

```
                         ┌───────────────────────┐
                         │      zsh-module       │  cdylib: zshpulse/core.so
                         │  (C ABI module entry) │  Builtins: zsh-pulse-highlight
                         └───────────┬───────────┘            zsh-pulse-complete
                                     │
           ┌─────────────────────────┼─────────────────────────┐
           ▼                         ▼                         ▼
  ┌─────────────────┐       ┌─────────────────┐       ┌─────────────────┐
  │     zsh-abi     │       │     engine      │       │   completion    │
  │ (dlsym, FFI,    │       │ (Worker thread, │       │ (Bounded FS,    │
  │  panic guards)  │       │  rev counters)  │       │  prefix scan)   │
  └─────────────────┘       └─────────────────┘       └───┬─────────┬───┘
                                                          │         │
                   ┌──────────────────────────────────────┘         ▼
                   ▼                                       ┌─────────────────┐
          ┌─────────────────┐                              │     ranking     │
          │     history     │                              │ (fzf scoring,   │
          │ (In-memory log, │                              │  top-K sort)    │
          │  fast parsing)  │                              └─────────────────┘
          └─────────────────┘
                   ▲
                   │
  ┌────────────────┴────────────────┐
  │              lexer              │  Pure Rust incremental lexer
  │ (Checkpoint states, tail-splice)│  Rules modeled after Zsh lx1/lx2
  └────────────────┬────────────────┘
                   ▼
          ┌─────────────────┐
          │    highlight    │  Span merging, overlay styling,
          │ (Spans, diffs,  │  bracket matching, byte/char offsets
          │  paired quotes) │
          └─────────────────┘
```

1. **`zsh-abi`**: Encapsulates all unsafe FFI code. Resolves Zsh internal symbols (`localkeymap`, `menucmp`, `getsparam_u`, `setaparam`, `zalloc`, `ztrdup`) dynamically via `dlsym`. Enforces panic boundaries (`catch_unwind`).
2. **`lexer`**: Pure Rust incremental lexer (`#![forbid(unsafe_code)]`). Slices command lines into semantic tokens and caches parse states at token boundaries for sub-microsecond incremental updates.
3. **`highlight`**: Computes styled spans, manages bracket and quote pair matching, handles non-overlapping overlay slicing, and converts byte indices to Unicode character positions for `region_highlight`.
4. **`completion`**: Fast bounded filesystem scanner (`complete_paths`) and completion dispatcher. Enforces strict iteration limits to prevent directory hangs.
5. **`ranking`**: Pure Rust implementation of the `fzf` Smith-Waterman fuzzy scoring algorithm, complete with exact bonus constants and $O(N)$ `select_nth_unstable` partitioning.
6. **`history`**: In-memory parser for plain and extended Zsh history timestamps (`: <timestamp>:<duration>;<cmd>`). Eliminates disk I/O on keystroke paths.
7. **`engine`**: Manages monotonic revision counters and coordinates optional background worker threads.
8. **`zsh-module`**: Bridges Rust engine state to Zsh's C execution model, exporting builtins `zsh-pulse-highlight` and `zsh-pulse-complete`.

---

## Further Documentation

- **[FEATURES.md](FEATURES.md)**: Exhaustive catalog of all syntax highlighting, autosuggestion, completion, menu navigation, and compilation features.
- **[ATTRIBUTION.md](ATTRIBUTION.md)**: Formal attribution, licenses, and algorithmic provenance for all borrowed and inspired upstream open-source code.
- **[docs/architecture.md](docs/architecture.md)**: In-depth technical architecture, crate relationships, data flows, and concurrency model.
- **[docs/performance.md](docs/performance.md)**: Benchmark methodologies, microsecond latency breakdowns, and comparison against alternatives.
- **[docs/zle-integration.md](docs/zle-integration.md)**: Mechanics of ZLE hook integration (`zle-line-pre-redraw`, `list-choices`, `menuselect`).
- **[docs/rust-zsh-boundary.md](docs/rust-zsh-boundary.md)**: Memory management rules, allocator contracts (`zalloc`/`ztrdup`), and dynamic symbol resolution.
- **[docs/known-limitations.md](docs/known-limitations.md)**: Explicit boundaries, platform compatibility, and non-goals.
- **[docs/archive/tasks-milestones.md](docs/archive/tasks-milestones.md)**: Consolidated development logs and completed milestone history.

---

## License

This project is licensed under the [MIT License](LICENSE).
For third-party open-source code, algorithmic designs, and upstream project attributions, see [ATTRIBUTION.md](ATTRIBUTION.md).

