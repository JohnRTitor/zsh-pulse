# Known limitations — `shell-pulse`

An honest list of what this plugin does **not** do, what is measured but not
optimised, and what is untested. Every entry says what the limitation is, how
you can tell, and what would have to change to fix it.

Companion documents: [architecture](architecture.md) ·
[performance](performance.md) ·
[ZLE integration](zle-integration.md) ·
[Rust↔Zsh boundary](rust-zsh-boundary.md)

---

## 1. Correctness and performance limitations that are true today

### 1.1 The end-to-end cost of a keystroke is linear in the length of the line

**What.** The *scanning* is sub-linear — the number of bytes re-scanned does
not grow with the line — but the *whole* re-lex does, because the token
offsets after the edit move and the token list therefore has to be rebuilt
with absolute offsets.

**How you can tell.** The benchmark harness says so in its own output:

```
NOTE: the *end-to-end* cost is still linear in the length of the line: the
offsets of the tokens after the edit move, so the token list has to be
rebuilt. What is sub-linear is the scanning, which is what the "re-scanned"
column measures.
```

and the measured table shows it: the incremental path saves 57 ns on a
169-byte line and 809 ns on a 1 389-byte line
([performance report §3.2](performance.md)), while the bytes re-scanned stay
at 10–47.

**What it would take.** The token and span representation would have to store
the tail's offsets *relative to a stable anchor* instead of absolutely, and
the anchor would be added when the regions are published. The publishing step
is already O(regions), not O(line), so the API is not the obstacle — the
representation is. This has not been done, and the current absolute-offset
representation is what makes the incremental splice simple enough to be
provably correct (the property tests compare the incremental result with a
full parse, byte for byte).

**How much it matters.** At the 1 389-byte end of the measured range the
whole end-to-end keystroke is still ~0.5 µs, i.e. 5 % of the 10 µs budget.
The linearity only becomes a problem at line lengths nobody types.

### 1.2 The ZLE path is only covered by an automated test when the `zpty` harness can run

**What.** The integration tests run `zsh -f -c`, which is not interactive. A
non-interactive zsh has no ZLE widget and no `region_highlight` special
parameter, so those tests stand in a **global** `region_highlight` array for
it. The real special parameter — its per-widget-call lifetime, its `PM_LOCAL`
scope, its interaction with zsh's own redraw — is only exercised by the
pseudo-terminal harness.

**The state of that harness.** It exists:
[`crates/zsh-module/tests/zsh_pty.rs`](../../crates/zsh-module/tests/zsh_pty.rs).
It drives a real interactive zsh under a pty, types a line into the line
editor and asserts on the **bytes zsh writes to the terminal** — `ESC[1m
ESC[36m` for `fg=cyan,bold`, `ESC[0m` to close the region. On the machine
these documents were written on, all three of its tests ran and passed.

**But it is conditional, and the conditions are real:**

* it needs the optional `zsh/zpty` module;
* it needs a pseudo-terminal the test can create;
* it needs a terminal type whose colour capabilities are the standard xterm
  ones — it checks this with zsh itself before asserting, and skips with a
  printed reason if it cannot find one;
* when any of these is missing the test **passes with a printed skip reason**.
  That is the right behaviour for a test dependency, but it means a green
  test run does not by itself prove the ZLE path is covered.

**And what it does not cover:** the harness proves the highlighting *reaches
the screen*; it does not measure *latency*.

**The latency measurement now exists too**, in
[`bench/bench_zpty.zsh`](../../bench/bench_zpty.zsh): it types one character
at a time into an interactive zsh under a pty and measures the interval
between the keystroke going in and the shell's answer being noticed. Its
median is **68.0 µs per keystroke**; the same harness with the plugin *not*
loaded measures **45.1 µs**
([performance report §3.5](performance.md)).

**But the latency harness cannot see the plugin's own cost.** Its resolution
is set by the pty round trip, which is ~45 µs — three orders of magnitude
above the 0.86 µs the native call actually costs. The 23 µs difference
between the two runs is zsh's work: the shell-function call, the parsing of
the region specifications, and the extra terminal bytes. So the pty harness
measures **the cost of the plugin existing**, not **the cost of the plugin
working**.

**What it would take** to see the plugin's own cost in an interactive shell:
a harness that subtracts a *matched* control — the same shell, the same
keystrokes, the same region set, but with the highlighting replaced by a
fixed byte sequence — so that the terminal output is identical in size and
only the computation differs.

### 1.3 `region_highlight` is replaced wholesale, so clearing it also clears another plugin's entries

**What.** `region_highlight` is a single array, and zsh's setter replaces the
whole user-settable part of it
([`zsh-src/Src/Zle/zle_refresh.c:488-586`](../upstream/zsh-src/Src/Zle/zle_refresh.c#L488)).
Only zsh's four *special* regions — the region between point and mark, the
isearch region, the suffix and the pasted text — survive
([`zsh-src/Src/Zle/zle.h:473`](../upstream/zsh-src/Src/Zle/zle.h#L473)).

So if another plugin also writes `region_highlight`, **the last writer wins**
and the other's regions disappear. Worse, the plugin's own *clear* path —
taken when the line cannot be lexed, when the line is over 4 KiB, or when the
1 ms budget is exceeded — writes an empty array and therefore removes another
plugin's highlighting as well.

**How you can tell.** Our own entries carry `memo=shell-pulse`
([`crates/zsh-module/src/region.rs:34`](../../crates/zsh-module/src/region.rs#L34)),
which is exactly the mechanism zsh's documentation suggests for telling
plugins' entries apart
([`zsh-src/Doc/Zsh/zle.yo:986-998`](../upstream/zsh-src/Doc/Zsh/zle.yo#L986)).
**The merge is not implemented.** The `memo` tag is there so that it *can* be.

**What it would take.** Reading the array back, filtering out the entries
whose `memo` is `shell-pulse`, and writing back the union. zsh's own
suggestion for this is to do the filtering in zsh
(`region_highlight=(${region_highlight:#*memo=…})`), which would mean moving
the merge into the adapter; doing it in Rust means reading the array back
through the parameter getter, whose return value is a `sepjoin`ed string
rather than an array, so the rows would have to be re-split.

**The alternative** — and the one this project takes — is to make the plugin's
regions the only ones it is responsible for and to document that it owns the
array while it is running.

### 1.4 The incremental lexer is not used below 128 bytes, because it is slower there

**What.** `LexCache::relex` re-parses a line shorter than
`INCREMENTAL_MIN_LEN` (128) outright
([`crates/lexer/src/incremental.rs:53`](../../crates/lexer/src/incremental.rs#L53),
[`crates/lexer/src/incremental.rs:166-171`](../../crates/lexer/src/incremental.rs#L166)).
The incremental path is measurably *slower* below that length, because the
diff and the merge cost more than the scan they save.

The threshold was re-measured when the benchmark's baseline was corrected to
the module's *own* full parse (the free `lex()` allocates two fresh vectors
on every call, so comparing against it measured the allocator rather than
incrementality). Against the right baseline the crossover is between 86 and
169 bytes.

**Why it is still fine.** The crossover is a measured constant, the policy is
one `if`, and the raw machinery (`relex_incremental`) stays reachable so that
the property tests keep exercising it
([`crates/lexer/src/incremental.rs:453-471`](../../crates/lexer/src/incremental.rs#L452)).

**What it costs.** The *shipped* path for a short line never runs the splice.
A bug in the splice that only manifests for short lines would therefore not
be caught by an end-to-end test — only by the property tests, which call the
raw path directly.


### 1.9 The ≤10 µs target is not demonstrated end to end

**What.** The module's own work per keystroke is **0.67 µs**
([performance report §1](performance.md)), comfortably inside the target. The
`zpty` harness, which drives a real interactive zsh, has three arms:

| arm | per keystroke |
|---|---|
| the plugin | 73.1 µs |
| the module loaded, no hook | 54.0 µs |
| nothing | 45.2 µs |

So the plugin adds **19.1 µs** in that harness, of which 8.8 µs is the
plugin merely existing (the hook, and the ZLE special parameters zsh
creates per widget) and the rest is zsh parsing the region specifications
and writing the extra escape sequences.

**Why it is not demonstrated.** The module's work and zsh's reaction to it
cannot be separated inside one process: the regions have to be published for
zsh to react to them. The pty also carries every escape sequence, which a
real terminal would absorb.

**What it would take.** A measurement of zsh's `set_region_highlight` cost
per region, and of the redraw cost with and without regions, so that the
module's share could be stated rather than inferred.

### 1.5 The per-range diff is implemented but not used on the hot path

**What.** The plan's C2 says "only changed ranges reach zsh". The
implementation does not do that. `highlight::diff`
([`crates/highlight/src/lib.rs:203-241`](../../crates/highlight/src/lib.rs#L203))
exists, is tested against a deliberately naive reference implementation
([`crates/highlight/src/lib.rs:480-499`](../../crates/highlight/src/lib.rs#L480)),
and **is not called by the module**. The module compares the whole span set
for equality and, if it differs, republishes *every* region
([`crates/zsh-module/src/lib.rs:180-186`](../../crates/zsh-module/src/lib.rs#L180),
[`crates/zsh-module/src/lib.rs:228-230`](../../crates/zsh-module/src/lib.rs#L228)).

**Why it was not done.** `region_highlight` is a single array: there is no
way to publish *some* of the regions and leave the others alone. A partial
update would still mean writing the whole array, and the whole array has to
be re-parsed by zsh on every redraw. So the diff would have to be used to
decide *whether* to publish, not *what* to publish — and the whole-set
equality test already does that, more cheaply.

**What it would take.** Nothing, as long as `region_highlight` keeps its
current shape. The dead code is a wart, not a bug: it should either be
deleted or documented as the specification of "what changed", which is what
it currently is.

### 1.6 The `unchanged` fast path never fires for a short line

**What.** `Highlighter::update` has two short-circuits: "the line did not
change" and "the styling did not change"
([`crates/zsh-module/src/lib.rs:174-186`](../../crates/zsh-module/src/lib.rs#L174)).
The first one is driven by `RelexOutcome::unchanged`, which
`LexCache::relex_short` always reports as `false`
([`crates/lexer/src/incremental.rs:417-426`](../../crates/lexer/src/incremental.rs#L417)),
because a short line is re-parsed rather than diffed.

**Consequence.** For lines under 128 bytes the "line did not change" check is
dead, and only the "styling did not change" check can save the work. The
behaviour is still correct — the second check catches it — but the first
check is doing nothing below the threshold, which is not obvious from the
code.

**What it would take.** `relex_short` could compare the new text with the
cached text and report `unchanged: true` when they are equal. That is a
`memcmp` the function already almost does.

---

## 2. Things that are measured but not optimised

### 2.1 The "everything else" is 80 % of the keystroke

On a 28-byte line the end-to-end cost is ~0.86 µs, of which the lexing is
~0.08 µs. The remaining ~0.40 µs is zsh's own parameter machinery: walking
the parameter table, `setarrvalue`, and `set_region_highlight` parsing every
row it is given.

That is not a defect — it is the cost of the API. But it means **the number
of regions matters more than the length of the line**, which is why
`region::MAX_ENTRIES` is 256 and why the "skip when unchanged" rule is worth
more than any lexer optimisation. See
[performance §4](performance.md).

### 2.2 The benchmark reports the fastest run, not the median

`bench_incremental.rs` reports the best of 50 rounds
([`crates/lexer/examples/bench_incremental.rs:63-74`](../../crates/lexer/examples/bench_incremental.rs#L63)).
That measures the *work*, not the interactive tail. The tail — the
scheduling noise, the page faults, the cache misses — is not in any number
in this repository.

### 2.3 The end-to-end harness uses zsh's clock, not `hyperfine`

`hyperfine` is not installed on the machine these documents were written on,
so `bench/bench_module_call.zsh` used its zsh-clock fallback. The spread of
the samples (0.67–0.86 µs) are that fallback's noise, not
the code's. The startup harness does the interleaved-arm, per-sample-difference
thing properly and reports a 0.045 ms noise floor; the keystroke harness does
not.

### 2.4 The `key()` hash in the lexer state is a pre-filter, not a decision

`LexState::key` is an FNV-1a hash over the state
([`crates/lexer/src/state.rs:299-321`](../../crates/lexer/src/state.rs#L299)).
It is used to skip the exact comparison for most checkpoints. It is a
correctness-neutral optimisation: the exact `==` still decides
([`crates/lexer/src/state.rs:327-329`](../../crates/lexer/src/state.rs#L327)).
But it has never been measured to be *faster* than the plain comparison —
`LexState` is 53 bytes, and comparing 53 bytes is not obviously more
expensive than hashing them.

---

## 3. Things that are untested or unmeasured

### 3.1 Only one zsh, one architecture, one libc

Every number and every subprocess test in this repository is from **zsh 5.9.2
on x86_64 Linux** with the system allocator.

Not tested:

* a zsh built with `ZSH_MEM`, where zsh replaces `malloc` and `free` with
  its own allocator
  ([`zsh-src/Src/mem.c:1189`](../upstream/zsh-src/Src/mem.c#L1189),
  [`:1630-1633`](../upstream/zsh-src/Src/mem.c#L1630)). The `zalloc`/`ztrdup`
  idiom is still correct there — see [boundary §4](rust-zsh-boundary.md) —
  but a module's call to `malloc` may resolve to zsh's override, so the
  "never let zsh free Rust's memory" rule becomes even more important;
* a Debian, Fedora or Homebrew zsh, whose exported symbol set differs. The
  probe exists for exactly this, but no other build has been run;
* aarch64, or any other architecture: the struct layout assertions in
  [`crates/zsh-abi/src/raw.rs:176-253`](../../crates/zsh-abi/src/raw.rs#L176)
  are hard-coded to x86_64;
* a musl libc, or any other allocator.

### 3.2 The signal risk inside the worker thread is only partly retired

zsh installs process-wide signal handlers, and a signal delivered to the
process can land on the worker's thread, where zsh's handler would run on a
stack zsh knows nothing about. This cannot be fixed from inside a module.

Measured: 300 iterations of `kill -WINCH $$` and `kill -CONT $$` while the
worker is alive; zsh survives. **Not** measured: SIGCHLD or SIGINT arriving
while zsh is actually running jobs. The mitigation is structural rather than
empirical — the worker holds no zsh handle, and the engine degrades to
single-threaded if the thread cannot start.

### 3.3 The user's real configuration has never been measured

The compatibility target has 16 551 completion functions and 8 278 widgets.
The design argues that the cost is independent of the widget count — one
hook, not a wrapped table — but **no measurement against that configuration
exists**. The Stage B exit criteria recorded that the module loads under the
real config; nobody has measured the per-keystroke cost there.

### 3.4 Idle CPU and RSS are unknown

The worker thread exists, is created at `boot_` and is polled. How much idle
CPU and how much resident memory it costs in a real interactive shell has not
been measured. The startup harness reports the module's on-disk size
(558 784 bytes) but not its resident size.

### 3.5 There is no paste detection

A paste is not detected and is not treated specially. zsh-syntax-highlighting
detects one and skips highlighting it
([`../upstream/PERFORMANCE_FAILURES.md` §6](../upstream/PERFORMANCE_FAILURES.md));
this plugin does not.

A paste over 4 KiB is caught by the line-length limit, and a slow one by the
1 ms budget. A paste *under* 4 KiB is highlighted in full, once, on the
redraw after the paste — which costs one full parse of the pasted line, not
one per pasted character.

**What it would take.** zsh does not tell a widget that a paste happened, so
the plugin would have to infer it — for example by noticing that `BUFFER`
changed by more than one character between two redraws. That is a heuristic,
and a heuristic that misfires would leave the display stale.

### 3.6 The Stage E benchmark suite is not done

| Item | State |
|---|---|
| E1 criterion micro-benchmarks | **not done** — the lexer numbers come from a hand-rolled harness |
| E2 zpty interactive latency harness | **done** — [`bench/bench_zpty.zsh`](../../bench/bench_zpty.zsh); but its resolution is the pty round trip, not the plugin |
| E3 startup cost + `strace` syscall counts | **done** — [`bench/bench_startup.zsh`](../../bench/bench_startup.zsh) |
| E4 allocation profiling (DHAT) | **not done** — allocations are *counted*, not attributed |
| E5 head-to-head vs FSH, zsh-syntax-highlighting, zsh-autocomplete | **not done** — only FSH has a Stage A baseline |
| E6 compatibility matrix across realistic zsh configurations | **not done** |

### 3.7 The lexer is a highlighter's lexer, not zsh's parser

It recognises the constructs that change how a line should be coloured and
treats everything else as plain word text. It does **not** implement:

* history expansion (`!`);
* `((...))` arithmetic contexts;
* the internals of `${...}`.

None of those change *where* a token starts or ends, so none of them affect
the highlighter — but a line that uses them heavily will be coloured
approximately rather than exactly. See
[`crates/lexer/src/scanner.rs:30-37`](../../crates/lexer/src/scanner.rs#L30).

The lexer also refuses input it cannot represent exactly: nesting deeper
than 8 levels, a here-document delimiter longer than 32 bytes, or a second
here-document opened before the first was consumed
([`crates/lexer/src/state.rs:37`](../../crates/lexer/src/state.rs#L37),
[`crates/lexer/src/state.rs:43`](../../crates/lexer/src/state.rs#L43),
[`crates/lexer/src/scanner.rs:62-64`](../../crates/lexer/src/scanner.rs#L62)).
Those lines are drawn **uncoloured** — the failure is visible rather than
wrong, but it is still a failure.

### 3.8 The colours are hard-coded

The mapping from style to terminal attribute is a fixed table
([`crates/zsh-module/src/region.rs:40-56`](../../crates/zsh-module/src/region.rs#L40)).
A user cannot restyle the plugin's output through `zle_highlight`, through a
highlight group, or through a configuration variable: the plugin always emits
the same literal attribute specifications.

zsh's own machinery would allow either of the other two — `match_highlight`
parses `fg=`, `bg=`, `hl=` and the attribute names
([`zsh-src/Src/prompt.c:2040-2080`](../upstream/zsh-src/Src/prompt.c#L2040)),
and `hl=<group>` resolves through the `.zle.hlgroups` associative parameter
([`zsh-src/Src/prompt.c:285-315`](../upstream/zsh-src/Src/prompt.c#L285)).

---

## 4. Bugs in the harnesses, which the numbers inherit

### 4.1 `bench/bench_module_call.zsh` hangs for a buffer length of 100 or more

The buffer-building loop is:

```zsh
parts=(git commit --amend --no-edit)
while (( $#parts[1] + 1 < LEN / 20 )); do
  parts+=(--flag=value /tmp/path/to/file.txt)
done
```

`$#parts[1]` is the **length of the first element** — `git` → 3 — and not
the number of elements. The condition is therefore `4 < LEN/20`, which is
true for `LEN ≥ 100` and never becomes false, so the script hangs.

Consequences:

* `LEN` between 29 and 99 silently produces a 28-byte buffer;
* `LEN` of 100 or more hangs forever.

**Only the default `LEN = 28` result is meaningful**, and that is the one
quoted in the [performance report](performance.md). The fix is a one-character
change (`${#parts}` for the count) plus a length check, but
`bench/bench_module_call.zsh` is outside the scope of this document.

### 4.2 The allocation test's count is perturbed by its own test binary

`crates/lexer/tests/alloc_budget.rs` installs a *process-wide* counting
allocator
([`crates/lexer/tests/alloc_budget.rs:28-31`](../../crates/lexer/tests/alloc_budget.rs#L28)),
and the file contains two tests. Cargo runs them in parallel, so an
allocation made by the *other* test can land inside the measured window.

The visible effect: with the default parallel execution the test prints
`allocated 1 times` on some runs and `allocated 0 times` on others; with
`--test-threads=1` it is deterministically `0`. The number quoted in the
[performance report §3.3](performance.md) is the serial one.

This is the same class of bug as the shared `DEGRADED` flag recorded in
[`../TASKS.md`](../TASKS.md), and the same fix applies: a test mutex around
the counter, or one test per binary.

### 4.3 The `hyperfine` path was not used

`hyperfine` is not installed here, so the harness took its zsh-clock
fallback. The spread of the measurements is the fallback's noise. This is
documented in the harness itself
([`bench/bench_module_call.zsh:92-107`](../../bench/bench_module_call.zsh#L92))
and in the [performance report §3.1](performance.md).

---

## 5. Design limitations

### 5.1 The colours are a fixed palette

There is no light-background / dark-background detection and no 256-colour
or true-colour support. The palette is eight ANSI colours, chosen to be
readable on both light and dark terminals by eye, and by nothing else. A
terminal with a light background and a poor colour scheme will look worse
than no highlighting.

### 5.2 Ordinary arguments are not highlighted

By design
([`crates/zsh-module/src/region.rs:54`](../../crates/zsh-module/src/region.rs#L54)),
an ordinary argument gets no region at all, so it is drawn exactly as the
user typed it. That is a deliberate readability choice, not a limitation —
but it does mean the plugin highlights *structure*, not *everything*.

### 5.3 The plugin owns the `region_highlight` array while it runs

See §1.3. There is no merge, and no negotiation with other plugins.

### 5.4 The plugin is zsh-only, and Linux/x86_64-only in practice

Bash support was explicitly out of scope for this phase
([`../PLAN.md`](../PLAN.md), "Non-goals"). The module ABI, the ZLE
integration and the special parameters are all zsh-specific.

### 5.5 The `Builtin` table is `static mut` and zsh writes to it

zsh sets `BINF_ADDED` in the flag word and rewrites `node.nam`
([`zsh-src/Src/module.c:555`](../upstream/zsh-src/Src/module.c#L555),
[`zsh-src/Src/hashtable.c:171`](../upstream/zsh-src/Src/hashtable.c#L171)).
The table is therefore mutable shared state, and the module reads it back
through `addr_of_mut!` rather than through a shared reference
([`crates/zsh-module/src/lib.rs:498-505`](../../crates/zsh-module/src/lib.rs#L498)).
It works, but it is the only place where the "zsh keeps the pointer" rule
makes the code awkward.

---

## 6. Features that are planned but not implemented

Stage D of the plan is **not started**. The seams are in place; the features
are not.

| Feature | State |
|---|---|
| Asynchronous completion | not started. The revision guard and the worker thread exist; nothing submits work. |
| History intelligence | not started. The plan's requirement — "no scan on the hot path" — is not at risk, because there is no scan. |
| Fuzzy ranking | not started. |
| Replacing native zsh completion | **out of scope by decision**, not by omission. |
| Bash support | **out of scope by decision**. |
| Tree-sitter for the lexer | rejected: fish does not use it and hand-writes its lexer. Revisit only with data. |
| A daemon | rejected: no measured need. |

---

## 7. A summary table

| # | Limitation | Kind | Severity |
|---|---|---|---|
| 1.1 | The end-to-end keystroke cost is linear in the line length | performance | low at realistic lengths |
| 1.2 | The interactive ZLE path is only covered when the `zpty` harness can run; the pty latency harness cannot resolve the plugin's own cost | testing | **high** |
| 1.3 | `region_highlight` is replaced wholesale; another plugin's entries are lost | correctness | **high** for a user who runs two plugins |
| 1.4 | The incremental path is unused below 128 bytes | design | low, measured |
| 1.5 | The per-range diff is implemented but not used | dead code | low |
| 1.6 | The `unchanged` fast path never fires for a short line | dead code | low |
| 2.1 | zsh's parameter machinery is ~80 % of the keystroke | performance | inherent to the API |
| 2.2 | The benchmark reports the fastest run, not the median | measurement | medium |
| 2.3 | The end-to-end harness uses zsh's clock, not `hyperfine` | measurement | medium |
| 3.1 | One zsh, one architecture, one libc | portability | **high** |
| 3.2 | Job-control signals with a live worker are untested | correctness | medium |
| 3.3 | The user's real configuration has never been measured | performance | medium |
| 3.4 | Idle CPU and RSS are unknown | performance | low |
| 3.5 | There is no paste detection | performance | low |
| 3.6 | Four of the six Stage E benchmark items are not done | process | medium |
| 3.7 | The lexer is approximate for history expansion, arithmetic and `${...}` internals | correctness | low |
| 3.8 | The colours are hard-coded and not user-configurable | usability | medium |
| 4.1 | `bench/bench_module_call.zsh` hangs for `LEN ≥ 100` | harness bug | medium |
| 4.2 | The allocation test's count is perturbed by its own test binary | harness bug | low |
| 5.1 | No light/dark or 256-colour support | usability | low |
| 5.5 | The `Builtin` table is `static mut` and zsh writes to it | design | low |
