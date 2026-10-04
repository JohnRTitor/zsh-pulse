# Implementation Plan — `shell-pulse`

A Rust-native interactive enhancement engine for zsh: incremental syntax
highlighting, asynchronous completion, history intelligence and fuzzy ranking,
running **inside** the zsh process.

This plan is derived from the reconnaissance in this directory. Every
architectural claim below is backed by a measurement or a source citation here,
not by assumption.

---

## Starting position (verified)

- Repo: single package `shell-pulse`, edition 2024, **zero dependencies**;
  `src/main.rs` is a hello world.
- Toolchain: rustc/cargo 1.98.1. crates.io reachable; `criterion-0.8.2`,
  `memchr-2.8.3`, `rustix-1.1.5`, `libc-0.2.190` already in the cargo cache.
- Headroom: 71 GB disk, ~2.4 GB RAM available. Rust only — **no C shim**
  (proved unnecessary, see `probe/`).
- zsh 5.9.2 (NixOS), ~1048 exported dynamic symbols.

## Evidence this plan is built on

| Finding | Where |
|---|---|
| Native module call costs **≈112 ns** | `PERFORMANCE_COMPARISON.md` §2 |
| FSH costs **≈40 µs fixed + ≈13 ns/char** | `PERFORMANCE_COMPARISON.md` §1 |
| Module ABI is `setup_`/`features_`/`enables_`/`boot_`/`cleanup_`/`finish_` | `probe/README.md` |
| A wrong accessor segfaults zsh | `PERFORMANCE_FAILURES.md` §7 |
| Only sub-linear reparse found: ble.sh checkpoint splice | `LESSONS.md` Borrow #1 |
| Two framing corrections (ble.sh is Bash; zshrs is not zsh) | `REPOSITORIES.md` |

---

## Stage B — Boundary spike (gate: retire 3 risks before product code)

**Goal:** a Rust `cdylib` that loads in zsh, registers a widget, reads and
writes live ZLE state, and refuses to load safely when symbols are missing.

| Task | Retires |
|---|---|
| B1. Cargo workspace + `zsh-abi` crate: `#[repr(C)]` `hashnode`/`builtin`, `extern "C"` decls, `setup_`/`boot_`/`cleanup_`/`finish_` (`#[no_mangle]`) | — |
| B2. **Capability probe**: `dlsym` every required symbol at load; on any miss, fail the load cleanly and select the fallback | export variance |
| B3. Export **both** symbol forms (`setup_` and `setup_shellpulse`); the unused one is ignored | `DYNAMIC_NAME_CLASH_OK` variance |
| B4. `catch_unwind` at every entry point — a Rust panic must never unwind into zsh | panic/abort containment |
| B5. `$module_path` packaging (`shellpulse/core.so`, no dots) + `zmodload shellpulse/core` | absolute-path rejection |
| B6. Measure inside a **real interactive zsh** via `zpty`: load time, per-call cost, idle RSS | validates 112 ns in ZLE context |
| B7. Thread probe: one std thread does work, hands back via fd, main thread reads it | threads inside zsh |

**Exit criteria (hard):** loads under `zsh -f` *and* under the real config
(16,551 completions / 8,278 widgets); no crash with symbols deliberately
removed; ≤1 µs/call; a failing ABI test fails the *test*, not the shell.

> Every ABI test runs `zsh -c` in a subprocess and asserts on exit status. The
> two segfaults from Stage A (`PERFORMANCE_FAILURES.md` §7) are exactly why.
---

## Stage C — Workspace, driven by evidence

```
crates/
  zsh-abi/       # FFI only: structs, symbols, capability probe, panic guards
  zsh-module/    # cdylib: entry points, widget/builtin registration,
                 #           region_highlight push
  lexer/         # zsh lexer + serialisable checkpoints
  highlight/     # spans, run-merging, diff
  completion/    # provider orchestration; delegates to native zsh first
  ranking/       # fuzzy scoring + history weighting
  history/       # background-built index
  engine/        # shared state; wires the above
zsh/             # init.zsh, widgets.zsh, completion.zsh  (~40-line adapter)
bench/           # criterion + zpty interactive harness
```

`lexer`, `highlight` and `ranking` are pure Rust with no zsh dependency, so
they are unit-testable and criterion-benchmarkable. Zsh-specific risk is
confined to two small crates.

### C1 — Lexer with checkpoints (the core idea)
Port ble.sh's splice (`ble.sh/lib/core-syntax.sh:5540-5550`):
- Lexer state at each token boundary serialises to a compact `u64` key:
  quoting stack, nesting depth, redirect state, active heredocs.
- Checkpoints every N tokens; on edit at offset `p`, resume from the nearest
  checkpoint ≤ `p`, then **compare state to the stored parse at each offset;
  the first match splices the saved tail**.
- Cost becomes *distance to convergence*, not line length.

**Acceptance:** the benchmark shows sub-linear cost on mid-line edits. If it
does not beat full-reparse, report that and fall back — the decision is
data-driven, not assumed.

### C2 — Spans and diffing
`HighlightSpan { start: u32, end: u32, style: StyleId }` stored as a
**run-merged** `Box<[HighlightSpan]>` (adjacent equal styles merged). Diff old
vs new runs with a two-pointer merge; only changed ranges reach zsh. Enforce a
`size_of` ceiling and an allocation count on the hot path.

### C3 — The `region_highlight` boundary (measured, not assumed)
`region_highlight` is a special array parameter
(`zsh-src/Src/Zle/zle_params.c:132-133`) — assignment replaces it wholesale.
So:
- If nothing changed, **do not touch it**. (Probe whether the 5.9 `memo=` key,
  `zsh-src/Src/Zle/zle.h:459-460`, can make that decision cheaper.)
- Otherwise build every entry into one reusable scratch buffer and `setaparam`
  **once**.
- Publish the conversion cost as its own microbenchmark.

### C4 — Integration via `zle-line-pre-redraw`
One hook, not a widget re-wrap — this avoids FSH's cost that scales with the
user's 8,278 widgets. Guarded by paste detection and a wall-clock budget
(zshrs's idea). On unparseable input, **bail to unstyled**; never emit a
silently wrong highlight.

---

## Stage D — Async, completion, ranking, history
---

## Stage E — Benchmarks and deliverables

Criterion micro (lexer/spans/rank) · `zpty` interactive keystroke→render ·
startup cost · `strace` syscall counts (must show **zero spawns**) · DHAT
allocation profiling · head-to-head vs FSH, zsh-syntax-highlighting and
zsh-autocomplete.

**Target: ≤10 µs per keystroke** at typical line lengths, from FSH's measured
~40 µs.

Then the eight deliverables: architecture decision document, performance
report, ZLE integration design, Rust↔Zsh boundary/ownership document,
reproducible benchmark suite, compatibility matrix, known limitations, and
working code with nothing stubbed.

---

## Risks

| Risk | Mitigation |
|---|---|
| ABI segfault kills the user's shell | Probe at load; subshell tests; panic guards (B2, B4) |
| Export variance across distro zsh builds | Fallback to native zsh; probe rather than link-and-hope |
| Threads unsafe inside zsh | B7 decides before any threading is built on |
| Checkpoint splice fails to beat full reparse | Fall back and report honestly |
| Benchmark measures the wrong path | Already bitten once — harness validates against deliberate failure |

## Non-goals (deferred, with reasons)

Tokio · daemon · Tree-sitter (fish does not use it; revisit only with data) ·
replacing native zsh completion · Bash support (this phase is zsh-only).

---

## Conventions

- One task per commit; `clippy -D warnings` clean.
- No `unwrap` outside tests.
- **No feature marked complete without a test that fails if it is stubbed.**
- No performance claim without a number attached.
- Every benchmark harness is validated against a deliberate failure.

- **D1 Async** — bounded channel + std threads (**not** Tokio; unproven here),
  one owner of the fd, closed on every path (zasync's lesson). Every request
  carries `revision: u64`; a result whose revision ≠ current is **discarded,
  never applied**.
- **D2 Completion** — native zsh first; Rust providers add filesystem, history
  and fuzzy. `compinit`/`compdef`/`zstyle` untouched. Bounded candidate set
  with explicit, reported truncation (ble.sh's failure mode).
- **D3 Ranking** — fzf's constants as a starting point
  (`fzf/src/algo/algo.go:112-146`); score-only rolling rows (O(width) scratch)
  plus a greedy backtrace for rendered rows only; `select_nth_unstable` above
  10k candidates.
- **D4 History** — background-built index; **no scan on the hot path** (the
  zsh-autosuggestions failure). Load lazily after the first interactive line.