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
editor and asserts on the **screen it drew**. The harness reconstructs that
screen from the bytes zsh writes — the text, the cursor motions, the erases
and the attributes in force — and then checks the attributes of the
characters of the command word. The file holds eight tests, and every one of
them skips with a printed reason when the environment cannot provide what
it needs.

**Why the screen and not the bytes.** zsh's line editor draws the line in
pieces and moves the cursor between them, so the text of the line can be
broken up — and even reordered — in the byte stream: what the user sees may
be `echo SPARGXYZ` while the bytes are `SPARG` `ESC[10D` `echo` `ESC[6C`
`X` `ESC[11D` `YZ`. A search for a piece of text in the raw bytes finds it
only when zsh happened to draw it in one piece, and which of the two
happens depends on how the input reached the shell. The screen is what the
user sees, and it is the same every time. Asserting on it is also a
*stronger* claim: the test can say that the characters of the command word
were drawn green, not merely that the bytes for green appeared somewhere.

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

### 1.3 A *clear* still replaces `region_highlight` wholesale, so it clears another plugin's entries

*(Fixed: the module now merges. It reads the entries zsh holds, drops the ones
carrying its own `memo=shell-pulse`, keeps everyone else's, and appends its
own — exactly what `zsh-syntax-highlighting` does
(`research/upstream/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh:168-175`).
The residual is that a *clear* still replaces the whole array, so another
plugin's entries are lost when the plugin bails out to unstyled.)*

**What.** `region_highlight` is a single array, and zsh's setter replaces the
whole user-settable part of it
([`zsh-src/Src/Zle/zle_refresh.c:488-586`](../upstream/zsh-src/Src/Zle/zle_refresh.c#L488)).
Only zsh's four *special* regions — the region between point and mark, the
isearch region, the suffix and the pasted text — survive
([`zsh-src/Src/Zle/zle.h:473`](../upstream/zsh-src/Src/Zle/zle.h#L473)).

So if another plugin also writes `region_highlight` **without** merging, the
last writer wins and the other's regions disappear.

**How you can tell.** Our own entries carry `memo=shell-pulse`
([`crates/zsh-module/src/region.rs:34`](../../crates/zsh-module/src/region.rs#L34)),
which is exactly the mechanism zsh's documentation suggests for telling
plugins' entries apart
([`zsh-src/Doc/Zsh/zle.yo:986-998`](../upstream/zsh-src/Doc/Zsh/zle.yo#L986)).
The module reads the array back, drops the entries that carry its own
`memo`, keeps everyone else's and appends its own. The behaviour is asserted
by `another_plugins_entries_survive_our_publish` in
[`crates/zsh-module/tests/zsh_integration.rs`](../../crates/zsh-module/tests/zsh_integration.rs).

**What is still true.** The *clear* path — taken when the line cannot be
lexed, when the line is over 4 KiB, or when the 1 ms budget is exceeded —
writes an empty array, and that still removes another plugin's highlighting
as well. The merge is not applied on that path, because there is nothing to
merge with: the module does not know what the other plugin's entries were.

**What it would take** to fix that last part: reading the array back on the
clear path too, and writing back everything that is not ours. zsh's own
suggestion for this is to do the filtering in zsh
(`region_highlight=(${region_highlight:#*memo=…})`), which would mean moving
the merge into the adapter; doing it in Rust means reading the array back
through the parameter getter, whose return value is a `sepjoin`ed string
rather than an array, so the rows would have to be re-split.

### 1.4 The incremental lexer is not used below 128 bytes, because it is slower there

**What.** `LexCache::relex` re-parses a line shorter than
`INCREMENTAL_MIN_LEN` (128) outright
([`crates/lexer/src/incremental.rs:53`](../../crates/lexer/src/incremental.rs#L53),
[`crates/lexer/src/incremental.rs:219-224`](../../crates/lexer/src/incremental.rs#L219)).
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
([`crates/lexer/src/incremental.rs:235`](../../crates/lexer/src/incremental.rs#L235)).

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
([`crates/highlight/src/lib.rs:223-241`](../../crates/highlight/src/lib.rs#L223))
exists, is tested against a deliberately naive reference implementation
([`crates/highlight/src/lib.rs:510`](../../crates/highlight/src/lib.rs#L510)),
and **is not called by the module**. The module compares the whole span set
for equality and, if it differs, republishes *every* region
([`crates/zsh-module/src/lib.rs:241-246`](../../crates/zsh-module/src/lib.rs#L241),
[`crates/zsh-module/src/lib.rs:322-331`](../../crates/zsh-module/src/lib.rs#L322)).

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

### 1.6 The `unchanged` fast path saves the parse, not the comparison

*(Fixed: `LexCache::relex_short` compares the new text with the one it has
and reports `unchanged: true` when they are equal
([`crates/lexer/src/incremental.rs:473-485`](../../crates/lexer/src/incremental.rs#L473)),
so the "the line did not change" short-circuit fires for a short line as
well. The residual is that the comparison itself is linear in the length of
the line — see below.)*

**What.** `Highlighter::update` has two short-circuits: "the line did not
change" and "the styling did not change"
([`crates/zsh-module/src/lib.rs:216-246`](../../crates/zsh-module/src/lib.rs#L216)).
The first one is driven by `RelexOutcome::unchanged`. It used to be dead
below the 128-byte threshold, because a short line was re-parsed rather
than diffed and `relex_short` therefore always reported `false`.

**What is still true.** The comparison is linear in the length of the line:
`relex_short` compares the whole text, and the incremental path compares
the common prefix and the common suffix, which is linear in the worst case
as well. The fast path therefore saves the *parse*, not the *comparison* —
the same linearity §1.1 is about.

### 1.10 The whole-line colouring cannot know everything

The line is now coloured in more places than it used to be, and each of the
new colours depends on something the plugin does not have. These are the
edges, and they are all deliberate.

**A quoted command word is neither coloured as a command nor looked up.**
To the lexer, `'ls'` is a quoted string rather than a command word, so the
shell's answer about it — the shell looks the name up with the quoting
removed — has no command word to apply to. The word is coloured as the
quoted string it is, and the filesystem is not consulted for it either: a
quoted word is not literal text. The same is true of `"ls"`.

**A command inside a compound statement is not coloured as a command.**
The lexer has no notion of reserved words. In `if true; then echo hi; fi`
it sees `if`, `then` and `fi` as command words and `echo` as an ordinary
argument: `then` ends the command position, so the word after it is an
argument and not a command. The same is true of `while … do … done`,
`until … do … done`, `for … do … done` and of a brace group `{ … }`. The
command inside the compound statement is therefore drawn as the user typed
it, while the reserved words around it are coloured like commands — the
shell knows those, and the shell's answer applies to them.

**`~user` is not resolved.** `~` and `~/…` are expanded to the home
directory, but `~user` would need a lookup in the password database — on the
interactive thread, on every keystroke. It is left alone.

**The lookup is a `stat`, not an `lstat`.** It follows symlinks, so a
symlink that points at a directory is coloured as a directory. That is the
answer the shell itself would give, but it is not the answer `lstat` would
give.

**A word that is not literal text is never looked up.** `"$HOME/x"` and
`$(pwd)/x` name a file, but the file they name is only knowable by running
the shell, so they are left alone.

**A relative word is resolved against `$PWD`, which is the shell's
*logical* path.** `$PWD` may contain symlinks, and the physical path — what
`pwd -P` prints — may be a different one. A relative path is therefore
resolved against the logical directory, which is the directory the command
would run in.

**Both caches expire after two seconds.** The module's path cache holds 64
answers and forgets each of them after two seconds, so a file that is
created or deleted while the session runs is picked up, but not instantly:
for up to two seconds the older answer is still the one that is used. The
adapter's cache of the shell's answers expires on the same schedule, and it
remembers the negative answers as well as the positive ones — a word the
shell knew may stop being known when the user runs `unalias` or removes a
function, and a command installed while the session runs has to be picked
up too.

**At most 16 words of a line are looked up on the filesystem.** Every
literal word is a candidate for naming a file — `Cargo.toml` names a file
just as much as `./Cargo.toml` does, and the reference implementation looks
every word up
([`../upstream/fsh/fast-highlight:1265-1295`](../upstream/fsh/fast-highlight#L1265)).
The number of lookups on one line is therefore bounded
(`MAX_PATH_LOOKUPS`, 16), and the words are offered to the lookup in the
order they appear, so the ones nearest the command — the ones most likely
to be paths — are the ones that get looked up. A word past the bound is
left as the user typed it. A word that is not literal text costs no
syscall, but it still counts against the bound.

**At most 32 regions are published for one line** (`region::MAX_ENTRIES`).
A long line — many quoted words, many options — produces more regions than
the old colouring did, and the ones past the cap are dropped. The truncation
is reported when `SHELL_PULSE_DEBUG` is set, but it is not otherwise
visible.

**A command word the shell was not asked about is never painted red or
green.** The module colours a command word green or red only when the shell
has answered for that exact word. Such a word is still looked up on the
filesystem like every other literal word — so `~/bin/tool` is magenta — and
otherwise it is drawn as the user typed it. A line the adapter could not
parse, or a shell whose `zsh/parameter` is not loaded, therefore has no red
words in it at all.

**A substitution is only looked inside when it makes up the whole word.**
`$(cmd)` and `` `cmd` `` hold a command of their own, and the adapter
answers for the command inside them. `x$(cmd)y` is not looked inside: the
text between the `$(` and the `)` is then not the whole command, and the
command word inside it is left alone.

### 1.11 The pathological line is bounded on both sides of the boundary

**What.** Three limits stand between a pathological line and the interactive
thread, and they sit on both sides of the Rust↔Zsh boundary:

| Bound | Value | Where | What it stops |
|---|---|---|---|
| the adapter's line cap | 4 KiB (`SHELL_PULSE_MAX_LINE`) | [`zsh/widgets.zsh:90`](../../zsh/widgets.zsh#L90) | the line is not split and its words are not walked, so a pasted megabyte costs a length comparison and not a split |
| the module's line limit | 4 KiB (`MAX_LINE`) | [`crates/zsh-module/src/lib.rs:102`](../../crates/zsh-module/src/lib.rs#L102) | the line is not highlighted, and its bytes are not copied out of zsh at all |
| the module's answer cap | 4 KiB (`MAX_ANSWER_BYTES`) | [`crates/zsh-module/src/lib.rs:1650`](../../crates/zsh-module/src/lib.rs#L1650) | the words the shell answers about are not copied into the module's own buffer without limit |

The last one matters because the module's buffers are deliberately reused:
a buffer that had grown to the size of a paste would keep that capacity for
the rest of the session. An answer that does not fit is dropped and the
command word it was about is left alone — a *missing* colour, never a wrong
one.

**What it is worth.** None of these bounds is a substitute for the 1 ms
budget: they bound the *size* of the work, the budget bounds its
*duration*. The two sides of the boundary are described in
[ZLE integration §5](zle-integration.md) and
[boundary §3](rust-zsh-boundary.md).

---

## 2. Things that are measured but not optimised

### 2.1 The "everything else" is 80 % of the keystroke

On a 28-byte line the end-to-end cost is ~0.86 µs, of which the lexing is
~0.08 µs. The remaining ~0.40 µs is zsh's own parameter machinery: walking
the parameter table, `setarrvalue`, and `set_region_highlight` parsing every
row it is given.

That is not a defect — it is the cost of the API. But it means **the number
of regions matters more than the length of the line**, which is why
`region::MAX_ENTRIES` is 32 and why the "skip when unchanged" rule is worth
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

A paste over 4 KiB is caught by the line-length limit — by the adapter
before it splits the line, and by the module before it copies it — and a
slow one by the 1 ms budget. A paste *under* 4 KiB is highlighted in full,
once, on the redraw after the paste — which costs one full parse of the
pasted line, not one per pasted character.

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
([`crates/zsh-module/src/region.rs:91-120`](../../crates/zsh-module/src/region.rs#L91)).
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

### 5.2 An ordinary argument is not highlighted

By design
([`crates/zsh-module/src/region.rs:118`](../../crates/zsh-module/src/region.rs#L118)),
an ordinary argument gets no region at all, so it is drawn exactly as the
user typed it. That is a deliberate readability choice, not a limitation —
but it does mean the plugin highlights *structure*, not *everything*.

The two exceptions are the words that say something about the machine
rather than about the command: an option is cyan, and a word that names a
file that exists is magenta. An argument that is neither is left alone.

### 5.3 The plugin owns the `region_highlight` array while it runs

See §1.3. The entries are merged rather than replaced, but there is no
negotiation with other plugins, and a *clear* still empties the array.

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
([`crates/zsh-module/src/lib.rs:1758`](../../crates/zsh-module/src/lib.rs#L1758)).
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
| 1.3 | A *clear* still empties `region_highlight`; another plugin's entries are lost | correctness | medium — the ordinary publish merges |
| 1.4 | The incremental path is unused below 128 bytes | design | low, measured |
| 1.5 | The per-range diff is implemented but not used | dead code | low |
| 1.6 | The `unchanged` fast path saves the parse but not the comparison | performance | low, inherent |
| 1.10 | The whole-line colouring cannot know everything: a quoted command word is neither coloured as a command nor looked up, a command inside a compound statement is not coloured as a command, `~user` is not resolved, symlinks are followed, a word that is not literal text is never looked up, both caches expire after 2 s, at most 16 words of a line are looked up on the filesystem, a long line loses the regions past 32, an unanswered command word is never painted red or green, and a substitution that is not the whole word is not looked inside | correctness | low, by design |
| 1.11 | The pathological line is bounded on both sides of the boundary: the adapter's 4 KiB line cap, the module's 4 KiB line limit, the module's 4 KiB answer cap | performance | low, by design |
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
