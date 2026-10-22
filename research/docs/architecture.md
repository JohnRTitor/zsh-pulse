# Architecture — `shell-pulse`

Status: describes the code as it stands in this repository.
Scope: the crates, the layering, the data flow, the threading model, and the
decisions that produced them.

Companion documents:

- [Performance report](performance.md) — the measured numbers.
- [ZLE integration design](zle-integration.md) — how the plugin talks to the line editor.
- [Rust↔Zsh boundary and ownership](rust-zsh-boundary.md) — every symbol, every allocation rule.
- [Known limitations](known-limitations.md) — what is not covered.

The plan this implements is [`../PLAN.md`](../PLAN.md); the work log is
[`../TASKS.md`](../TASKS.md).

---

## 1. What the thing is

`shell-pulse` is a zsh plugin that highlights the command line as the user
types. The lexer is Rust; the line editor is zsh's; the two are joined by a
native module that zsh loads with `zmodload`.

The constraints that shaped everything below:

| Constraint | Consequence |
|---|---|
| The plugin runs inside the user's interactive shell | A crash is a lost session, so every boundary is probed, guarded and tested in a subprocess. |
| Nothing may be spawned on the keystroke path | The work happens in-process, in Rust, on the redraw thread. |
| The per-keystroke budget is ≤ 10 µs | Every buffer is reused; the allocation budget is asserted by a test, not hoped for. |
| zsh's exported symbol set varies between distributions | No zsh symbol is linked at build time; all are resolved with `dlsym` at load time. |
| The lexer must be testable on its own | The lexer and the span layer have no zsh dependency and no `unsafe`. |

---

## 2. The crates

```
                        ┌──────────────────┐
                        │  zsh-module      │  cdylib: shellpulse/highlight.so
                        │  (zsh-module)    │  the only crate that talks to zsh
                        └───┬───────┬───┬──┘
                            │       │   │
          ┌─────────────────┘       │   └──────────────────┐
          ▼                         ▼                      ▼
   ┌────────────────────┐   ┌──────────────┐    ┌──────────────────┐
   │  zsh-abi           │   │  engine      │    │  lexer           │
   │  (zsh-abi)         │   │  (engine)    │    │  (lexer)         │
   │  all the unsafe    │   │  shared      │    │  pure Rust       │
   │  code, all of it   │   │  state,      │    └────────┬─────────┘
   └────────────────────┘   │  worker      │             │ tokens
                            │  thread      │             ▼
                            └──────────────┘    ┌──────────────────┐
                                                │  highlight       │
                                                │  (highlight)     │
                                                │  spans, offsets  │
                                                └──────────────────┘
```

Arrows point *from* the dependent *to* the dependency: `zsh-module` uses
`zsh-abi`, `engine`, `lexer` and `highlight`; `highlight` uses `lexer`;
`engine` declares `zsh-abi` in its manifest but does not reference it.

| Crate | Path | Depends on | `unsafe` | What it owns |
|---|---|---|---|---|
| `zsh-abi` | [`crates/zsh-abi/`](../../crates/zsh-abi) | *nothing* | **all of it** | The zsh ABI: transcribed structs, `dlsym` resolution, the capability probe, panic guards, the resolved function-pointer table, and the ownership rules for memory that crosses into zsh. |
| `lexer` | [`crates/lexer/`](../../crates/lexer) | *nothing* | none | A zsh command-line lexer in pure Rust: tokens, an exactly-comparable parse state, resume points, and the incremental re-lexer. |
| `highlight` | [`crates/highlight/`](../../crates/highlight) | `lexer` | none | Styled spans over a line: run-merging, byte↔character offset conversion, and a diff of two span sets. |
| `engine` | [`crates/engine/`](../../crates/engine) | `zsh-abi` (declared, not used) | none | Process-wide engine state: the line revision counter, the highlight-call counter, and the optional worker thread. |
| `zsh-module` | [`crates/zsh-module/`](../../crates/zsh-module) | `zsh-abi`, `engine`, `lexer`, `highlight` | a little | The `cdylib`: the module entry points, the builtin, the highlighter state, and the `region_highlight` writer. |
| *(no crate)* | [`zsh/init.zsh`](../../zsh/init.zsh) | — | — | The 63-line zsh adapter: load the module, install the redraw hook, chain any hook that was already there. |

The workspace has **zero external dependencies**
([`Cargo.toml`](../../Cargo.toml)): `Cargo.lock` contains only the five
workspace members. `lexer` and `highlight` are `#![forbid(unsafe_code)]`;
`engine` contains no `unsafe` at all; the `unsafe` lives in `zsh-abi` and
`zsh-module`, which is where it belongs.

### Why the split is where it is

* **`lexer` and `highlight` know nothing about zsh.** They are the parts that
  can be unit-tested and benchmarked without a shell, and they are the parts
  with the interesting algorithms. `lexer` is `#![forbid(unsafe_code)]`
  ([`crates/lexer/src/lib.rs:37`](../../crates/lexer/src/lib.rs#L37)) and
  `highlight` likewise
  ([`crates/highlight/src/lib.rs:25`](../../crates/highlight/src/lib.rs#L25)).
* **`zsh-abi` is the only crate that contains the FFI.** Its own module
  documentation states the rule: "This crate contains **all** unsafe code in
  the project" ([`crates/zsh-abi/src/lib.rs:3-4`](../../crates/zsh-abi/src/lib.rs#L3)).
  The audit surface is one directory.
* **`zsh-module` is a shell, not a library.** It owns the module ABI, the
  builtin table, and the mutable state that lives for the life of the shell
  process. It is the crate that has to be right about lifetimes.
* **`engine` exists so the interesting logic is not inside the FFI crate.**
  The revision counter and the worker thread are ordinary safe Rust with
  ordinary tests.

---

## 3. Data flow: a keystroke to the screen

This is the path that runs on every redraw. Each step names the file that
implements it.

```
 user types a character
        │
        ▼
 zsh's line editor redraws
        │
        ▼
 redrawhook()  ── zsh-src/Src/Zle/zle_main.c:1066
        │  looks up the widget named "zle-line-pre-redraw"
        │  and runs it (zsh-src/Src/Zle/zle_main.c:1069, :1087)
        ▼
 shell-pulse-highlight-redraw()          zsh/init.zsh:39
        │  calls the builtin with --region-highlight
        ▼
 shell-pulse-highlight  (the builtin)    crates/zsh-module/src/lib.rs:288
        │
        ├─ guard(...)                       crates/zsh-abi/src/guard.rs:47
        │     a Rust panic can never unwind into zsh's C frames
        │
        ├─ zsh()  →  Api::resolve(probe())  crates/zsh-abi/src/api.rs:111
        │     the six zsh symbols, resolved once, by dlsym
        │
        ├─ wants_regions(args)              crates/zsh-module/src/lib.rs:389
        │     the "--region-highlight" flag; without it nothing is published
        │
        ├─ read CURSOR                     crates/zsh-abi/src/api.rs:213
        │     getsparam_u + parse_i64, no allocation
        ├─ read BUFFER                     crates/zsh-abi/src/api.rs:184
        │     getsparam_u into a reused Vec<u8>; no allocation once warm
        │
        ├─ engine().record_highlight()     crates/engine/src/state.rs:97
        │     bumps the line revision; a stale async result is refused
        │
        ├─ Highlighter::update(...)         crates/zsh-module/src/lib.rs:151
        │     │
        │     ├─ LexCache::relex(buffer)   crates/lexer/src/incremental.rs:166
        │     │     ├─ length < 64  → full re-parse
        │     │     └─ length ≥ 64  → resume from a checkpoint, splice the tail
        │     │
        │     ├─ SpanSet::update_from_tokens   crates/highlight/src/lib.rs:129
        │     │     run-merge the tokens into styled spans
        │     │
        │     ├─ if the spans are unchanged → return, zsh is not touched
        │     │
        │     ├─ region::Entries::build(...)  crates/zsh-module/src/region.rs:89
        │     │     one reusable byte buffer, "<start> <end> <attr> memo=shell-pulse"
        │     │
        │     ├─ if the elapsed time > 1 ms → clear and return
        │     │
        │     └─ Api::set_param_array("region_highlight", rows)
        │           crates/zsh-abi/src/api.rs:313
        │           zalloc for the array, ztrdup for each row, one setaparam
        │
        ▼
 zsh's region_highlight setter            zsh-src/Src/Zle/zle_refresh.c:488
        │  parses each row, then freearray(av)  zsh-src/Src/Zle/zle_refresh.c:586
        ▼
 the region is drawn with the terminal attribute
```

### The three "do nothing" decisions

Every one of them is a decision to *not* call into zsh, and each has a test:

1. **The line did not change** (`outcome.unchanged` and we have already
   published) → return without touching `region_highlight`
   ([`crates/zsh-module/src/lib.rs:174-178`](../../crates/zsh-module/src/lib.rs#L174)).
   This one only fires for lines of 128 bytes or more — see
   [known limitations §1.6](known-limitations.md).
2. **The styling did not change** (the new span set equals the published one)
   → return ([`crates/zsh-module/src/lib.rs:181-186`](../../crates/zsh-module/src/lib.rs#L181)).
3. **The caller did not ask** (no `--region-highlight`) → read the line, change
   nothing ([`crates/zsh-module/src/lib.rs:188-194`](../../crates/zsh-module/src/lib.rs#L188)).

The third is what keeps the module from ever overwriting a `region_highlight`
array that belongs to the user or to another plugin. It is checked by
`the_regions_are_not_published_unless_the_caller_asks` in
[`crates/zsh-module/tests/zsh_integration.rs`](../../crates/zsh-module/tests/zsh_integration.rs).

### The failure path

The lexer refuses to represent some inputs exactly — nesting deeper than
`MAX_DEPTH` (8), a here-document delimiter longer than `MAX_HEREDOC_LEN` (32),
a second here-document opened before the first was consumed
([`crates/lexer/src/state.rs:37`](../../crates/lexer/src/state.rs#L37),
[`crates/lexer/src/state.rs:43`](../../crates/lexer/src/state.rs#L43),
[`crates/lexer/src/scanner.rs:62-64`](../../crates/lexer/src/scanner.rs#L62)).
When that happens the module **clears** the highlights rather than leaving
the previous line's regions on screen, and the line is drawn unstyled:

```rust
// crates/zsh-module/src/lib.rs:166-173
let outcome = match self.cache.relex(buffer) {
    Ok(o) => o,
    Err(_) => {
        unsafe { self.clear(api, publish) };
        return;
    }
};
```

A wrong highlight is worse than no highlight, so the design fails *visible* and
*uncoloured* rather than *quietly wrong*.

---

## 4. The threading model

### Today

There is exactly one thread that touches zsh: the shell's own. The optional
worker thread never calls into zsh.

| Thread | Created | May call into zsh? | Lifetime |
|---|---|---|---|
| The shell's main thread | by zsh | yes — it is zsh | the shell's life |
| The engine worker (`shellpulse-wrk`) | `Worker::spawn`, lazily, from `boot_` ([`crates/zsh-module/src/lib.rs:603`](../../crates/zsh-module/src/lib.rs#L603)) | **never** — it holds no zsh handle at all ([`crates/engine/src/worker.rs:8-12`](../../crates/engine/src/worker.rs#L8)) | until the `Worker` is dropped, which joins the thread ([`crates/engine/src/worker.rs:201-214`](../../crates/engine/src/worker.rs#L201)) |

The worker's discipline is two rules
([`crates/engine/src/worker.rs:8-15`](../../crates/engine/src/worker.rs#L8)):

1. **The worker never calls into zsh.** Everything crossing the boundary is
   `Vec<u8>` and integers ([`crates/engine/src/worker.rs:54-68`](../../crates/engine/src/worker.rs#L54)).
2. **Only the owning thread submits.** A worker submitting to its own reply
   channel would deadlock.

The worker is **optional**: `Worker::spawn` returns a `Result`, and
`worker::shared()` yields `None` if the thread could not be created
([`crates/engine/src/worker.rs:239-241`](../../crates/engine/src/worker.rs#L239)).
The engine is fully functional with zero workers.

### The residual risk, stated plainly

zsh installs process-wide signal handlers. A signal delivered to the process
can land on *our* thread, where zsh's handler would run on a stack zsh knows
nothing about ([`crates/engine/src/worker.rs:18-25`](../../crates/engine/src/worker.rs#L18)).
This cannot be fixed from inside a module.

What has been **measured**: 300 iterations of `kill -WINCH $$` and
`kill -CONT $$` while the worker is alive, and zsh survives
(`zsh_survives_signals_delivered_while_a_worker_is_alive` in
[`crates/zsh-module/tests/zsh_integration.rs`](../../crates/zsh-module/tests/zsh_integration.rs)).
What has **not** been measured: signals tied to job control (SIGCHLD, SIGINT)
arriving while zsh is actually running jobs. The mitigation is structural, not
empirical: the worker holds no zsh handle, and the engine degrades to
single-threaded if the thread cannot start.

### The revision guard

Every keystroke advances a monotonic `revision: u64`
([`crates/engine/src/state.rs:27-29`](../../crates/engine/src/state.rs#L27)).
A result tagged with an older revision is refused
([`crates/engine/src/state.rs:32-34`](../../crates/engine/src/state.rs#L32)).
The rule — "a result whose revision ≠ current is discarded, never applied" —
is the plan's D1 constraint, and it is already in place for the
highlighting path even though no asynchronous producer exists yet.

---

## 5. Decisions taken, and the alternatives rejected

Each decision names the alternative it displaced and the reason. Where the
reason is a measurement rather than an argument, the bullet says so and
points at the harness.

### 5.1 A native module, not a spawned helper

* **Chosen:** a Rust `cdylib` loaded with `zmodload`.
* **Rejected:** forking a helper per keystroke (~1–3 ms), a persistent helper
  over a socket (~20–60 µs round trip), a daemon.
* **Why:** the native call itself is ~112 ns
  ([`../upstream/PERFORMANCE_COMPARISON.md` §2](../upstream/PERFORMANCE_COMPARISON.md)),
  four orders of magnitude below the fork. The full reasoning is in
  [`../upstream/ARCHITECTURE_DECISION.md`](../upstream/ARCHITECTURE_DECISION.md).
* **Measured:** yes — the 112 ns figure and the per-call cost of *this* module
  are both in the [performance report](performance.md).

### 5.2 `dlsym` at load time, not link-time symbols

* **Chosen:** every required zsh symbol is resolved with `dlsym(RTLD_DEFAULT, …)`
  during `setup_` ([`crates/zsh-abi/src/probe.rs:72-84`](../../crates/zsh-abi/src/probe.rs#L72)).
* **Rejected:** `extern "C" { fn getsparam(..); }` — an undefined dynamic
  reference, which dies at the first call, arbitrarily late, inside the user's
  shell; and it makes the crate unlinkable in a non-zsh process, which removes
  the ability to unit-test anything around it
  ([`crates/zsh-abi/src/api.rs:4-20`](../../crates/zsh-abi/src/api.rs#L4)).
* **Consequence:** the crate has **no link-time dependency on zsh at all** and
  links normally under `cargo test`. A missing symbol is a clean load failure
  (`setup_` returns −1, [`crates/zsh-module/src/lib.rs:568-592`](../../crates/zsh-module/src/lib.rs#L568)),
  never a later crash.

### 5.3 `zalloc` and `ztrdup`, never `zhalloc` and `dupstring`

* **Chosen:** `zalloc` for the array, `ztrdup` for the elements.
* **Rejected:** `zhalloc` / `dupstring`, which look equivalent and allocate
  from zsh's **pool** allocator. Pool memory is rewound and recycled by
  `popheap()` ([`zsh-src/Src/mem.c:443`](../upstream/zsh-src/Src/mem.c#L443)),
  which zsh runs around every command
  ([`zsh-src/Src/exec.c:1275`](../upstream/zsh-src/Src/exec.c#L1275)).
* **Why it matters:** a parameter holding pool memory points at recycled
  memory the moment the next command runs. Observed as a `SIGSEGV` inside
  `sepjoin`.
* **Enforced:** the probe requires `zalloc` and `ztrdup` and explicitly
  *not* the pool allocators, and a unit test asserts their absence from the
  required set ([`crates/zsh-abi/src/probe.rs:117-125`](../../crates/zsh-abi/src/probe.rs#L117)).
* **Verified by:** two subprocess tests that write the same parameter 25 and
  200 times, run a command in between (which is what triggers zsh's
  pushheap/popheap pair) and read the value back.

This is the single most important rule in the project. It is written out in
full in the [boundary document](rust-zsh-boundary.md).

### 5.4 The lexer is pure Rust and has no zsh dependency

* **Chosen:** a hand-written lexer, porting ble.sh's checkpoint splice
  ([`ble.sh/lib/core-syntax.sh:5540-5550`](../upstream/ble.sh/lib/core-syntax.sh)).
* **Rejected:** Tree-sitter. fish does not use it and hand-writes its lexer
  ([`../upstream/LESSONS.md`](../upstream/LESSONS.md), "Investigate" §4).
* **Why:** the lexer is where the interesting, testable algorithm lives, and it
  must be benchmarkable without a shell.

### 5.5 The parse state is compared exactly, never approximately

* **Chosen:** `LexState::same_parse` is `==` ([`crates/lexer/src/state.rs:327-329`](../../crates/lexer/src/state.rs#L327)).
* **Rejected:** a hash or similarity score as the *decision*. The `key()` hash
  exists but is only a pre-filter ([`crates/lexer/src/state.rs:293-321`](../../crates/lexer/src/state.rs#L293)).
* **Why:** the splice reuses the old tail byte for byte. A "close enough"
  comparison splices a tail parsed as if it were inside a quote onto a line
  where it is not — highlighting that lies about the command.

### 5.6 A resume point records the character that decided where its unit starts

* **Chosen:** every resume point carries a `guard` — the offset of the
  character that decided where the unit starts
  ([`crates/lexer/src/lib.rs:66-76`](../../crates/lexer/src/lib.rs#L66)).
* **Rejected:** resuming from the last checkpoint at or before the edit.
* **Why:** without the guard, editing the newline that ended a here-document
  delimiter invalidates the checkpoint, and the incremental path produces
  *wrong* highlighting rather than none. The regression test is
  `a_chained_edit_that_changes_a_token_kind_agrees_with_a_full_scan`.

### 5.7 One hook, not a re-wrapped widget table

* **Chosen:** `zle-line-pre-redraw` ([`zsh/init.zsh:39-63`](../../zsh/init.zsh#L39)).
* **Rejected:** wrapping every widget the way FSH does
  ([`../upstream/fsh/fast-syntax-highlighting.plugin.zsh:242-284`](../upstream/fsh/fast-syntax-highlighting.plugin.zsh)),
  whose cost scales with the user's widget count.
* **Why:** the hook is called once per redraw regardless of how many widgets
  the user has, and zsh's own documentation describes it as exactly the
  opportunity to update `region_highlight`
  ([`zsh-src/Doc/Zsh/zle.yo:1164-1168`](../upstream/zsh-src/Doc/Zsh/zle.yo#L1164)).

### 5.8 The adapter chains an existing hook

* **Chosen:** if a `zle-line-pre-redraw` widget already exists, remember it and
  call it from ours ([`zsh/init.zsh:51-61`](../../zsh/init.zsh#L51)).
* **Rejected:** `zle -N zle-line-pre-redraw …` unconditionally.
* **Why:** zsh allows one widget per name, and other plugins use this hook.

### 5.9 A wall-clock budget, and a bail to unstyled

* **Chosen:** a 1 ms budget per keystroke
  ([`crates/zsh-module/src/lib.rs:92`](../../crates/zsh-module/src/lib.rs#L92))
  and a 4 KiB line limit ([`crates/zsh-module/src/lib.rs:101`](../../crates/zsh-module/src/lib.rs#L101)).
* **Rejected:** no limit (zsh-syntax-highlighting) — a pathological line then
  eats a keystroke.
* **Measured:** the budget is 100× the *target* per-keystroke cost (10 µs)
  and about 1 500× the *measured* one (0.67 µs); see the
  [performance report](performance.md).

### 5.10 No Tokio, no async runtime

* **Chosen:** one `std::thread` and an `mpsc` channel
  ([`crates/engine/src/worker.rs:38-47`](../../crates/engine/src/worker.rs#L38)).
* **Rejected:** Tokio. Unproven against this workload; a runtime would add
  per-shell memory for no demonstrated benefit.

### 5.11 The full re-parse wins below 128 bytes

* **Chosen:** `LexCache::relex` re-parses a line shorter than
  `INCREMENTAL_MIN_LEN` (64) outright
  ([`crates/lexer/src/incremental.rs:53`](../../crates/lexer/src/incremental.rs#L53),
  [`crates/lexer/src/incremental.rs:166-171`](../../crates/lexer/src/incremental.rs#L166)).
* **Rejected:** always taking the incremental path.
* **Why:** measured. Below 128 bytes the diff and the merge cost more than the
  scan they save. The plan says to report that and fall back, and the code
  does. The raw machinery stays reachable as `relex_incremental` so that it
  remains covered by the property tests.

---

## 6. What is not built yet

The plan's Stage D (asynchronous completion, history intelligence, fuzzy
ranking) is **not started**. The seams it needs are in place:

| Stage D piece | What exists today | What is missing |
|---|---|---|
| D1 async core | the revision counter, the worker thread, the bounded channel | a producer that submits work, and a consumer that applies the result |
| D2 completion | nothing | the providers; the decision is native-zsh-first |
| D3 ranking | nothing | the scorer |
| D4 history | nothing | the background index |

The plan's Stage E deliverables are the documents in this directory; the
benchmark harnesses are in [`bench/`](../../bench).

---

## 7. Where to look next

| Question | File |
|---|---|
| How fast is it, and how was that measured? | [performance.md](performance.md) |
| How does the plugin talk to the line editor? | [zle-integration.md](zle-integration.md) |
| Which zsh symbols does it call, and who owns the memory? | [rust-zsh-boundary.md](rust-zsh-boundary.md) |
| What is *not* right yet? | [known-limitations.md](known-limitations.md) |
| Why was it built this way? | [`../upstream/ARCHITECTURE_DECISION.md`](../upstream/ARCHITECTURE_DECISION.md) |
| What did the upstreams get wrong? | [`../upstream/PERFORMANCE_FAILURES.md`](../upstream/PERFORMANCE_FAILURES.md) |
| What was learned from other projects? | [`../upstream/LESSONS.md`](../upstream/LESSONS.md) |
