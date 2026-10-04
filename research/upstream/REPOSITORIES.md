# Repositories Under Study

All clones are shallow (`--depth 1`) into `research/upstream/`. Revisions recorded
2026-10-10. **Every measurement and citation in this research refers to exactly
these commits.**

| Local dir | Remote | Branch | HEAD commit | Date | License | Lang | Build system |
|---|---|---|---|---|---|---|---|
| `fsh` | github.com/zdharma-continuum/fast-syntax-highlighting | master | `4672ad5dd9ad` | 2026-08-31 | BSD-3-Clause | Shell | none (zsh script) |
| `zsh-autocomplete` | github.com/marlonrichert/zsh-autocomplete | main | `77706d4cf248` | 2026-09-13 | MIT | Shell | none (zsh script) |
| `zasync` | github.com/marlonrichert/zasync | main | `dcdc53e2ab17` | 2026-08-26 | MIT | Shell | none (zsh script) |
| `zsh-autosuggestions` | github.com/zsh-users/zsh-autosuggestions | master | `85919cd1ffa7` | 2025-06-24 | MIT | Shell | none (zsh script) |
| `zsh-syntax-highlighting` | github.com/zsh-users/zsh-syntax-highlighting | master | `0bfcb582e71d` | 2026-08-25 | BSD-3-Clause | Shell | none (zsh script) |
| `ble.sh` | github.com/akinomyoga/ble.sh | master | `d81fd54feb0d` | 2026-09-08 | BSD-3-Clause | Shell | codegen (`ble.pp`) |
| `zshrs` | github.com/MenkeTechnologies/zshrs | main | `3ee933213282` | 2026-10-03 | MIT | **Rust** | cargo workspace |
| `mcfly` | github.com/cantino/mcfly | master | `6eed8ebd879c` | 2026-08-26 | MIT | Rust | cargo |
| `atuin` | github.com/atuinsh/atuin | main | `d440a2e61682` | 2026-10-03 | MIT | Rust | cargo workspace |
| `fzf` | github.com/junegunn/fzf | master | `b1be3a8be1b8` | 2026-09-14 | MIT | Go | go build |
| `carapace-bin` | github.com/carapace-sh/carapace-bin | master | `c9cac841afb9` | 2026-09-30 | MIT | Go | go build |
| `fish-shell` | github.com/fish-shell/fish-shell | master | `35c290272cc9` | 2026-09-27 | NOASSERTION | Rust | cargo/cmake |
| `reedline` | github.com/nushell/reedline | main | `b72fd34f54ab` | 2026-10-04 | MIT | Rust | cargo |
| `rustyline` | github.com/kkawakam/rustyline | master | `05eb9200f178` | 2026-09-22 | MIT | Rust | cargo |
| `zsh-src` | github.com/zsh-users/zsh | master | `8cc5eade5ff8` | 2026-09-28 | MIT-like | C | autoconf/make |

`zsh-src` is at master, slightly newer than the 5.9.2 binary installed on this
machine; 5.9.2-specific behaviour was verified against the **installed binary**
where it differed.

---

## Classification and relevance

| Project | Category | Which part of our problem it actually solves |
|---|---|---|
| `fsh` | syntax-highlighting, ZLE-integration | The exact integration model we must beat: wraps *every* ZLE widget, rebuilds `region_highlight` per keystroke. Primary baseline. |
| `zsh-syntax-highlighting` | syntax-highlighting | Reference for zsh-idiomatic styling, nested-quote handling, and the `${(z)}` tokenization approach. |
| `zsh-autocomplete` | autocomplete, async-ZLE-integration | Primary completion baseline: async fd lifecycle, stale-result policy, pty probe for list sizing. |
| `zasync` | async-zle-integration | The fd/`zle -F` lifecycle primitive. Small (570 lines) and *correct*; a template for resource ownership. |
| `zsh-autosuggestions` | autosuggestion, history | The `POSTDISPLAY`/ghost-text model, and a documented linear-scan history failure. |
| `ble.sh` | interactive-line-editor | **Not a zsh project** (pure Bash, see below). Its *incremental* syntax-highlighting algorithm is the single most valuable idea found. |
| `zshrs` | Rust-native, interactive-line-editor, native-shell-plugin | The only prior art for "Rust shell + plugin ABI". Establishes what does **not** transfer to stock zsh. |
| `mcfly` | history, fuzzy-ranking | Frecency-style ranking weights; a tractable ranking model to imitate. |
| `atuin` | history, fuzzy-ranking, autosuggestion | History indexing (SQLite), top-K selection technique. |
| `fzf` | fuzzy-ranking | The reference fuzzy algorithm (Smith-Waterman variant) and its exact scoring constants. |
| `carapace-bin` | completion-engine, cross-shell | Schema-driven completion as an external command; relevant to keeping completion *out* of Rust. |
| `fish-shell` | syntax-highlighting, completion-engine | Rust lexer→AST→highlight-span pipeline; the closest architectural analogue to what we want to build. |
| `reedline` | interactive-line-editor | `Highlighter` trait shape: whole-line callback returning styled text. |
| `rustyline` | interactive-line-editor | `Validator` trait shape; confirms whole-line re-validation per keystroke. |
| `zsh-src` | (reference) | Ground truth for the ZLE hook, `region_highlight`, and **the loadable-module ABI**. |

### Notable exclusions
`carapace-bin` was studied but is not a design template: it is an external
command invoked per completion, which is the process-spawn model we are
rejecting. Recorded for completeness.

---

## Premise corrections found during reconnaissance

Two framings in the original brief did not survive source inspection. Both were
verified directly rather than taken on trust.

1. **ble.sh is not a zsh project.** It is pure Bash. `grep -rn '\bzle\b' --include='*.sh' src lib`
   returns **0 hits**; `grep -rn 'region_highlight'` returns **0 hits**. It hooks
   readline via `bind -x` (`src/util.sh:5517`, `src/history.sh:2072`) and
   reimplements the editor against its own canvas. It therefore has no ZLE
   widget table, no `zle -C`, and no `zle -F` — nothing in its ZLE-adjacent
   design is directly portable. Its incremental parsing algorithm still is.

2. **zshrs's plugin ABI does not work with stock zsh.** zshrs is a from-scratch
   Rust reimplementation of zsh — 493 `.rs` files, 682,626 lines under `src/`,
   and **no `Src/` directory and no C sources**. `vendor/zsh/` contains only
   data (`functions`, `help`, `info`, `man1`, `scripts`). Its host API is a table
   of Rust function pointers (`znative/src/lib.rs:109-190`) resolved by
   `zmodload -R`, which upstream zsh interprets as "remove module alias"
   (`Src/module.c:2459`). Nothing in `znative` transfers.