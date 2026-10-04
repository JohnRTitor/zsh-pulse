# Performance Comparison

All numbers below were **measured on this machine**, not copied from project
documentation. Machine: 12 cores, ~1.3 GB RAM available, zsh 5.9.2 (NixOS),
Linux 5.x, clang 21, Rust 1.98.1. Free memory is low, so heavy upstreams were
read but never built.

Where a project publishes a performance claim, the claim and the evidence are
recorded separately (§7). **No upstream ships a reproducible end-to-end
per-keystroke benchmark**, which is itself a finding.

---

## 1. Baseline: FSH per-keystroke highlight cost (measured)

Harness `research/upstream/bench/bench_hl.zsh` calls each highlighter's entry
point directly with `BUFFER` **mutated on every iteration** — deliberately, so
that FSH's `_zsh_highlight_buffer_modified` guard
(`fast-syntax-highlighting.plugin.zsh:199-202`) cannot short-circuit to the
unchanged-buffer fast path. An earlier run *without* this mutation reported a
misleadingly flat ~52 µs by measuring that fast path; the numbers below are the
corrected ones.

2000 iterations per shape:

| Buffer | Length | Empty-loop | FSH total | **FSH net** |
|---|---|---|---|---|
| `git commit --amend --no-edit` | 29 | 12.1 µs | 52.1 µs | **≈40 µs** |
| `cargo build … \| tee … && kubectl apply …` | 89 | 12.8 µs | 52.6 µs | **≈40 µs** |
| `echo "$(date)" … \| grep -E … > /dev/null` | 80 | 12.3 µs | 52.1 µs | **≈40 µs** |
| synthetic pipeline | 831 | 13.5 µs | 64.3 µs | **≈51 µs** |
| synthetic pipeline | 1671 | 15.0 µs | 78.5 µs | **≈63 µs** |
| synthetic pipeline | 3351 | 18.9 µs | 101.8 µs | **≈83 µs** |

**Fitted cost model: `≈40 µs fixed + ≈0.013 µs per character`.**

Two conclusions that matter:

1. **A large fixed cost dominates.** Even a 29-character command costs ~40 µs
   of pure overhead — `emulate -LR zsh`, `setopt`, iterating the widget table,
   and `region_highlight=( $reply )` array copies
   (`fast-syntax-highlighting.plugin.zsh:77-110`). Parsing is *not* the main
---

## 2. Baseline: native module boundary cost (measured)

A hand-declared shared object — structurally identical to what a Rust `cdylib`
would be — was loaded with `zmodload` and registered a native builtin.
200,000 builtin invocations, `hyperfine`, shell startup subtracted:

| Arm | Time (mean ± σ) |
|---|---|
| `zsh -fc 'for i in {1..200000}; do :; done'` | 245.2 ms ± 6.9 ms |
| `zsh -fc '… zmodload p6; for i in {1..200000}; do p6-hl; done'` | 270.1 ms ± 3.0 ms |

**Marginal cost of a native call that reads `BUFFER`: ≈112 ns.**

This is the decisive number. A forked helper costs ~1–3 ms per call, so the
native module boundary is roughly **four orders of magnitude cheaper** than the
process model, and cheaper than any round-trip IPC design.

---

## 3. Where the alternatives lose

| Mechanism | Cost | Verdict |
|---|---|---|
| Fork + exec helper per keystroke | ~1–3 ms | Rejected: 10⁴× the native call |
| Persistent helper over socket | ~20–60 µs round trip | Rejected: ~2× FSH's *entire* highlight budget |
| zsh-autosuggestions history lookup | `${history[(r)pattern]}` — **linear glob scan of the whole in-memory history, per keystroke** (`zsh-autosuggestions.zsh:662`), plus a `$( )` fork | Unbounded with history size |
| zsh-autocomplete pty probe | forks a second zsh via `zpty` (`Functions/Init/.autocomplete__async:225`) purely to measure one integer, `compstate[list_lines]` | Largest single cost in that design |
| **Native module** | **112 ns** | **Selected** |

---

## 4. Completion candidate scale (derived from source)

| Candidates | Strategy | Rationale |
|---|---|---|
| 10 – 1,000 | full sort | < 0.1 ms in Rust |
| 10,000 | `select_nth_unstable` (O(n)) then sort the head | atuin: `crates/atuin-daemon/src/search/index.rs:565-574`; parallelises above 10k (`index.rs:547-555`) |
| 100,000 | same, plus chunked parallel scoring on workers | keep the keystroke path single-threaded |

**Fuzzy scoring:** fzf's Smith-Waterman variant needs a full `width × m` matrix
when match positions are required (`fzf/src/algo/algo.go:934`) — unacceptable
when only a score is needed. Recommendation: score-only rolling rows (O(width)
scratch) for ranking, plus a greedy backtrace for the ~3 rendered candidates.

---

## 5. What is *not* yet measured

Deliberately absent, to be produced in Stage D once an implementation exists:
keystroke → pixels (needs a real terminal), RSS in an interactive shell,
hot-path allocations, flamegraphs, `perf` counters, 10k/100k candidate
completion latency.

`zsh/zpty` drives a real interactive zsh and captures real redraw escape
sequences (verified) — that is the harness Stage D will use, so that latency is
measured against the real editor loop rather than a synthetic function call.

Tooling: `perf` and `hyperfine` are not installed but are obtainable via
`nix shell nixpkgs#hyperfine` / `nixpkgs#linuxPackages.perf` (verified).

---

## 6. Upstream performance claims vs. evidence

| Claim | Where | Evidence found | Survives? |
|---|---|---|---|
| FSH is "fast" | README | No benchmark code; `test/` is correctness only | Unverified — measured here at ≈40 µs fixed + linear |
| zsh-autosuggestions is "asynchronous" | README | `exec {fd}< <(...)` per keystroke (`zsh-autosuggestions.zsh:790`) | **Hides rather than removes latency — the linear scan still runs** |
| ble.sh is "incremental" | docs + code | Checkpoint splice is real (`lib/core-syntax.sh:5540-5550`) | **Yes** — the only genuinely sub-linear reparse found |
| ble.sh completion limits | `lib/core-complete-def.sh:44` | `bleopt/declare -v complete_limit ''` — **disabled by default** | Limits exist but do not protect the common case |
| zshrs is "42–1590× faster than zsh" | `docs/hot_path_performance.md:36-45` | The project's own published table shows zshrs **slower**, not faster | **No — contradicted by its own doc** |
   cost at typical line lengths.
2. **The marginal cost is linear in buffer length** (~13 ns/char), with no
   incremental reuse. This is the cost an incremental parser must beat.

FSH has *no* default length cutoff: the `ZSH_HIGHLIGHT_MAXLENGTH` test at
`fast-syntax-highlighting.plugin.zsh:89` only applies when the user has set the
variable, so the 3351-char row is genuine full-buffer work.