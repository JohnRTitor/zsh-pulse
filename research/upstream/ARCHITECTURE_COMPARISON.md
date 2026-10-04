# Architecture Comparison

## Runtime models and ZLE integration

| Project | Runtime | Parsing | Async | Completion | History | Integration | Key Idea | Key Problem |
|---|---|---|---|---|---|---|---|---|
| **fast-syntax-highlighting** | in-process shell script | zsh `${(z)}` tokenize + `proc_buf` word split, **full re-run per keystroke** | none | none | none | `zle -N` wrapper on **every** widget | wrap all widgets; cheap per-token classify | full rebuild per keystroke; widget-wrap cost scales with widget count |
| **zsh-syntax-highlighting** | in-process shell script | `${(z)}` tokenize + per-token `proc_buf` chop (O(n²)) | none | none | none | `add-zle-hook-widget` / `zle-line-pre-redraw`, paste guard | declarative `zstyle` states, unclosed-quote styles | per-token string re-chop; no incremental parse; no time budget |
| **zsh-autocomplete** | in-process zsh + **forked pty probe** | delegated to native zsh completion | `zasync` fd + `zle -F` | wraps native `_complete`; keeps `_comps` | zsh `$history` via `(r)` glob scan | `zle -C` completion widgets + `zle -F` | delegate to zsh, async the slow parts | forks a **second zsh via zpty** just to size the menu; per-keystroke `$( )` + array copies |
| **zasync** | in-process zsh + process substitution | n/a | `<( worker )` + `zle -F` | n/a | n/a | `zle -F` fd callback | slot→fd bidirectional map, one owner, always close | none material; 570 lines and correct |
| **zsh-autosuggestions** | in-process zsh + fork per keystroke | n/a | `exec {fd}< <(...)` + `zle -F` | none (patches `_complete` output) | `${history[(r)pattern]}` linear glob scan | `zle -N` wrappers + `POSTDISPLAY` | ghost text via `POSTDISPLAY` | **linear scan of whole history per keystroke**, in the interpreter, plus a fork |
| **ble.sh** | in-process **Bash** (reimplements editor) | incremental: state-checkpoint early-break + tail splice | fork + cooperative polling every 50 iters | own completion engine, 11k lines | own history file | readline `bind -x` — **no ZLE at all** | cost ∝ distance-to-convergence, not line length | `complete_limit` **disabled by default** → unbounded TAB latency on huge dirs |
| **zshrs** | native Rust **reimplementation of zsh** | Rust lexer + AST + packed attr bitmap | Rust threads | own compsys | own | Rust `znative` ABI via `zmodload -R` | full control of lexer/ZLE in Rust | requires zshrs's own shell; no published perf numbers |
| **fish** | native Rust binary | hand-written lexer → AST → walk → highlight spans | worker threads | own completion | own SQLite-backed history | native | spans as the internal currency | no incremental parse; whole-line AST per keystroke |
| **reedline** | native Rust crate | none (caller supplies `Highlighter`) | async | own `CompletionEngine` | own store | native | whole-line `highlight(line, cursor) -> StyledText` | full re-highlight per keystroke |
| **rustyline** | native Rust crate | none (caller supplies `Validator`) | none | own | own | native | whole-line `validate(&ctx) -> ValidationResult` | full re-validate per keystroke |
| **fzf** | Go binary | N/A | streaming goroutines | N/A | N/A | N/A | Smith-Waterman fuzzy + exact score constants | allocates per item; not designed for a synchronous keystroke |
| **mcfly / atuin** | Rust binary / daemon | N/A | N/A | N/A | frecency / SQLite | N/A | small explicit weighting; top-K via `select_nth_unstable` | out-of-process → IPC |
| **carapace-bin** | Go binary, spawned per completion | schema-driven | N/A | full framework | N/A | external command | schemas beat code | process spawn per completion |

---

## The hot path, compared

```
keypress → ZLE mutates BUFFER/CURSOR → [work] → region_highlight → redraw
```

| Project | Where work happens | Full reparse per keystroke? | Spawns a process per keystroke? |
|---|---|---|---|
| FSH | `_zsh_highlight` wrapped around **every** widget | **Yes** whenever buffer modified (`fast-syntax-highlighting.plugin.zsh:93-100`) | No |
| zsh-syntax-highlighting | `zle-line-pre-redraw` | **Yes** (`main-highlighter.zsh:586-592`) | No |
| zsh-autocomplete | completion widget + async helper | No (delegates) | **Yes** — pty probe + `$( )` |
| zsh-autosuggestions | wrapped widgets | n/a (history scan) | **Yes** — `exec {fd}< <(...)` per keystroke |
| ble.sh | own editor loop | **No** — checkpoint splice (`core-syntax.sh:5540-5550`) | Yes (async paths) |
| fish | Rust highlighter | **Yes** (whole-line AST) | No |
| reedline / rustyline | caller callback | **Yes** (whole-line) | No |

---

## Minimum computation after one keystroke

The question reduces to: *how much of the line must be re-examined after a
character is inserted or deleted?*

| Strategy | Work after 1 keystroke | Found in | Cost |
|---|---|---|---|
| Full reparse | O(line length) | fish, reedline, rustyline, FSH, z-sy-h | simple, always correct |
| Cached parse + diff | O(line) compare, then O(spans) apply | (our target) | needs span diffing |
| **State-checkpoint resume** | **O(distance to convergence)** | **ble.sh** | best observed, ~O(1) mid-line |
| Incremental lexer | O(1) amortized per token, O(k) to rescan enclosing construct | (our target) | needs care at boundaries |
| Incremental parser | O(log n)–O(k) | (no production impl found) | highest complexity |

**Finding:** the only production implementation of a genuinely sub-linear
reparse is ble.sh's checkpoint splice, and it is *not* in zsh and *not* in Rust.
That is the gap this project targets.