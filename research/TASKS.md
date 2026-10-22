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

### The Stage C blocker, resolved

The blocker recorded above ("writing a parameter twice corrupts the heap")
was **not** a zsh bug and **not** a race. It was an allocator mismatch, and
the root cause is now understood and fixed.

`setaparam` takes ownership of the value array and `freearray`
(`zsh-src/Src/utils.c:4122`) releases it with `free()` and each element with
`zsfree()`. The array must therefore come from `zalloc` (plain `malloc`) and
the elements from `ztrdup`. The superficially equivalent symbols `zhalloc`
and `dupstring` are **wrong**: they allocate from zsh's *pool* allocator, and
pool memory is rewound and recycled by `popheap()`
(`zsh-src/Src/mem.c:1425`), which zsh runs around every command
(`zsh-src/Src/exec.c:1275`). A parameter holding pool memory therefore
pointed at recycled memory: reading it back was a `SIGSEGV` inside
`sepjoin`.

zsh's own idiom matches the fix exactly (`zsh-src/Src/params.c:1013-1017`).
The probe now requires `zalloc` and `ztrdup` and explicitly *not* the pool
allocators, so a zsh that lacks them fails the load cleanly instead of
corrupting the heap later.

The same rule applies to the ZLE special parameter `region_highlight`: its
setter ends with `freearray(av)`
(`zsh-src/Src/Zle/zle_refresh.c:589`), so it takes ownership in exactly the
same way.

- [x] **C0** Pure-Rust crates: `lexer`, `highlight` (no zsh dependency, so
  they are unit- and benchmark-testable)
  *(done — 60 tests; the lexer's rules are taken from zsh's own `lx1`/`lx2`
  tables, not from intuition)*
- [x] **C1** Incremental lexer with serialisable checkpoints; splice on
  state convergence (port of ble.sh `core-syntax.sh:5540-5550`)
  *(done — see the acceptance result below)*
- [x] **C2** `HighlightSpan` + run-merged storage + two-pointer diff;
  `size_of` and allocation-count budget on the hot path
  *(done — `HighlightSpan` is 12 bytes, the state is 45, the spans are
  run-merged, the diff is a two-pointer merge checked against a naive
  reference implementation, and the re-lex allocates **once** in the steady
  state)*
- [x] **C3** `region_highlight` boundary — one `setaparam`, reuse scratch
  buffer, skip entirely when unchanged
  *(done — one `setaparam` per change, nothing at all when the styling is
  unchanged, and the entries are built in one reused buffer so a keystroke
  does not allocate per region)*
- [x] **C4** `zle-line-pre-redraw` integration: one hook, paste guard,
  wall-clock budget, bail to unstyled on unparseable input
  *(done — one hook, a 1 ms wall-clock budget, a 4 KiB line limit, a 256
  region limit, and a clean bail to unstyled)*
- [x] **C5** Zsh adapter (`zsh/init.zsh`) + loadable fallback path
  *(done — the adapter loads the module and installs the hook, chains an
  existing hook rather than replacing it, and does nothing at all if the
  module cannot be loaded)*

### Acceptance for C1 — the numbers

`cargo run --release -p lexer --example bench_incremental` inserts one
character in the middle of a command line and compares an incremental
re-lex with a full re-parse. The "policy" column is what the module
actually does: the incremental path for a long line, a full re-parse for a
short one.

| bytes | full re-parse | incremental | policy | speed-up | bytes re-scanned |
|---|---|---|---|---|---|
| 17 | 0.08 µs | 0.06 µs | 0.06 µs | 1.33× | 18 |
| 46 | 0.10 µs | 0.08 µs | 0.08 µs | 1.25× | 47 |
| 86 | 0.19 µs | 0.16 µs | 0.15 µs | 1.27× | 21 |
| 169 | 0.36 µs | 0.21 µs | 0.21 µs | 1.71× | 10 |
| 354 | 0.63 µs | 0.34 µs | 0.34 µs | 1.86× | 14 |
| 694 | 1.12 µs | 0.58 µs | 0.59 µs | 1.90× | 21 |
| 1 389 | 1.94 µs | 1.08 µs | 1.08 µs | 1.80× | 10 |

**The scan is sub-linear**: the line grew 81.7× and the bytes re-scanned grew
0.6×. The work is the distance to convergence, not the length of the line.

**The end-to-end cost is still linear**, and the harness says so rather than
hiding it: the token offsets after the edit move, so the token list has to
be rebuilt. What the incremental path buys is a much smaller constant —
1.25× to 1.90× faster at every size measured.

**The end-to-end cost of a keystroke**, measured through zsh with
`bench/bench_module_call.zsh` (which now exercises the real path: a
`region_highlight` parameter exists, and the line changes on every
iteration): **0.83–0.89 µs per keystroke**, against a target of 10 µs. The
lexer's own share of that is about 0.1 µs.

**That number was 0.48 µs until the harness was corrected.** The old
harness alternated between two lines that produced *identical* spans, so
the module took its "the styling has not changed" path and published nothing
after the first iteration: what it measured was the cost of doing nothing.
The harness now refuses to time anything unless the module has really
published, and the two lines differ in their styling.

**The whole round trip**, from the keystroke to the redraw, measured through
a pty with `bench/bench_zpty.zsh`: **68.9 µs**. That number is dominated by
zsh's own line editor and the pty, not by us — which is the point: our share
is 0.86 µs of it.

**The fallback, and why there is one.** Below 128 bytes the incremental path
was *slower* than a full re-parse: the diff and the merge cost more than
the scan they save. The plan says to report that and fall back, so
`LexCache::relex` re-parses a short line outright
(`INCREMENTAL_MIN_LEN`), and `relex_incremental` is the raw machinery,
which the property tests exercise on their own so that it stays covered.

The threshold was re-measured when the benchmark's baseline was corrected:
the "full re-parse" column used to be the free `lex()` function, which
allocates two fresh vectors on every call, while the module's own full parse
reuses its buffers. The comparison was therefore between an allocating and
an allocation-free implementation, and the speed-ups it reported were
allocator overhead. Against the right baseline the incremental path saves
57–809 ns per edit above 128 bytes, and is *slower* below it.

The harness fails loudly if the properties stop holding, and it checks that
the incremental result is byte-for-byte the same as a full parse.

### The design decisions that were not obvious

1. **The lexer state is exactly comparable, never "close enough".** The
   splice reuses the old tail, so the two parses must agree about the
   grammar, not merely resemble each other. `LexState::same_parse` is
   `==`.
2. **A resume point records the character that decided where its unit
   starts** (`guard`). Editing that character can move the unit — changing
   the newline that ended a here-document delimiter, for instance — which
   makes the resume point invalid. Without the guard, the incremental path
   produced *wrong* highlighting, which is worse than none.
3. **The first unit of a spliced tail cannot have its guard shifted by the
   length of the edit**, because the unit before it may have changed
   length. Getting this wrong made the incremental path produce a
   *different token kind* (`$$` is a parameter expansion, a lone `$` is
   not), which is a wrong highlight rather than a missing one. The
   regression test is
   `a_chained_edit_that_changes_a_token_kind_agrees_with_a_full_scan`.
4. **The flags are per stack level.** `expect_command` is a property of the
   current context: inside `$( ... )` the next word is a command, but the
   word after the closing `)` is an argument again. One global flag lost
   that.
5. **The scan stops at the convergence point.** Re-scanning the whole tail
   and then throwing it away was the first implementation; the benchmark is
   what showed it.
6. **`region_highlight` is recreated for every widget call**
   (`zsh-src/Src/Zle/zle_main.c:1533`), but the array that drives the
   display is not. So "do nothing when nothing changed" is correct — and
   the module must check that the parameter exists before writing to it, or
   it would create a *global* `region_highlight` in the user's shell.
7. **The module only writes to `region_highlight` when it is asked to.**
   The hook passes `--region-highlight`; without it the module only reads
   the line. A user's own global `region_highlight` is therefore never
   touched, and the module never creates one.
8. **The plugin chains an existing `zle-line-pre-redraw` hook** rather than
   replacing it. zsh allows one widget per name, and other plugins use that
   hook too.

### Known limitations

- The end-to-end cost of a keystroke is linear in the length of the line,
  because the token offsets after the edit move. Making it sub-linear needs
  the tail's offsets to be stored relative to a stable anchor, which the
  `region_highlight` API does not allow (it takes absolute character
  offsets).
- The ZLE path is now covered by `crates/zsh-module/tests/zsh_pty.rs`, which
  drives a real interactive zsh through a pty — but only on a machine that
  has the `zsh/zpty` module and a usable terminal. Everywhere else the
  test skips with a printed reason.
- The `region_highlight` array is replaced wholesale, so clearing it also
  clears any entries another plugin added. Our own entries carry
  `memo=shell-pulse` so that they can be told apart, but the merge is not
  implemented.

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
  *(the lexer, span and allocation measurements exist as
  `crates/lexer/examples/bench_incremental.rs` and
  `crates/lexer/tests/alloc_budget.rs`; criterion itself is not set up)*
- [x] **E2** `zpty` interactive latency harness (keystroke → render)
  *(done — `bench/bench_zpty.zsh` and `crates/zsh-module/tests/zsh_pty.rs`.
  The test drives a **real interactive zsh** through a pty and asserts on the
  terminal's byte stream: the `fg=cyan,bold` attribute reaches the screen as
  `ESC [ 1 m ESC [ 3 6 m`, and a line with nothing to highlight produces no
  such bytes. A control run without the module proves the bytes come from
  us. The harness measures **68.9 µs per keystroke** from keystroke to
  redraw, which is the whole round trip through the pty, not just our work.)*
- [x] **E3** Startup cost + `strace` syscall counts (must show zero spawns)
  *(done — `bench/bench_startup.zsh`. The module costs **0.12 ms** to load
  and the whole plugin **0.17 ms**; it adds **62 syscalls** at load time; and
  the keystroke path spawns **nothing** — zero `execve`, `clone`, `fork` —
  which the harness proves by showing that its own counter does find a child
  process spawned on purpose.)*
- [ ] **E4** Allocation profiling (DHAT)
  *(the allocation *count* on the hot path is measured and bounded — one
  allocation per re-lex, see `crates/lexer/tests/alloc_budget.rs` — but the
  allocator itself has not been profiled)*
- [ ] **E5** Head-to-head vs FSH, zsh-syntax-highlighting, zsh-autocomplete
- [ ] **E6** Compatibility matrix across realistic zsh configurations
  *(only zsh 5.9.2 on x86_64 Linux has been tested)*
- [x] **E7** Deliverables: architecture doc, performance report, ZLE
  integration design, Rust↔Zsh boundary doc, known limitations
  *(done — `research/docs/`)*

**Target:** ≤10 µs per keystroke at typical line lengths (FSH measures ~40 µs).

---

## Deferred (not started, with reasons)

- [ ] Tokio — unproven against this workload; std threads first
- [ ] Daemon — no measured need; rejected in `ARCHITECTURE_DECISION.md`
- [ ] Tree-sitter — fish hand-writes its lexer; revisit only with data
- [ ] Replacing native zsh completion — explicitly out of scope
- [ ] Bash support — zsh-only this phase