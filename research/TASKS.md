# Task List

Derived from [`PLAN.md`](PLAN.md). Checked off as each task is completed.
**A task is only checked when it is done and verified — never when stubbed.**

Legend: `[ ]` todo · `[~]` in progress · `[x]` done · `[!]` blocked

---

## Stage B — Boundary spike
*Gate: all three open risks retired before any product code is written.*

- [x] **B1** Cargo workspace + `zsh-abi` crate: `#[repr(C)]` structs,
  `extern "C"` decls, `#[unsafe(no_mangle)]` entry points
  *(done — and stronger than planned: **zero** zsh symbols are linked, they are
  all resolved via `dlsym`, so the crate links and unit-tests normally)*
- [x] **B2** Capability probe — `dlsym` every required symbol at load; fail
  the load cleanly and select the fallback on any miss *(retires: export
  variance — verified working; all 6 symbols resolve on this zsh)*
- [x] **B3** Export both symbol forms (`setup_` and
  `setup_shellpulseQshighlight`); unused one ignored *(retires:
  `DYNAMIC_NAME_CLASH_OK` variance)*
- [x] **B4** `catch_unwind` guard at every entry point — a Rust panic must
  never unwind into zsh *(verified by 1000-panic test)*
- [x] **B5** `$module_path` packaging (`shellpulse/highlight.so`) +
  `zmodload shellpulse/highlight`
- [x] **B6** Per-call cost measured with a reproducible harness
  (`bench/bench_module_call.zsh` + `bench/install.sh`).

  | | per native call |
  |---|---|
  | First implementation | **641 ns** |
  | After removing per-call allocations | **276 ns** |
  | Budget | ≤1 µs |

  Measured A/B on the same harness, 200 000 calls, 28-byte buffer.
  **Under budget with 3.6× headroom.** The 1.2 µs figure quoted earlier came
  from an ad-hoc command that is no longer reproducible; the harness now
  fails loudly if the buffer fails to expand rather than silently measuring an
  empty one.
- [x] **B7** Thread probe: a std worker thread inside real zsh, answering work
  over a channel without ever calling into zsh.

  **Verdict: safe, with one measured caveat.**

  | Check | Result |
  |---|---|
  | Thread starts inside live zsh and round-trips a job | ✅ `worker ready: rev=0 checksum=…` |
  | zsh thread count goes 1 → ≥2 | ✅ verified in-process |
  | 600 signal deliveries (SIGWINCH/SIGCONT) while the worker lives | ✅ zsh survives, builtin still works |
  | Per-call cost after adding the worker | ✅ 290 ns (was 276 ns — within noise) |
  | Engine works with zero workers | ✅ `Worker::spawn` returns `Result`; `shared()` yields `None` |

  **Caveat:** only *harmless* signals were tested. Signals tied to job control
  (SIGCHLD, SIGINT) arriving while zsh is actually running jobs are **not**
  covered, and the theoretical hazard — zsh's handler running on our thread's
  stack — is reduced, not eliminated, by these results. Mitigation remains
  structural: the worker holds no zsh handle and the engine degrades to
  single-threaded if it cannot start.

### Provenance note

A `crates/engine/src/worker.rs` appeared in the working tree that was not
authored in this session. On inspection it contained **15 NUL bytes embedded in
identifiers and syntax** — e.g. `static WORKER: Once<NUL>OnceLock<NUL>Worker>` —
so it could never have compiled, and it was not declared as a module either.
It was a corrupt artifact, not a working draft. The design in its doc comments
was sound, so it was reimplemented from scratch rather than patched.

### Test-harness bugs found (worth remembering)

- Parallel `cargo test` runs raced copying the module into one destination,
  producing a truncated `.so` that segfaulted zsh — which looked exactly like an
  ABI bug. Fixed with a `OnceLock`.
- The `DEGRADED` global is shared, so three guard tests clobbered each other
  under parallel execution. Fixed with a test mutex.
- **The integration tests were running a stale `.so`.** `cargo test` builds the
  crate's rlib for the harness but does not refresh the `cdylib`, so tests
  validated code from hours earlier. `module_so()` now rebuilds when the
  artifact is older than any source — and immediately caught a stale assertion.
- The first benchmark harness left `${(qqq)LINE}` for the inner shell to expand,
  silently measuring an **empty** buffer; then `${(qqq)}`'s double quotes broke
  the command entirely. The harness now fails loudly on either.
- Counting `/proc/self/status` threads is unreliable under parallel tests.
  Tests now count threads *named* `shellpulse-wrk`, which is immune to
  scheduling.

**Exit criteria status**

| Criterion | Result |
|---|---|
| Loads under `zsh -f` | ✅ pass |
| Loads under the real config (1,942 comps / 451 widgets measured in a non-tty run) | ✅ pass — load, builtin, and `BUFFER`/`CURSOR` all verified |
| No crash with symbols deliberately removed | ✅ pass (`a_missing_module_fails_cleanly…`) |
| ≤1 µs/call | ✅ **276 ns** (was 641 ns; harness in `bench/`) |
| Failing ABI test fails the test, not the shell | ✅ 10 subprocess-based tests |

Stage B is complete. All three open risks are retired. The Stage C blocker is
the `setaparam` double-write corruption (#4 below), not threading.

### Findings that changed the design

1. **A Rust `&str` is not NUL-terminated.** Handing one to zsh as `char *`
   produced a builtin named `shell-pulse-SHELL_PULSE_DEBUG` and corrupted zsh's
   builtin table. `CStr` now only accepts `c"…"` literals, and a test asserts
   the trailing NUL really exists.
2. **`setaparam` takes ownership** of the value array (`assignaparam` calls
   `freearray` on every path). Passing a Rust stack array segfaulted zsh; values
   must be allocated by `zhalloc`/`dupstring`.
3. **`setaparam`'s return value is meaningless** — it returns a `Param` pointer
   truncated to `int`.
4. **Writing a parameter twice corrupts the heap** (`free(): invalid size`).
   The write path is therefore **gated behind `SHELL_PULSE_ECHO`** and off by
   default. **This blocks Stage C's `region_highlight` push and must be
   understood before C3.**
5. ~~B6 gap~~ — **resolved.** Reading `BUFFER`/`CURSOR` no longer allocates:
   `read_param_bytes` fills a reused buffer via `getsparam_u`, and `CURSOR` is
   parsed straight to an `i64` by `parse_i64` (unit-tested, overflow-checked).
   Env-var flags are resolved once via `OnceLock` instead of per keystroke.

### Test-harness bugs found (worth remembering)

- Parallel `cargo test` runs raced copying the module into one destination,
  producing a truncated `.so` that segfaulted zsh — which looked exactly like
  an ABI bug. Fixed with a `OnceLock`.
- The `DEGRADED` global is shared, so three guard tests clobbered each other
  under parallel execution. Fixed with a test mutex.
- The first benchmark harness left `${(qqq)LINE}` for the inner shell to expand,
  silently measuring an **empty** buffer; then `${(qqq)}`'s double quotes broke
  the command entirely. The harness now fails loudly on either.

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