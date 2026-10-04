# Borrow / Avoid / Investigate

Every entry cites the project and file where the behaviour was found.

---

## Borrow

### 1. State-checkpoint reparse with tail splice — ble.sh
`ble.sh/lib/core-syntax.sh:5540-5550`
The parser serialises its state at each step (`ble/syntax/parse/serialize-stat`)
and, on reaching an offset `i` where the serialised state equals the state
recorded at the same offset in the previous parse, splices the saved tail and
breaks. Cost becomes proportional to *distance to convergence* rather than line
length. This is the only genuinely sub-linear reparse found in any project
studied, and it is not in Rust. **Borrow the idea; reimplement it.**

### 2. Single owner of the fd map, always closed — zasync
`zasync/Functions/.zasync.fd-callback:35-43`, `.zasync.start:27`, `.zasync.cancel:39`
Bidirectional `slot→fd` and `fd→slot` maps; the fd is closed on EOF, cancel
closes before start, and a pid-0 guard prevents signalling the wrong process
group. 570 lines, no leaks. **Borrow as the resource-ownership template.**

### 3. Time-budgeted, self-cancelling highlight pass — zshrs
`zshrs/src/Zle/zle_fx.rs:560-562`, `:608-611`
A highlight pass with a wall-clock budget that abandons itself and returns
partial spans — bounded typing latency **without** a background thread.

### 4. `$PATH`-fingerprinted command-validity cache — zshrs
`zshrs/src/highlight/syntax_highlight.rs:742-780`
Caches command existence keyed by the `$PATH` that produced the answer.

### 5. Explicit `free_cstring` instead of Rust-side `Drop` — zshrs
`zshrs/znative/src/lib.rs:138`

### 6. Bidirectional ABI version check — zshrs
`zshrs/znative/src/lib.rs:73`, `:478-480`; `src/extensions/plugin_host.rs:408-416`
Exact-equality `ABI_VERSION` checked on both sides, plugin verifying the host
*before* touching any pointer.

### 7. Staging buffers so a failed init leaves no partial registrations
`zshrs/src/extensions/plugin_host.rs:394-401`

### 8. Top-K via `select_nth_unstable` — atuin
`atuin/crates/atuin-daemon/src/search/index.rs:565-574`
O(n) partition before sorting; parallelises only above 10,000 candidates
(`index.rs:547-555`).

### 9. fzf's scoring constants — as a tuning reference
`fzf/src/algo/algo.go:112-146`
`scoreMatch=16`, `scoreGapStart=-3`, `scoreGapExtension=-1`,
`bonusBoundaryWhite=10`, `bonusBoundaryDelimiter=9`, `bonusCamel123=7`,
---

## Avoid

### 1. Wrapping every widget — FSH
`fsh/fast-syntax-highlighting.plugin.zsh:242-284`
Wraps the entire widget table, so overhead scales with widget count. On the
compatibility target that is **8,278 widgets**.

### 2. Fixed per-call shell overhead — FSH
`fsh/fast-syntax-highlighting.plugin.zsh:77-110`
`emulate -LR zsh`, `setopt`, and an array copy into `region_highlight` on every
call. Measured as the dominant cost: ~40 µs fixed, independent of line length.

### 3. Full reparse on every mutation — FSH, zsh-syntax-highlighting, fish, reedline, rustyline
`fsh/…:93-100`; `z-sy-h/main-highlighter.zsh:586-592`;
`fish/src/highlight/highlight.rs:96-106`; `reedline/src/highlighter/mod.rs:24-30`;
`rustyline/src/validate.rs:56`. Measured FSH marginal cost ≈13 ns/char, no reuse.

### 4. Per-token string re-chopping — zsh-syntax-highlighting
`main-highlighter.zsh:688` (`proc_buf` per token) plus a per-character scan per
word at `:1350-1353` — quadratic in token length.

### 5. Forking a second shell to measure an integer — zsh-autocomplete
`Functions/Init/.autocomplete__async:225`, `:267`, `:276-286`
A `zpty`-spawned zsh to read `compstate[list_lines]` under a `TMOUT` fence.

### 6. Linear history scan per keystroke — zsh-autosuggestions
`zsh-autosuggestions.zsh:662` — `${history[(r)$pattern]}` glob-scans the entire
in-memory history array.

### 7. A fork per keystroke for autosuggest — zsh-autosuggestions
`zsh-autosuggestions.zsh:790` — `builtin exec {fd}< <(...)`. Asynchrony hides
the latency; it does not remove the work.

### 8. Disabled-by-default candidate limits — ble.sh
`ble.sh/lib/core-complete-def.sh:44` — `bleopt/declare -v complete_limit ''`.
`<TAB>` in a 10,000-file directory is unbounded.

### 9. Per-candidate allocation in the matcher — fzf
`fzf/src/algo/algo.go:775-776` copies the input into a fresh `[]rune` per
candidate; `algo.go:544-545` allocates a one-element slice for `withPos`; the
default config computes positions for *every* row. In Rust: index `&[u8]` in
place, and compute positions only for rendered rows.

### 10. zshrs's plugin ABI as a dependency
`znative/src/lib.rs:87`, `src/ported/module.rs:5300-5337`. Requires zshrs's own
Rust zsh. **Cannot be used with stock zsh**; only its patterns are reusable.

---

## Investigate

1. **Rust `cdylib` module ABI on stock zsh.** Proven to load here at ~112 ns.
   Still to verify: export variance across distro builds, and whether threads
   inside the process interact safely with zsh's allocator and signal handlers.
2. **Threads inside the zsh process.** No upstream does this. zsh is not
   thread-safe; a worker must never call into zsh. Needs a real experiment.
3. **Rust port of ble.sh's checkpoint splice.** Highest-value unexplored
   direction: the algorithm is proven, the language is not.
4. **Tree-sitter for zsh.** fish does **not** use it (no `tree-sitter` in its
   Cargo tree) and hand-writes its lexer and AST — evidence that a hand-written
   lexer is viable at this scale. Still worth a measured spike before ruling it
   out.
5. **`memo=` on `region_highlight`.** zsh 5.9 added a user-settable `memo` key
   (`Src/Zle/zle.h:459-460`, parsed at `Src/Zle/zle_refresh.c:517-552`). Its
   semantics as a changed-detection signal are undocumented; probe before
   relying on it.
6. **`zle -F` cancellation granularity.** zasync closes the fd on cancel
   (`.zasync.cancel`); whether one long-lived fd with revision-tagged messages is
   cheaper needs measuring.
7. **Whether a persistent daemon ever pays off** — only for a shared history
   index, where amortisation across keystrokes changes the arithmetic. Not now.
`bonusConsecutive=4`, `bonusFirstCharMultiplier=2`. Its `history` scheme flattens
boundary bonuses to 8 (`algo.go:190-192`) — designed for shell input.
**Borrow the constants; do not borrow the matrix.**

### 10. Small explicit ranking factors — mcfly
`mcfly/src/history/history.rs:22-34` — ten named factors, cheap and explainable.

### 11. Ghost text via `POSTDISPLAY`, not by rewriting `BUFFER`
`zsh-autosuggestions.zsh:246-262`

### 12. Delegating to native completion rather than replacing it
`zsh-autocomplete` wraps `_complete` and preserves `_comps`; `carapace-bin`
supplements zsh's completion from outside rather than replacing compsys.