# shell-pulse: Ultimate Completion & Syntax Highlighting Tasks

Roadmap for achieving complete feature parity with `fish-shell` and `zsh-autocomplete` while maintaining sub-microsecond latency and zero subshells.

## Phase 1: Hybrid Completer Integration (Full Tool Support)
- [x] Configure Zsh completer pipeline: prepend `_shell_pulse_completer` before `_complete` and `_approximate`
- [x] Bounded filesystem paths scanner in Rust (`complete_paths`) to prevent freezes on massive directories (e.g., `/nix/store`)
- [x] Category grouping & descriptions (`[directories]`, `[files]`, `[history search]`, `[commands]`, `[options]`)
- [x] Fallthrough preservation of 1,000+ native Zsh tool completers (`git`, `cargo`, `docker`, `nix`, flags)

## Phase 2: Menu Navigation & Real-time Type-Ahead
- [x] Implement `down-line-or-select` widget for `↓` and `Ctrl-N` (enter menu selection immediately)
- [x] Implement `up-line-or-search` widget for `↑` and `Ctrl-P` (open interactive multi-line history search menu)
- [x] Tab & Shift-Tab bindings (`expand-or-complete`, `reverse-menu-complete`)
- [x] Real-time Type-Ahead Auto-Listing: debounced hook in `line-pre-redraw` that displays up to 16 lines of completions below the prompt without requiring Tab
- [x] Tab unambiguous common prefix insertion during type-ahead

## Phase 3: Fish-Style Autosuggestions
- [x] Full-line autosuggestion acceptance (`Right-Arrow`, `End`, `Ctrl-F`, `Ctrl-E`)
- [x] Fish-style word-by-word autosuggestion acceptance (`Alt-Right`, `Alt-F`, `Ctrl-Right`)
- [x] Recent directories (`cdr` / `chpwd_recent_dirs`) integration for `cd` navigation

## Phase 4: Fish & FSH Syntax Highlighting Parity
- [x] Option highlighting semantics: recognize `--` as end-of-options delimiter (tokens following `--` starting with `-` are treated as words/arguments, not options)
- [x] Live filesystem path validation: valid directories highlighted and underlined (`fg=magenta,underline`), valid files highlighted (`fg=magenta`)
- [x] Bracket and quote matching:
  - [x] Recognize matching pairs of `()`, `{}`, `[]`, and quotes `""`, `''`
  - [x] Highlight paired bracket/quote when cursor touches it (`bg=blue` / `bold`)
  - [x] Highlight unclosed or mismatched brackets/quotes in red error (`fg=red,bold`)

## Phase 5: Verification & Microbenchmarks
- [x] Keystroke latency verification (sub-10µs microbenchmark)
- [x] Large directory (`/nix/store`) non-blocking completion verification
- [x] Full workspace test suite pass (`cargo test --all` and `cargo clippy --all -- -D warnings`)
