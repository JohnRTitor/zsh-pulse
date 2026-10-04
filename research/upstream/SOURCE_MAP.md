# Source Map

Maps each concept we rely on to its exact location, so any claim in this
research can be re-verified. Revisions are in `REPOSITORIES.md`.

## Zsh internals — the integration contract (`zsh-src`)

| Concept | File | Symbol / lines | Why it matters |
|---|---|---|---|
| Module load name validation | `Src/module.c` | `modname_ok()` :2177-2186 | Names are slash-separated identifiers; no `.`, no `.so`, no leading `/`. Explains every `zmodload` failure we hit. |
| Module entry symbol names | `Src/module.c` | `STR_SETUP` … :1554-1565 | `setup_`, `features_`, `enables_`, `boot_`, `cleanup_`, `finish_`. |
| Symbol lookup | `Src/module.c` | `module_func()` :1774-1800 | With `DYNAMIC_NAME_CLASH_OK` it is `dlsym(handle, "setup_")` unmangled; otherwise `<prefix><name with / → Qs, _ → Qu>`. Verified: unmangled on this build. |
| Example module, authoritative shapes | `Src/Modules/example.c` | :198-243 | Reference implementations of all six entry points. |
| Feature enablement contract | `Src/module.c` | :2016-2117 | Walks `enables[0..arrlen(features))`; not filling it segfaults. |
| Module builtin table | `Src/zsh.h` | `struct builtin` :1440-1448 | Must be transcribed exactly, padding included. |
| Hash node | `Src/zsh.h` | `struct hashnode` :1226-1230 | Embedded first in `struct builtin`. |
| Builtin handler signature | `Src/zsh.h` | `HandlerFunc` :1436 | `int (*)(char *, char **, Options, int)`. |
| Feature struct | `Src/zsh.h` | `struct features` :1553-1571 | Builtins + conditions + math funcs + params. |
| Scalar param read | `Src/params.c` | `getsparam()` :3037-3045 | Returns **`char *`**, not `char **` — the segfault source. |
| Unmetafied variant | `Src/params.c` | `getsparam_u()` :3048-3055 | Use this to avoid metafied bytes in Rust. |
| ZLE pre-redraw hook | `Src/Zle/zle_main.c` | `redrawhook()` :1066-1090 | Where `zle-line-pre-redraw` is invoked; our chosen integration point. |
| ZLE hook registration | `Src/Zle/zle_main.c` | :1838, :2146 | `zle-line-init` / `zle-line-finish` invocation sites. |
| Highlight region struct | `Src/Zle/zle.h` | `struct region_highlight` :435-462 | `atr`, `atrmask`, `layer`, `start`, `start_meta`, `end`, `end_meta`, `flags`, `memo`. |
| `region_highlight` param | `Src/Zle/zle_params.c` | :132-133, :169 | Array parameter with a custom getter/setter. |
| Highlight style parsing | `Src/Zle/zle_refresh.c` | :316-352 | How `zle_highlight` entries become regions. |
| `memo=` parsing | `Src/Zle/zle_refresh.c` | :448-475, :517-552 | 5.9 feature; semantics as a changed-detection signal are undocumented. |
| Builtin registry | `Src/builtin.c` | `addbuiltins()` :543 | 3-arg signature: `(name, Builtin[], size)`. |

## FSH (`fsh`)

| Concept | Location | Why it matters |
|---|---|---|
| Highlight entry point | `fast-syntax-highlighting.plugin.zsh:65-110` | `emulate -LR zsh` + `setopt` + full rebuild; the measured ~40 µs fixed cost. |
| Unchanged-buffer guard | `fast-syntax-highlighting.plugin.zsh:199-202` | Our benchmark initially measured *this* path; the numbers were discarded and retaken. |
| Length guard (opt-in only) | `fast-syntax-highlighting.plugin.zsh:89` | No default cutoff — 3351-char lines are fully highlighted. |
| Widget-table wrapping | `fast-syntax-highlighting.plugin.zsh:242-284` | Cost scales with the user's widget count (8,278 on target). |
| `region_highlight` build | `fast-syntax-highlighting.plugin.zsh:188` | `region_highlight+=("$start $end $region")`. |
| Tokenizer | `fast-highlight:1-1468` | `${(z)}`-based; whole-line each pass. |
## zsh-autocomplete / zasync

| Concept | Location | Why it matters |
|---|---|---|
| fd slot/fd bidirectional maps | `zasync:11-20` | Resource-ownership template. |
| Worker launch | `zasync/Functions/.zasync.start:33` | `<( worker )` process substitution. |
| fd close on EOF | `zasync/Functions/.zasync.fd-callback:35-43` | Closes in an always-equivalent path. |
| Process-group kill | `zasync/Functions/.zasync.cancel:37-39` | pid-0 guard before signalling. |
| Staleness by state compare | `zsh-autocomplete/Functions/Util/.autocomplete__async-save-state:1-4` and `…-same-state:1-3` | Two-key string compare of buffer/context. |
| The pty probe | `zsh-autocomplete/Functions/Init/.autocomplete__async:225,267,276-286` | Second full zsh forked to read one integer. |
| Per-keystroke fixed work | `zsh-autocomplete/Functions/Init/.autocomplete__async:94-95,127,137,345` | `$( )`, array glob, `region_highlight` copy. |
| Only real timing in the codebase | `zsh-autocomplete/Functions/Init/.autocomplete__async:2,12` | `EPOCHREALTIME` cooldown. |

## zsh-autosuggestions

| Concept | Location | Why it matters |
|---|---|---|
| History strategy | `zsh-autosuggestions.zsh:637-662` | `${history[(r)$pattern]}` — the linear scan. |
| Per-keystroke fork | `zsh-autosuggestions.zsh:766-798` | `exec {fd}< <(...)`, cancel-then-close. |
| Ghost text highlight | `zsh-autosuggestions.zsh:246-262` | `region_highlight` for the `POSTDISPLAY` region. |
| pty completion capture | `zsh-autosuggestions.zsh:605-632` | The completion strategy forks a pty too. |

## ble.sh

| Concept | Location | Why it matters |
|---|---|---|
| **Checkpoint splice** | `lib/core-syntax.sh:5540-5550` | The one sub-linear reparse found. |
| Parse-state serialisation | `lib/core-syntax.sh:5542` | `ble/syntax/parse/serialize-stat`. |
| Candidate limit default | `lib/core-complete-def.sh:44` | `complete_limit ''` — disabled. |
| Limit enforcement | `lib/core-complete.sh:2208-2220` | Only where limits are applied. |
| Highlight budget | `lib/core-syntax.sh:7787-7788` | `highlight_eval_word_limit=200`. |
| Readline hooking (no ZLE) | `src/util.sh:5517`, `src/history.sh:2072` | `bind -x`; confirms ble.sh never uses ZLE. |

## zshrs

| Concept | Location | Why it matters |
|---|---|---|
| Not a patched zsh | `src/` (493 files, 682k lines), no `Src/` | A from-scratch Rust zsh. |
| Plugin ABI table | `znative/src/lib.rs:109-190` | Rust fn pointers; unusable by stock zsh. |
| ABI version gate | `znative/src/lib.rs:73`, `:478-480` | Bidirectional exact check. |
| Explicit free | `znative/src/lib.rs:138` | `free_cstring`. |
| Entry symbol | `znative/src/lib.rs:87` | `znative_init`, unknown to zsh. |
| `zmodload -R` interception | `src/ported/module.rs:5300-5337` | Upstream zsh means "remove alias" here. |
| Staging on init | `src/extensions/plugin_host.rs:394-401` | No partial registration on failure. |
| Time-budgeted highlight | `src/Zle/zle_fx.rs:560-562,608-611` | Bounded latency without a thread. |
| `$PATH`-fingerprinted cache | `src/highlight/syntax_highlight.rs:742-780` | Command-existence cache. |
| Self-reported perf | `docs/hot_path_performance.md:36-45` | Its own table shows zshrs **slower** than zsh. |

## Fish, reedline, rustyline

| Concept | Location | Why it matters |
|---|---|---|
| Fish highlight entry | `fish-shell/src/highlight/highlight.rs:96-106` | `highlight_shell(buff, &mut Vec<HighlightSpec>, …)`. |
| Fish span representation | `fish-shell/src/highlight/highlight.rs:1366-1371` | `HighlightSpec` per token — style roles, not (start,end) pairs. |
| Fish colour-resolver cache | `fish-shell/src/highlight/highlight.rs:125-154` | Explicitly "no invalidation", one redraw lifetime. |
| Reedline `Highlighter` | `reedline/src/highlighter/mod.rs:24-30` | `highlight(&self, line, cursor) -> StyledText`, whole line. |
| Rustyline `Validator` | `rustyline/src/validate.rs:56-70` | `validate(&self, &mut ValidationContext) -> Result<ValidationResult>`. |
| Rustyline highlighter | `rustyline/src/highlight.rs:25-70` | Whole-line `Cow<str>` rewriting. |

## Ranking and history

| Concept | Location | Why it matters |
|---|---|---|
| fzf scores | `fzf/src/algo/algo.go:112-146` | The constants worth porting. |
| fzf history scheme | `fzf/src/algo/algo.go:190-192` | Boundary bonuses flattened to 8. |
| fzf bonus table | `fzf/src/algo/algo.go:268-296` | Precomputed 7×7 lookup. |
| fzf per-candidate copy | `fzf/src/algo/algo.go:775-776` | Avoid in Rust. |
| fzf full matrix | `fzf/src/algo/algo.go:934` | Why score-only rolling rows are better for us. |
| fzf i16 overflow cap | `fzf/src/algo/algo.go:737-742` | Pattern length ≤ 1000 for `i16` scoring. |
| Atuin top-K | `atuin/crates/atuin-daemon/src/search/index.rs:565-574` | `select_nth_unstable` then sort. |
| Atuin parallelism threshold | `atuin/crates/atuin-daemon/src/search/index.rs:547-555` | Goes parallel only above 10k. |
| McFly factors | `mcfly/src/history/history.rs:22-34` | Ten named, cheap ranking factors. |