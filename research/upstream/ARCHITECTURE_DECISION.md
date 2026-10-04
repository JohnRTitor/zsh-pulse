# Architecture Decision

Status: **recommended, pending the Stage B spike confirmation of the two
remaining risks listed under "Residual risks".**
Date: 2026-10-10. Evidence: `PERFORMANCE_COMPARISON.md`, `LESSONS.md`.

---

## Decision

**Rust `cdylib` loaded into the zsh process via `zmodload`, with a ~40-line zsh
adapter, optional worker threads inside the same process, and a
capability-probing loader that degrades to native zsh highlighting/completion if
the module cannot load.**

---

## The boundary, stated concretely (verified experimentally)

A module is `dlopen`ed by zsh and resolved against the zsh binary's exported
dynamic symbols. **No zsh headers are required** — a handful of symbols and
three structs are transcribed, exactly as C modules do.

Verified on this machine (zsh 5.9.2, stock NixOS build):

| Fact | Evidence |
|---|---|
| A hand-declared `.so` loads and registers a native builtin | `zmodload pulse` → `whence -w pulse-hl` = `builtin` |
| The module can read live ZLE state with no copy-out parameter | `pulse BUFFER=[git commit --amend --no-edit] CURSOR=[24]` |
| A native call costs **≈112 ns** | 200k invocations, `PERFORMANCE_COMPARISON.md` §2 |
| Entry symbols are **`setup_` / `features_` / `enables_` / `boot_` / `cleanup_` / `finish_`** | `Src/module.c:1554-1565`; confirmed against `Src/Modules/example.c:198-243`. This zsh defines `DYNAMIC_NAME_CLASH_OK`, so `module_func` calls `dlsym(handle, "setup_")` **unmangled** (`Src/module.c:1774-1777`) — the `_pulse`-suffixed variant is silently ignored |
| Module names must be **slash-separated identifiers** — no dots, no `.so`, no leading `/` | `modname_ok()` at `Src/module.c:2177-2186`. `zmodload /abs/path/mod.so` and `zmodload mod.so` are both rejected; `zmodload mymod/probe` resolving to `mymod/probe.so` in `module_path` works |
| Required structs: `struct hashnode` (`Src/zsh.h:1226-1230`), `struct builtin` (`Src/zsh.h:1440-1448`), `HandlerFunc = int(*)(char*, char**, Options, int)` (`Src/zsh.h:1436`) | transcribed |

**Gotcha recorded the hard way:** `getsparam()` returns **`char *`**, not
`char **` (`Src/params.c:3037-3045` returns `getstrvalue(v)`). Declaring it as
---

## Alternatives considered and rejected

### A. Pure zsh wrapper + Rust CLI, spawned per keystroke
- Advantages: trivially safe, no ABI risk, trivially portable.
- Measured cost: **~1–3 ms per invocation** (fork+exec).
- Decision: **rejected.** 10⁴× the native call, and ~30× FSH's entire 40 µs
  budget. Only acceptable for cold, off-hot-path work (semantic command
  metadata).

### B. Persistent helper process over a socket
- Advantages: full Rust, crash-isolated, no ABI coupling, easy to swap engines.
- Derived cost: **~20–60 µs per round trip** — about 1.5× FSH's *entire*
  per-keystroke budget, before any actual work is done.
- Decision: **rejected as the hot path.** Retained as the **fallback** when the
  module cannot load, and as a possible future home for a shared history index
  (where amortisation across many keystrokes changes the arithmetic).

### C. Generic shared library loaded ad hoc
- Not reachable: zsh exposes no `dlopen` binding other than `zmodload`, and
  `zmodload` requires the module naming and symbol conventions above. Collapses
  into the decision above.
- Decision: **rejected — not a distinct option on zsh.**

### D. Native zsh module implemented in Rust
- This *is* the decision; D and E were evaluated together because in practice
  they are the same artifact with different layering.
- Advantages: as measured.
- Disadvantages: hand-transcribed ABI; a wrong struct or accessor crashes the
  user's shell (demonstrated); availability depends on the local zsh build's
  exported symbols.
- Decision: **selected, with a probing loader and a safe fallback.**

### E. Rust core + thin zsh adapter + internal worker threads
- Selected *as the layering*, but the thread decision is **deferred to
  measurement**.
- The zsh adapter's only jobs: register a builtin/widget, invoke the engine,
  push `region_highlight`, and own the `zle -F` fd. All logic in Rust.

### F. Daemon
- Decision: **rejected.** No measured need; adds IPC, lifecycle, and failure
  modes on the hot path for zero demonstrated benefit.

---

## Threading decision: deferred, with a default

No evidence yet justifies Tokio. Default: **std threads + scoped threads + a
bounded channel**, created lazily (first completion request), one worker for
filesystem/history work, a second reserved for command metadata. Reconsider
Tokio only if a benchmark shows the executor is the bottleneck. Threads inside
the zsh process must never call back into zsh — they return results over the fd,
and the ZLE side reads them.

---

## Completion decision: hybrid, native-zsh-first

Zsh's completion ecosystem (16,551 functions on the target machine) is an asset,
not a liability. The engine will:
1. call native completion first and let it win when it produces matches;
2. add Rust-native providers only where native completion is weak or blocking
   (filesystem listing on huge directories, history ranking, fuzzy reorder);
3. never bypass `compinit`/`compdef`/`zstyle`.

---

## Residual risks (must be retired in Stage B)

1. **Export-set variance.** This zsh exports ~1048 dynamic symbols. NixOS,
   Debian, Fedora and Homebrew builds differ. Mitigation: at load time, resolve
   *every* required symbol via `dlsym` and refuse to load if any is missing,
   falling back to option B. Do **not** rely on link-time failure.
2. **`DYNAMIC_NAME_CLASH_OK` variance.** If a build lacks it, symbols must be
   `<prefix><mangled name>`. Mitigation: export **both** symbol forms; the one
   not used is simply ignored.
3. **Segfault containment.** A wrong accessor kills the user's shell. Mitigation:
   the accessor set is transcribed from `zsh-src` and covered by tests that
   run in a *subshell*, so a crash is a test failure rather than a lost shell.

---

## Gate questions, answered

| Question | Answer |
|---|---|
| Lowest-overhead ZLE integration mechanism? | native module (112 ns measured) |
| Minimum hot-path computation? | mutate only the affected span range; ~40 µs → single-digit µs target |
| What must be asynchronous? | filesystem listing, history ranking, command metadata, git state |
| What state must persist? | lexer checkpoints, last span set, history index (background-built) |
| What can be cached? | spans, command existence (PATH-fingerprinted), fs listings (mtime-keyed) |
| How are stale async results prevented? | monotonic `revision: u64` per request; apply only if it matches |
| How are resources cleaned up? | single owner of the fd map; close on every path (the zasync lesson) |
| How does native completion stay compatible? | delegate first; never replace `compinit`/`compdef` |
| Known pathological cases? | 10k+ file directories, huge history, heredocs, unbalanced quotes |
| What target are we optimising for? | keystroke→highlight latency first; ~40 µs → target ≤10 µs; zero spawns, zero idle CPU |
`char **` produced a pointer into the middle of the buffer's bytes and
**segfaulted zsh**. The accessor surface must be transcribed from source, not
guessed.