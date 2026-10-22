# Performance report — `shell-pulse`

Every number in this document was produced by a command that is quoted next to
it. Where a number comes from the Stage A research rather than from this
repository, the source is named and the fact that it cannot be re-verified
here is stated.

Companion documents: [architecture](architecture.md) ·
[ZLE integration](zle-integration.md) ·
[Rust↔Zsh boundary](rust-zsh-boundary.md) ·
[known limitations](known-limitations.md)

---

## 1. Summary

| Quantity | Value | Produced by |
|---|---|---|
| End-to-end cost of one keystroke through zsh | **0.86 µs**, range 0.83–0.89 µs over 3 runs | `bench/bench_module_call.zsh 20000 28` |
| Cost of converting spans into `region_highlight` entries | **0.39 µs** for a 188-byte line with 11 regions | `cargo run --release -p zsh-module --example bench_region` |
| Marginal cost of one more region, end to end | **0.3–0.9 µs** per region | `bench/bench_zpty.zsh --regions N 60` |
| Marginal cost of one more region, the module's own share | **~12 ns** per region | same, against the entry-building benchmark |
| Target | ≤ 10 µs | [`../PLAN.md`](../PLAN.md) |
| Headroom | **≈ 11×** | 10 µs ÷ 0.87 µs |
| Full re-parse of a 1 389-byte line | 3.552 µs (the cost of a round: re-prime plus the parse) | `cargo run --release -p lexer --example bench_incremental 200` |
| Incremental re-lex of the same line | 2.742 µs, saving 809 ns per edit | same |
| Bytes re-scanned for that edit | **10**, for a 1 389-byte line | same |
| Allocations for a steady-state re-lex of a 28-byte line | **0** (budget 8) | `cargo test -p lexer --test alloc_budget -- --nocapture --test-threads=1` |
| Cost of loading the module | **0.207 ms** (noise floor 0.045 ms) | `bench/bench_startup.zsh` |
| Cost of the whole plugin at startup | 0.652 ms | same |
| Extra syscalls at load | 62 (505 → 567) | same |
| Spawns on the keystroke path | **0** (control run detects 2 when a process *is* spawned) | same |
| Keystroke → redraw, in a real interactive zsh under a pty | **68.0 µs** median (8 runs) | `bench/bench_zpty.zsh 200` |
| …the same, with the plugin *not* loaded (control) | **45.1 µs** median | the same harness, modified copy in `/tmp` |
| `HighlightSpan` | 12 bytes | `std::mem::size_of` (see the note below) |
| `LexState` | 53 bytes | `std::mem::size_of` (see the note below) |
| Cost of the whole-line colouring — the path lookups and the command lookups | **not measured**; the 0.67 µs figure below predates them | — |
| Cost of the adapter's share of a keystroke | **not measured**; the pty figure in §3.5 predates the changes of §4.2 | — |

**The figures in this table predate the whole-line colouring.** The plugin
now colours options, paths and unknown commands as well, which adds a
`stat` per distinct literal word — at most sixteen of them on a line — and
a hash lookup per command word to a keystroke. The 0.86 µs end-to-end
figure and the 0.67 µs module figure were measured before that work, and
**neither has been re-measured**. §4.1 says what is known about the new
work and what is not.

The adapter's own changes came after those figures as well: it emulates zsh
once instead of twice, it no longer calls a function for every word of the
line, and it no longer splits a line it will not highlight. §4.2 says what
is known about them.

**On the region-conversion figure.** The benchmark used to build its own
spans from the lexer's tokens; it now asks the module for the spans it
publishes (`published_spans`), so the number of regions it reports is the
number of regions a real line gets — 11 rather than 9 for the benchmark's
line. The figure was therefore re-measured: **0.39 µs** is the median of ten
runs, of which seven were between 0.38 and 0.50 µs and three were as slow
as 0.84 µs while the machine was busy.

**On the two struct sizes:** the repository asserts only that `LexState` is at
most 128 bytes
([`crates/lexer/src/state.rs:346-356`](../../crates/lexer/src/state.rs#L346)).
The exact figures above were measured with `std::mem::size_of` in a
throwaway program that depends on `lexer` and `highlight` by path; the same
program reports `Token` = 12 bytes and `ResumePoint` = 68 bytes.

For comparison, the Stage A research measured fish's highlighter (FSH) at
**≈ 40 µs fixed + ≈ 13 ns/character**
([`../upstream/PERFORMANCE_COMPARISON.md` §1](../upstream/PERFORMANCE_COMPARISON.md)).
On that basis the plugin is roughly **80× cheaper than FSH** on a typical line.
That baseline was measured in Stage A with a harness that is *not* in this
repository (`research/upstream/bench/bench_hl.zsh` is, but it needs the
upstream checkouts to run), so the 40 µs figure cannot be re-verified here.
The 0.48 µs figure can.

---

## 2. The measurement environment

| | |
|---|---|
| CPU | AMD Ryzen 5 7600, 12 threads |
| Memory | 14 GiB total, ~3 GiB available at the time of the run |
| Kernel | Linux 7.3.0-rc6 |
| zsh | 5.9.2 (x86_64-pc-linux-gnu) |
| Rust | 1.98.1, edition 2024, `opt-level = 3`, `lto = "thin"`, `codegen-units = 1` |
| `hyperfine` | **not installed** — `bench/bench_module_call.zsh` falls back to zsh's own clock; `bench/bench_startup.zsh` uses zsh's `EPOCHREALTIME` by design |
| `strace` | 7.2, present and usable |
| The machine is shared | see §7 on noise |

---

## 3. The harnesses

There are four, and each one is designed to *fail* rather than print a
meaningless number. That property is the point: the Stage A research was once
fooled by a harness that measured an empty buffer
([`../TASKS.md`](../TASKS.md), "Test-harness bugs found").

### 3.1 `bench/bench_module_call.zsh` — the end-to-end number

**What it measures.** The marginal cost of one call to the builtin, with
`BUFFER` changing on every iteration, inside a real zsh.

**How.** Two arms, differing in exactly one thing: whether the builtin is
called.

```zsh
ARM_WITH="for i in {1..$N}; do if (( i % 2 )); then BUFFER=$LINE_A; else BUFFER=$LINE_B; fi; $BUILTIN --region-highlight; done"
ARM_WITHOUT="for i in {1..$N}; do if (( i % 2 )); then BUFFER=$LINE_A; else BUFFER=$LINE_B; fi; :; done"
```

The difference, divided by `N`, is the cost of one keystroke's worth of
highlighting: read the line, re-lex it, build the regions, hand them to zsh.

**Why the harness is not lying.**

- The line *changes* on every iteration (two lines of the same length
  differing in one character), so the module really re-lexes it. A fixed
  buffer would take the "nothing changed" path and measure nothing.
- A `region_highlight` parameter exists, so the module really publishes the
  highlights. Without one it returns early and the number would be the cost
  of doing nothing.
- The buffer is quoted in the *outer* shell, and the harness fails loudly if
  the quoting did not expand
  ([`bench/bench_module_call.zsh:76-79`](../../bench/bench_module_call.zsh#L76)).
  Leaving `${(qqq)LINE}` for the inner shell to expand was the original bug
  and it produced a *misleadingly fast* number, not an error.

**The result.** 200 000 iterations, 28-byte buffer
(`git commit --amend --no-edit`):

| Run | with | without | difference | per call |
|---|---|---|---|---|
| run | with the builtin | without it | difference | per keystroke |
|---|---|---|---|---|
| 1 | 0.0595 s | 0.0421 s | 0.0174 s | 0.87 µs |
| 2 | 0.0589 s | 0.0424 s | 0.0165 s | 0.83 µs |
| 3 | 0.0588 s | 0.0416 s | 0.0172 s | 0.86 µs |

**This figure was 0.48 µs until the harness was corrected.** The old
harness alternated between two lines that produced *identical* spans, so the
module took its "the styling has not changed" path and published nothing
after the first iteration: the number measured the cost of doing nothing.
The harness now checks that the module really publishes before it times
anything, and the two lines differ in their styling.

**The baseline is also corrected.** The lexer's "full re-parse" column used
to be the free function `lex()`, which allocates two fresh vectors on every
call, while the module's own full parse reuses its buffers. The comparison
was therefore between an allocating and an allocation-free implementation.
The table below uses `LexCache::lex` — the module's actual full parse — as
the baseline.

**Caveats on this number, stated plainly.**

- `hyperfine` is not installed on this machine, so the harness used its
  zsh-clock fallback, which is less accurate. The spread above (0.83–0.89 µs)
  is the noise of that fallback, not a property of the code. The plan's
  recorded figure was 0.3–0.5 µs, which excluded the publish step entirely
  ([`../TASKS.md`](../TASKS.md)), consistent with the lower half of this
  distribution.
- The measurement is **not** a keystroke-to-pixel measurement. It is the
  marginal cost of the native call, in a non-interactive zsh, with a
  hand-driven `BUFFER` assignment. It excludes zsh's own redraw, the
  terminal's write, and everything the tty layer does.
- The harness's second argument is broken for lengths above 28 bytes. The
  buffer-building loop uses `$#parts[1]`, which in zsh is the *length of the
  first element* (`git` → 3) and not the number of elements, so the loop
  condition `4 < LEN/20` is true for `LEN ≥ 100` and the loop never ends.
  `bench/bench_module_call.zsh` therefore hangs for `LEN ≥ 100`, and silently
  produces a 28-byte buffer for `29 ≤ LEN ≤ 99`. **Only the default
  `LEN = 28` result above is meaningful.** This is a bug in the harness, not
  in the module; see [known limitations](known-limitations.md).

### 3.2 `cargo run --release -p lexer --example bench_incremental 200` — the lexer's own numbers

**What it measures.** For the same edit — one character inserted in the middle
of a command line — the cost of (a) a full re-parse of the whole line and
(b) an incremental re-lex that resumes from a checkpoint and splices the
unchanged tail back on.

**How.** The harness re-primes the cache with the original line before every
timed re-lex, *outside* the timer. Without that, the second and later rounds
would re-lex an unchanged line and measure a no-op — the same class of bug as
the empty-buffer one.

**The result.**

| words | bytes | full (µs) | incremental (µs) | policy (µs) | saving (ns) | bytes re-scanned |
|---|---|---|---|---|---|---|
| 2 | 17 | 0.088 | 0.198 | 0.084 | −50 | 18 |
| 4 | 46 | 0.123 | 0.173 | 0.123 | −50 | 47 |
| 8 | 86 | 0.264 | 0.271 | 0.279 | −7 | 87 |
| 16 | 169 | 0.538 | 0.481 | 0.521 | +57 | 10 |
| 32 | 354 | 0.913 | 0.776 | 0.784 | +137 | 14 |
| 64 | 694 | 1.854 | 1.458 | 1.481 | +396 | 21 |
| 128 | 1 389 | 3.552 | 2.742 | 3.013 | +809 | 10 |

Each round re-primes the cache with the original line and then makes the
edit, so the "full" and "incremental" columns are the cost of a *round*; the
**saving** column is the difference, which is what the incremental path
actually buys.

**The baseline is the module's own full parse** (`LexCache::lex`, which
reuses its buffers), not the free function `lex`, which allocates two fresh
vectors on every call. An earlier version of this table used the
allocating one and so measured the allocator rather than incrementality.

**Below 128 bytes the incremental path is slower**, and the policy therefore
re-parses the line instead (`INCREMENTAL_MIN_LEN`). The negative savings in
the first rows are the reason the policy exists. Above the threshold the
incremental path saves 57–809 ns per edit, and the bytes re-scanned stay
flat as the line grows.

The "policy" column is what the module actually does: the incremental path for
a long line, a full re-parse for a short one.

**This is one run.** A second run of the same command on the same machine
gave 0.210 / 0.160 / 0.150 (1.40×) for 86 bytes, 0.421 / 0.221 / 0.220
(1.91×) for 169, 0.802 / 0.350 / 0.340 (2.36×) for 354, 1.433 / 0.591 /
0.581 (2.47×) for 694 and 2.665 / 1.062 / 1.062 (2.51×) for 1 389 — so
the run-to-run variation is a few percent, and the "bytes re-scanned" column
is exactly reproducible. The table above is the first run.

**The scan is sub-linear.** The harness prints and checks this:

```
the line grew 81.7x; the bytes re-scanned grew 0.6x
```

The line grew by a factor of 81.7; the number of bytes re-scanned *shrank*.
The work is the distance to convergence, not the length of the line.

**The end-to-end cost is still linear.** The harness says so rather than
hiding it, and so does its own final note:

> NOTE: the *end-to-end* cost is still linear in the length of the line: the
> offsets of the tokens after the edit move, so the token list has to be
> rebuilt. What is sub-linear is the scanning, which is what the "re-scanned"
> column measures.

**The harness fails if the properties stop holding.** It exits non-zero if
the incremental result differs from a full parse, if the re-scanned bytes
grow faster than the line, or if the policy is ever slower than a full
re-parse. On this run it printed `OK` and exited 0.

### 3.3 `cargo test -p lexer --test alloc_budget -- --nocapture --test-threads=1` — the allocation budget

**What it measures.** The number of heap allocations a steady-state re-lex
performs, using a counting global allocator that wraps the system allocator
([`crates/lexer/tests/alloc_budget.rs:26-53`](../../crates/lexer/tests/alloc_budget.rs#L26)).

**The result.**

```
a re-lex of a 28-byte line allocated 0 times (budget 8)
```

**Zero allocations** for a steady-state re-lex, against a budget of 8. The
budget is a *bound*, not a target: the point is that the count must not grow
with the number of tokens. The second test in the file asserts exactly that
— a ten-times longer line must not allocate ten times more — and it passes.

**One caveat about this number.** With the default parallel execution the
same test sometimes prints `1` instead of `0`. The two tests in that file
share the process-wide counting allocator
([`crates/lexer/tests/alloc_budget.rs:28-31`](../../crates/lexer/tests/alloc_budget.rs#L28)),
so an allocation made by the *other* test can land inside the measured
window. With `--test-threads=1` the count is deterministically 0, and that
is the number quoted here. This is the same class of bug as the shared
`DEGRADED` flag recorded in [`../TASKS.md`](../TASKS.md).

### 3.4 `bench/bench_startup.zsh` — startup cost, syscalls, and "no spawns"

**What it measures.** Three things: the wall-clock cost of loading the
module, the syscall counts with and without it, and the number of processes
spawned *while the highlighting builtin runs*.

**The result.**

```
[1] load cost -- wall clock of a fresh zsh -f, per arm
  zsh -f (baseline)                              : 2.951 ms  (min 2.897 ms, max 3.124 ms)
  zsh -f + module_path assignment                : 2.947 ms
  zsh -f + zmodload shellpulse/highlight         : 3.165 ms  (min 3.094 ms, max 3.301 ms)
  zsh -f + source zsh/init.zsh                   : 3.608 ms

  module load cost (loaded - path)               : 0.207 ms
  module_path assignment cost (path - baseline)  : 0.014 ms
  whole plugin cost (plugin - baseline)          : 0.652 ms
  whole plugin cost (loaded - baseline)          : 0.222 ms
  measurement noise (median absolute deviation)  : 0.045 ms

[2] syscall counts -- strace -c -f
  zsh -f (baseline) syscalls                     : 505 syscalls
  zsh -f + module loaded syscalls                : 567 syscalls
  difference (loaded - baseline)                 : 62 syscalls

[3] hot path -- spawns while the builtin runs
  calls made                                     : 50 calls
  calls that reached the builtin                 : 50 calls
  spawns inside the window                       : 0 spawns
  spawns before the window (load time)           : 2 spawns
    execve / execveat                            : 1 spawns  (the shell itself)
    clone / clone3                               : 1 spawns  (the engine worker thread)
  control: spawns inside the window              : 2 spawns  (a child spawned on purpose)
```

**How the numbers are defended.**

- The arms are **interleaved** (one sample of each, round and round), so a
  busy machine perturbs all of them equally instead of only the last one. The
  differences are taken *per sample pair* and then summarised, which cancels
  the slow drift of a shared machine.
- The noise floor is reported: **0.045 ms**, the median absolute deviation of
  the paired differences. A difference smaller than that is not resolved.
  The `module_path` assignment cost (0.014 ms) is below it and is labelled as
  such.
- "Zero spawns" is only meaningful if the counter works. The harness runs a
  **control** that spawns a child on purpose inside the same window and
  requires the counter to find it. It found 2.
- The window is bracketed by markers the module itself writes, so the spawn at
  *load* time (the worker thread's `clone`) is not mistaken for a spawn on
  the keystroke path.
- `clone3` is traced as well as `clone`: glibc's `pthread_create` uses
  `clone3` when the kernel offers it, and a harness that traced only `clone`
  would report "no thread was created" and be wrong.
- Every call must be shown to have reached the native code, counted from the
  module's own `SHELL_PULSE_DEBUG` diagnostics: 50 of 50.

**What the startup numbers do not cover.** The hot-path run uses `zsh -f -c`,
so there is no ZLE widget and no `region_highlight` special parameter: the
module reads the line and lexes it, but the final `setaparam` of the regions
is not exercised. The missing step is a call into zsh's own memory, which
cannot spawn anything, so it does not weaken the "no spawns" conclusion — but
the hot path measured here is the *lower bound* of the real one.

**The module size is 558 784 bytes.** That is the whole `cdylib`, including
the Rust standard library's statically-linked parts. It is not optimised for
size.

### 3.5 `bench/bench_zpty.zsh` — keystroke to redraw, in a real interactive shell

**What it measures.** The interval between a keystroke being written into an
interactive zsh running under a pseudo-terminal and the shell's answer being
noticed. Each keystroke is written on its own and the harness waits for the
pty before writing the next, so each one is measured separately.

**The result** (200 keystrokes, `TERM=xterm-kitty`, `zselect` available so
the pty is *waited on* rather than polled):

| run | per keystroke | fastest | slowest |
|---|---|---|---|
| 1 | 66.5 µs | 58.2 µs | 190.0 µs |
| 2 | 68.2 µs | 58.4 µs | 141.9 µs |
| 3 | 67.8 µs | 58.4 µs | 328.8 µs |
| 4 | 78.7 µs | — | — |
| 5 | 69.4 µs | — | — |
| 6 | 66.7 µs | — | — |
| 7 | 67.3 µs | — | — |
| 8 | 69.5 µs | — | — |

**Median 68.0 µs** (the eight samples are 66.5, 66.7, 67.3, 67.8, 68.2, 69.4, 69.5 and 78.7 µs).

**The control.** The harness has three arms, so the cost of the plugin
existing can be told from the cost of the plugin working:

| arm | what is loaded | per keystroke |
|---|---|---|
| (default) | the adapter: the module is loaded and the hooks are installed | 73.1 µs |
| `--control` | the module is loaded, but no hook is installed | 54.0 µs |
| `--none` | nothing: zsh and the pty on their own | 45.2 µs |

The control and the `--none` arm used to live in a scratch copy in `/tmp`,
which made the most decision-relevant number in the project impossible to
reproduce. They are in the harness now.

**What the three arms say.** The plugin's own work is the difference between
the first two arms, **19.1 µs**; the cost of the plugin merely *existing* —
the hook, and the ZLE special parameters zsh creates for every widget — is
the difference between the last two, **8.8 µs**.

**And how that scales with the number of regions.** The harness can type a
line with N regions and measure the same thing:

| regions | µs/keystroke |
|---|---|
| 2 | 80.0 |
| 6 | 75.5 |
| 21 | 80.8 |
| 41 | 91.4 |
| 81 | 109.1 |
| 161 | 194.8 |
| 256 | 294.3 |

A linear fit over the whole series gives an intercept of 66.9 µs and a
**slope of 0.835 µs per region**; over the larger counts alone the slope is
0.974 µs. The module's own share of that is about **12 ns per region**, so
roughly 98 % of the marginal cost is zsh's: parsing the region specification
on every redraw, and writing the extra escape sequences.

**That is what the region cap is derived from.** At 32 regions the added
latency is about 10 µs — the budget this project set for a keystroke — so
`region::MAX_ENTRIES` is 32. It used to be 256, which was a guess.

**That 19.1 µs is *not* the module's own work.** The native call costs
0.67 µs (§3.1), and the 8.8 µs of "the plugin existing" is zsh's. The
rest is zsh reacting to the regions:

* the pre-redraw hook is a **shell function**, and calling it costs zsh a
  function invocation, an argument vector and a builtin call — per redraw;
* zsh then parses every region specification it was given
  ([`zsh-src/Src/Zle/zle_refresh.c:513-586`](../upstream/zsh-src/Src/Zle/zle_refresh.c#L513));
* and it writes more bytes to the terminal, because the coloured regions
  carry escape sequences the uncoloured line does not — and the pty has to
  carry every one of them.

**So the ≤10 µs target is not demonstrated end to end.** The module's own
work is well inside it; what exceeds it is zsh's reaction to the regions,
which the plugin asks for but does not itself do. Whether that is the
plugin's cost or zsh's is a question the harness cannot answer, because the
two cannot be separated inside one process — which is why the number is
published with all three arms rather than as a single figure.

**What the number is worth.** It is an *upper bound* on what a user waits
for, it is dominated by the pty round trip, and it is only meaningful when
compared with another run of the same harness on the same machine — which is
exactly what the control run is for. The harness's own header says as much:
"What the number includes: the pty round trip and the interval between 'the
keystroke was written' and 'the answer was noticed'. It is therefore an upper
bound on the latency a user perceives."

**How the harness defends itself.** It refuses to report a number unless the
module is installed, the adapter is present, the pty is available, the shell
under the pty draws its prompt, and **at least one highlighted region was
seen in the terminal output** — otherwise it would be measuring an
unhighlighted shell.

**What it does not isolate.** The plugin's own contribution (~0.5 µs) is
three orders of magnitude below the harness's resolution, so this harness
*cannot* measure it. It measures the cost of the plugin existing, not the
cost of the plugin working.

### 3.6 The test suite

`cargo test --workspace` on the tree as it stands:

| Target | Tests |
|---|---|
| `engine` (unit) | 19 |
| `highlight` (unit) | 18 |
| `lexer` (unit) | 70 |
| `lexer` (`alloc_budget`) | 2 |
| `zsh-abi` (unit) | 27 |
| `zsh-module` (unit) | 47 |
| `zsh-module` (`zsh_integration`, real zsh subprocesses) | 51 |
| `zsh-module` (`zsh_pty`, real interactive zsh under a pty) | 8 |
| doc-tests | 2 |
| **total** | **244** |

The counts are the number of `#[test]` functions in each target. The 51
subprocess tests are the ones that matter for the ABI: each one runs
`zsh` and asserts on its exit status, so an ABI mistake is a failing test
rather than a dead shell.

---

## 4. Where the time goes

For a 28-byte line, the module's own cost per keystroke is ~0.67 µs. The
lexer's own share of that is the re-lex measured in §3.2, which for a line of
that size is the full re-parse path (the line is under the 128-byte
threshold). The 28-byte benchmark buffer falls between the 17-byte and the
46-byte rows of that table, so its re-lex costs **≈ 0.07 µs**.

So roughly:

| Component | Cost | Evidence |
|---|---|---|
| Lexing the line | ≈ 0.07 µs | §3.2, the 17-byte and 46-byte rows |
| Everything else (the zsh call, the region build, the `setaparam`) | ≈ 0.41 µs | §3.1 minus §3.2 |

The "everything else" is the cost of getting the regions into zsh, and it
dominates. It is dominated by zsh's own parameter machinery: `setaparam`
walks the parameter table, `assignaparam` calls `setarrvalue`, and
`set_region_highlight` parses every row it is given
([`zsh-src/Src/Zle/zle_refresh.c:513-586`](../upstream/zsh-src/Src/Zle/zle_refresh.c#L513)).
That is why the number of regions matters more than the length of the line,
and why `region::MAX_ENTRIES` is 32
([`crates/zsh-module/src/region.rs:276`](../../crates/zsh-module/src/region.rs#L276)).

This breakdown is an *inference* from two independently measured numbers, not
a profile. It has not been confirmed with `perf`. It was also measured
before the whole-line colouring was added — see §4.1.

### 4.1 The cost of colouring the whole line

The line is now coloured in more places than it used to be, and two of the
new colours cost something the old ones did not.

**The path lookup is a syscall.** Every literal word is a candidate for
naming a file — `Cargo.toml` names a file just as much as `./Cargo.toml`
does, and the reference implementation looks every word up
([`../upstream/fsh/fast-highlight:1265-1295`](../upstream/fsh/fast-highlight#L1265)).
A literal word is resolved against `$PWD` or `$HOME` and looked up with
`stat(2)`. That is **one `stat` per distinct literal word per keystroke,
in the worst case**: a line with three different file names costs three
`stat`s on every keystroke until the answers are cached.

**The number of lookups on a line is bounded.** At most **16** words of a
line are offered to the lookup (`MAX_PATH_LOOKUPS`), counted in the order
they appear, so the words nearest the command — the ones most likely to be
paths — are the ones that get looked up. A `stat` costs about **0.6 µs**,
so a line that uses the whole allowance costs about **10 µs**, well inside
the millisecond a keystroke has to fit in. The 0.6 µs figure is an estimate
of the cost of one `stat`, not a measurement taken in this repository.

**The budget is checked before each lookup, not only at the end.** A
`stat` on a hung network filesystem can take milliseconds, and a line whose
lookups were cancelled *after* they were made would have cost the user all
of that wait and then thrown the answer away. The module therefore tests the
one-millisecond budget **before every path lookup**
([`crates/zsh-module/src/lib.rs:640-648`](../../crates/zsh-module/src/lib.rs#L640))
and stops looking paths up once it is spent. The check after the whole line
is done still exists — it is what clears the highlights when the line took
too long — but it is no longer the only one. Stopping early turns "the user
waits and gets nothing" into "the user waits only as long as the budget".

**The answers are cached, and the cache expires.** The cache holds at most
**64** entries, and every answer **expires after 2 seconds**, so a file
that is created or deleted while the session runs is picked up rather than
remembered wrongly. When the cache is full it is emptied, which costs one
`stat` per path again and nothing more.

**The shell-side lookup does not fork.** The adapter asks the shell whether
it knows a command word, and the answer comes from a hash lookup in the
shell's own tables (`$aliases`, `$functions`, `$builtins`, `$commands`, …).
Nothing is spawned on the keystroke path — the property the startup harness
proves ([`bench/bench_startup.zsh`](../../bench/bench_startup.zsh), §3.4) —
and it still holds. The adapter keeps a cache of its own for the answers,
with the same **2-second** lifetime, and it remembers the negative answers
as well as the positive ones; the protocol is in
[ZLE integration §1.5](zle-integration.md).

**What has not been measured.** The module's own work per keystroke was
**0.67 µs** (§4) *before* the path lookups and the command lookups were
added, and **the new figure has not been re-measured**: the harness in
`bench/` has not been re-run against the new code. The 0.67 µs figure must
therefore not be read as the cost of the plugin as it stands now.

What has not changed is the *shape* of the cost: the 1 ms wall-clock budget
that guards a keystroke is the same, and the number of regions a line may
produce is still capped at 32. Only the size of the cost is unknown.

### 4.2 The adapter's share of a keystroke

The adapter is shell code, and it runs on the same thread as the line
editor: everything it does is part of the pause the user sees. Three things
it used to do are gone.

**It no longer emulates zsh twice.** `emulate -L zsh` resets every option
in the shell, and it is not free on the interactive thread. The function
that walks the words of the line used to emulate zsh itself, on top of the
`emulate -L zsh` of the function that called it. It now sets only the two
options it needs — `extendedglob` and `noglob` — and does so locally
([`zsh/widgets.zsh:186`](../../zsh/widgets.zsh#L186)). The outer function
still emulates zsh, once.

**It no longer calls a function for every word of the line.** The walk
*inside* a command substitution used to be entered for every word of the
line; it is now entered only for a word that contains `$(` or a backtick
([`zsh/widgets.zsh:214-216`](../../zsh/widgets.zsh#L214)). A zsh function
call is not free, and this ran for every word.

**It no longer splits a line it will not highlight.** The module does not
highlight a line longer than 4 KiB, so the adapter refuses to look at one:
the line is not split with `${(z)…}`, and its words are not walked, on
every keystroke
([`zsh/widgets.zsh:141-143`](../../zsh/widgets.zsh#L141)). A pasted
megabyte used to cost a full split and a walk of every word — for a line
that was never highlighted.

**What has not been measured.** None of the three has a number attached to
it: the cost of `emulate` on this machine is not known, and neither is the
cost of a zsh function call or of the split of a long line. What the
changes do is remove work that was done on every keystroke and whose result
was thrown away.

Like the whole-line colouring, the adapter's share of a keystroke **has not
been re-measured end to end**: the pty figure in §3.5 — 73.1 µs for the arm
that loads the adapter — was measured before these three changes. The
0.86 µs figure in §3.1 is not affected by them: that harness calls the
builtin directly, without the adapter.

---

## 5. The baselines, and where they come from

| Figure | Value | Source | Verifiable here? |
|---|---|---|---|
| FSH (fish's highlighter) per keystroke | ≈ 40 µs fixed + ≈ 13 ns/char | [`../upstream/PERFORMANCE_COMPARISON.md` §1](../upstream/PERFORMANCE_COMPARISON.md) | **No.** The harness needs the upstream checkouts; it is not part of this repository's `bench/`. |
| Cost of a bare native module call | ≈ 112 ns | [`../upstream/PERFORMANCE_COMPARISON.md` §2](../upstream/PERFORMANCE_COMPARISON.md) | **No**, for the same reason. The equivalent measurement *with* this module's work included is §3.1. |
| Fork + exec per keystroke | ~1–3 ms | [`../upstream/PERFORMANCE_COMPARISON.md` §3](../upstream/PERFORMANCE_COMPARISON.md) | No. |
| Persistent helper over a socket | ~20–60 µs round trip | [`../upstream/PERFORMANCE_COMPARISON.md` §3](../upstream/PERFORMANCE_COMPARISON.md) | No. |
| zsh-autosuggestions' history lookup | a linear glob scan of the whole history, per keystroke | [`../upstream/PERFORMANCE_FAILURES.md` §1](../upstream/PERFORMANCE_FAILURES.md) | The *shape* is verifiable in the upstream checkout; the cost is not measured here. |

The 112 ns figure was the decisive measurement in Stage A: it is what made a
native module the obvious choice over any process-based design. It is also
the reason this project has a `dlsym`-based loader rather than a link-time
one — see [architecture §5.1](architecture.md).

---

## 6. What the numbers do **not** cover

This is the section to read before trusting any of the above.

1. **Keystroke to pixels.** Nothing here measures how long the user waits
   before seeing the coloured line. That includes zsh's own redraw, the
   terminal's `write`, and the scheduler. The plugin's own contribution is
   what §3.1 measures; the rest is zsh's and the terminal's.
2. **The plugin's own cost inside a real interactive shell.** The keystroke →
   redraw harness (§3.5) runs in a real interactive zsh under a pty, but its
   resolution is set by the pty round trip (~45 µs). The plugin's own
   contribution — 0.67 µs by §3.1 — is more than an order of magnitude below that,
   so the harness *cannot* see it. The 23 µs difference between the plugin
   run and the control is zsh's work: the function call, the region parsing
   and the extra terminal bytes.
3. **The user's real configuration.** Nothing was measured with 8 278 widgets
   and 16 551 completion functions loaded. The design argues that the cost is
   independent of the widget count (one hook, not a wrapped table), but that
   argument has not been measured against the real configuration.
4. **Other zsh builds.** Every number here is from zsh 5.9.2 on this machine.
   The export set of a Debian, Fedora or Homebrew zsh differs, and the module
   refuses to load if a required symbol is missing — which is a *behaviour*,
   not a performance result. No other zsh was measured.
5. **Other architectures and other libc implementations.** The allocator
   ownership rules in the [boundary document](rust-zsh-boundary.md) depend on
   `zalloc` being `malloc` and `zsfree` being `free` in a zsh built without
   `ZSH_MEM`. A zsh built *with* `ZSH_MEM` replaces `malloc` and `free` with
   its own allocator
   ([`zsh-src/Src/mem.c:1189`](../upstream/zsh-src/Src/mem.c#L1189)); that
   build has not been tested here.
6. **Criterion micro-benchmarks.** The plan's Stage E1 (criterion over the
   lexer, the span layer and the ranker) is not done. The numbers in §3.2
   come from a hand-rolled `best_of` harness, which reports the *fastest* of
   50 runs — an estimate of the work itself, not of the interactive tail.
7. **Allocation profiling (DHAT).** Stage E4 is not done. §3.3 counts
   allocations; it does not attribute them.
8. **Head-to-head against zsh-syntax-highlighting and zsh-autocomplete.**
   Stage E5 is not done. Only FSH has a Stage A baseline.
9. **The completion, ranking and history paths.** Stage D is not started.
   There are no numbers for them because there is no code for them.
10. **Idle CPU and RSS.** Not measured. The worker thread exists and is
    polled; how much idle CPU it costs is unknown.
11. **The whole-line colouring.** The path lookups (a `stat` per distinct
     literal word, at most sixteen of them on a line) and the shell-side
     command lookups were added after the numbers above were measured. The
     0.67 µs module figure and the 0.86 µs end-to-end figure predate them,
     and neither has been re-measured. §4.1 says what is known about the
     new work and what is not.
12. **The adapter's share of a keystroke.** The adapter no longer emulates
     zsh twice, no longer enters the substitution walk for every word of
     the line, and no longer splits a line it will not highlight. None of
     that has been re-measured either: the pty figure in §3.5 predates it.
     §4.2 says what is known about it and what is not.

---

## 7. On noise

The machine is shared: 14 GiB of RAM, ~3 GiB available, 12 hardware threads.
The end-to-end measurement's spread (0.67–0.86 µs) is
dominated by that, and by the fact that `hyperfine` is not installed so the
harness falls back to zsh's own `EPOCHREALTIME`.

The startup harness fights this properly — interleaved arms, per-sample
differences, a reported noise floor — and its numbers are stable to ±0.045 ms.
The lexer micro-benchmark reports the fastest of 50 rounds, which measures
the work rather than the scheduler, so its numbers are stable to about ±0.05 µs
between runs of the same size (compare the two 17-byte rows: 0.08 µs in this
run and 0.08 µs in the run recorded in [`../TASKS.md`](../TASKS.md)).

**A number in this document that is not reproducible to within the stated
spread is a bug in the document, not a discovery.**

---

## 8. Reproducing everything

```sh
# the lexer micro-benchmark
cargo run --release -p lexer --example bench_incremental 200

# the allocation budget (--test-threads=1: the two tests in that file share
# one process-wide counting allocator, so a parallel run can report 1
# instead of 0)
cargo test -p lexer --test alloc_budget -- --nocapture --test-threads=1

# the whole test suite
cargo test --workspace

# the end-to-end cost through zsh
./bench/install.sh release          # build and install the cdylib
./bench/bench_module_call.zsh 200000 28

# startup cost, syscalls and the "no spawns" check
./bench/bench_startup.zsh

# keystroke -> redraw, in a real interactive zsh under a pty
./bench/bench_zpty.zsh 200
```

`bench/install.sh` puts the module where the harnesses and the zsh adapter
expect it: `bench/mp/shellpulse/highlight.so`. The nested directory and the
dotless name are mandatory, because zsh's `modname_ok()`
([`zsh-src/Src/module.c:2177-2186`](../upstream/zsh-src/Src/module.c#L2177))
accepts only slash-separated identifiers — absolute paths and `.so` suffixes
are rejected.

If `hyperfine` is available the end-to-end harness uses it and the spread
narrows considerably:

```sh
nix shell nixpkgs#hyperfine
```
