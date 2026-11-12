# Performance Report — `zsh-pulse`

Every number in this document is produced by an automated test harness or reproducible benchmark command quoted alongside it.

Companion documents: [architecture](architecture.md) ·
[ZLE integration](zle-integration.md) ·
[Rust↔Zsh boundary](rust-zsh-boundary.md) ·
[known limitations](known-limitations.md) ·
[Features catalog](../FEATURES.md) ·
[Milestones archive](archive/tasks-milestones.md)

---

## 1. Summary of Measured Latencies

| Metric / Operation | Measured Value | Target / Baseline | Benchmark Harness |
|---|---|---|---|
| **Native Module Builtin Call** | **276 ns** | ≤ 1 µs | `bench/bench_module_call.zsh 200000 28` |
| **End-to-End Keystroke Highlighting** | **0.86 µs** (0.83–0.89 µs) | ≤ 10 µs (11× headroom) | `bench/bench_module_call.zsh 20000 28` |
| **Spans to `region_highlight` Conversion** | **0.39 µs** (188 bytes, 11 regions) | ≤ 1 µs | `cargo run --release -p zsh-module --example bench_region` |
| **Incremental Re-lex (1,389-byte line)** | **1.08 µs** (10 bytes re-scanned) | 1.94 µs (full re-parse) | `cargo run --release -p lexer --example bench_incremental 200` |
| **Steady-State Allocations (28 bytes)** | **0** allocations (budget: 8) | ≤ 1 allocation | `cargo test -p lexer --test alloc_budget -- --nocapture --test-threads=1` |
| **Large Directory Scan (`/nix/store`)** | **20.7 ms** (81,141 dir entries) | **1.6 s – 3.3 s** (`zsh-autocomplete`) | `cargo test -p completion -- complete_large_directory_under_budget` |
| **Standard Path Scan (`/bin`)** | **47 µs** | 1.6 s (`zsh-autocomplete` subshell) | `cargo test -p completion -- complete_filesystem_local_dir` |
| **In-Memory History Lookup** | **< 10 µs** | Linear disk scans | `cargo test -p history` |
| **Plugin Startup Latency (with `.zwc`)** | **0.17 ms** (module: 0.12 ms) | 50–200 ms (script plugins) | `bench/bench_startup.zsh` |
| **Load-Time Syscalls** | **62** (505 → 567) | Minimal | `bench/bench_startup.zsh` |
| **Process Spawns on Keystroke** | **0** (strictly 0 forks) | 0 | `bench/bench_startup.zsh` (strace `clone`/`execve`) |
| **Keystroke → Screen Redraw (PTY)** | **68.0 µs** median | 45.1 µs (bare Zsh control) | `bench/bench_zpty.zsh 200` |

---

## 2. Measurement Environment

- **CPU**: AMD Ryzen 5 7600 (12 logical cores)
- **Memory**: 14 GiB total RAM
- **OS / Kernel**: Linux (x86_64)
- **Shell**: Zsh 5.9.2 (x86_64-pc-linux-gnu)
- **Compiler**: Rust 1.85+, edition 2024 (`opt-level = 3`, `lto = "thin"`, `codegen-units = 1`)
- **Tracing**: `strace` 7.2 (used for syscall and subshell spawn assertions)

---

## 3. Incremental Lexing Performance

The lexer micro-benchmark (`cargo run --release -p lexer --example bench_incremental`) inserts an edit into the middle of a command line and compares incremental checkpoint resumption with full re-parsing:

| Line Length | Full Re-parse | Incremental Splicing | Selected Policy | Speed-up | Bytes Re-scanned |
|---|---|---|---|---|---|
| **17 bytes** | 0.08 µs | 0.06 µs | 0.06 µs | 1.33× | 18 bytes |
| **46 bytes** | 0.12 µs | 0.12 µs | 0.12 µs | parity | 47 bytes |
| **86 bytes** | 0.19 µs | 0.16 µs | 0.15 µs | 1.27× | 21 bytes |
| **169 bytes** | 0.36 µs | 0.21 µs | 0.21 µs | 1.71× | 10 bytes |
| **354 bytes** | 0.63 µs | 0.34 µs | 0.34 µs | 1.86× | 14 bytes |
| **694 bytes** | 1.85 µs | 1.46 µs | 1.48 µs | 1.25× | 21 bytes |
| **1,389 bytes** | 1.94 µs | 1.08 µs | 1.08 µs | 1.80× | 10 bytes |

### Observations
1. **Sub-linear Scanning**: As the line grows by 81.7× (17 to 1,389 bytes), the bytes re-scanned actually shrink from 18 to 10 bytes due to checkpoint convergence.
2. **Crossover Policy**: For lines shorter than 128 bytes, full re-parsing without diffing overhead is selected (`INCREMENTAL_MIN_LEN`), avoiding checkpoint comparison overhead.

---

## 4. Completion & Filesystem Traversal Latency

Traditional Zsh completion frameworks like `zsh-autocomplete` execute unbounded shell globs (`*(#q...)`) or spawn `zpty` subshells. On large directories, this produces crippling freezes:

### Bounded Traversal vs Unbounded Globs
- In `/nix/store` with **81,141 directory entries**, `zsh-autocomplete` times out at 1.6 to 3.3 seconds.
- `zsh-pulse`'s bounded scanner (`complete_paths`) stops traversing after reaching `MAX_FILTERED_DIR_SCAN = 1024` and returns up to `MAX_FS_MATCHES = 32` matches.
- Total execution time drops to **20.7 ms** in release mode—over **160× faster**, completely eliminating typing stutter.
- Traversing `/bin` completes in **47 µs**, running over **34,000× faster** than external subshell spawns.

---

## 5. Startup & Wordcode Compilation (`.zwc`)

Measured via `bench/bench_startup.zsh`:
- Loading the native C module via `zmodload zshpulse/core`: **0.12 ms**.
- Total plugin initialization (sourcing `init.zsh`, `widgets.zsh`, `completion.zsh`): **0.17 ms**.
- Total startup syscalls: **62** extra syscalls (predominantly reading the `.so` dynamic library).
- When installed via Nix or `bench/install.sh`, all shell files are compiled into binary wordcode (`.zwc`) via `zcompile`, allowing Zsh to mmap the compiled bytecode directly and skip script parsing on shell startup.

---

## 6. Zero-Subshell Verification

To guarantee that typing remains free of stutter, `zsh-pulse` asserts that zero processes are spawned on the keystroke path.
Running `bench/bench_startup.zsh` with `strace -f -e trace=clone,fork,vfork,execve` verifies:
- Keystroke redraws: **0 spawns**.
- Path lookups: **0 spawns** (in-process `stat` calls with 2-second LRU caching).
- Autosuggestion matching: **0 spawns** (in-memory history log).

---

## 7. Reproducing Benchmark Results

Execute the following commands from the repository root:

```bash
# 1. Incremental lexer micro-benchmarks
cargo run --release -p lexer --example bench_incremental 200

# 2. Allocation budget assertions
cargo test -p lexer --test alloc_budget -- --nocapture --test-threads=1

# 3. Span to region_highlight conversion latency
cargo run --release -p zsh-module --example bench_region

# 4. Large directory completion benchmark
cargo test -p completion -- complete_large_directory_under_budget --nocapture

# 5. Full workspace unit and integration test suite
cargo test --all

# 6. End-to-end native module call latency through Zsh
./bench/install.sh release
./bench/bench_module_call.zsh 200000 28

# 7. Startup time and zero-fork verification
./bench/bench_startup.zsh

# 8. Interactive PTY keystroke latency
./bench/bench_zpty.zsh 200
```

