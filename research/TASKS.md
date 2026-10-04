# Task List

Derived from [`PLAN.md`](PLAN.md). Checked off as each task is completed.
**A task is only checked when it is done and verified — never when stubbed.**

Legend: `[ ]` todo · `[~]` in progress · `[x]` done · `[!]` blocked

---

## Stage B — Boundary spike
*Gate: all three open risks retired before any product code is written.*

- [~] **B1** Cargo workspace + `zsh-abi` crate: `#[repr(C)]` structs,
  `extern "C"` decls, `#[no_mangle]` entry points
- [ ] **B2** Capability probe — `dlsym` every required symbol at load; fail
  the load cleanly and select the fallback on any miss *(retires: export
  variance)*
- [ ] **B3** Export both symbol forms (`setup_` and `setup_shellpulse`);
  unused one ignored *(retires: `DYNAMIC_NAME_CLASH_OK` variance)*
- [ ] **B4** `catch_unwind` guard at every entry point — a Rust panic must
  never unwind into zsh
- [ ] **B5** `$module_path` packaging (`shellpulse/core.so`) +
  `zmodload shellpulse/core`
- [ ] **B6** Measure in a real interactive zsh via `zpty`: load time,
  per-call cost, idle RSS
- [ ] **B7** Thread probe: std thread does work, returns via fd, main thread
  reads *(retires: threads-inside-zsh risk)*

**Exit criteria:** loads under `zsh -f` **and** the real config (16,551
completions / 8,278 widgets); no crash with symbols deliberately removed;
≤1 µs/call; a failing ABI test fails the test, not the shell.

---

## Stage C — Lexer, spans, ZLE integration

- [ ] **C0** Pure-Rust crates: `lexer`, `highlight` (no zsh dependency, so
  they are unit- and benchmark-testable)
- [ ] **C1** Incremental lexer with serialisable checkpoints; splice on
  state convergence (port of ble.sh `core-syntax.sh:5540-5550`)
- [ ] **C2** `HighlightSpan` + run-merged storage + two-pointer diff;
  `size_of` and allocation-count budget on the hot path
- [ ] **C3** `region_highlight` boundary — one `setaparam`, reuse scratch
  buffer, skip entirely when unchanged; own microbenchmark
- [ ] **C4** `zle-line-pre-redraw` integration: one hook, paste guard,
  wall-clock budget, bail to unstyled on unparseable input
- [ ] **C5** Zsh adapter (`zsh/init.zsh`, `zsh/widgets.zsh`) + loadable
  fallback path

**Acceptance for C1:** benchmark shows sub-linear cost on mid-line edits. If
it does not beat full-reparse, report that and fall back.

---

## Stage D — Async, completion, ranking, history

- [ ] **D1** Async core: bounded channel + std threads, single fd owner
  closed on every path, monotonic `revision` guard that discards stale results
- [ ] **D2** Completion: native zsh first, Rust providers for filesystem /
  history / fuzzy; `compinit`/`compdef`/`zstyle` untouched; bounded candidate
  set with reported truncation
- [ ] **D3** Ranking: fzf constants, score-only rolling rows, greedy backtrace
  for rendered rows, `select_nth_unstable` above 10k
- [ ] **D4** History: background-built index, lazy load after first
  interactive line, **no scan on the hot path**

---

## Stage E — Benchmarks and deliverables

- [ ] **E1** Criterion micro-benchmarks (lexer / spans / ranking)
- [ ] **E2** `zpty` interactive latency harness (keystroke → render)
- [ ] **E3** Startup cost + `strace` syscall counts (must show zero spawns)
- [ ] **E4** Allocation profiling (DHAT)
- [ ] **E5** Head-to-head vs FSH, zsh-syntax-highlighting, zsh-autocomplete
- [ ] **E6** Compatibility matrix across realistic zsh configurations
- [ ] **E7** Deliverables: architecture doc, performance report, ZLE
  integration design, Rust↔Zsh boundary doc, known limitations

**Target:** ≤10 µs per keystroke at typical line lengths (FSH measures ~40 µs).

---

## Deferred (not started, with reasons)

- [ ] Tokio — unproven against this workload; std threads first
- [ ] Daemon — no measured need; rejected in `ARCHITECTURE_DECISION.md`
- [ ] Tree-sitter — fish hand-writes its lexer; revisit only with data
- [ ] Replacing native zsh completion — explicitly out of scope
- [ ] Bash support — zsh-only this phase