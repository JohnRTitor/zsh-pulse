# Rust↔Zsh Boundary and Ownership Rules — `zsh-pulse`

Everything in this document is about one thing: **which side of the Rust↔Zsh boundary owns which byte of memory, and what must be true for the shell not to crash or corrupt memory.**

Companion documents: [architecture](architecture.md) ·
[performance](performance.md) ·
[ZLE integration](zle-integration.md) ·
[known limitations](known-limitations.md) ·
[Features catalog](../FEATURES.md) ·
[Milestones archive](archive/tasks-milestones.md)

---

## 0. THE CARDINAL RULE

> ### If Zsh will free it, it MUST come from `zalloc` / `ztrdup`.
>
> - The **array pointer** handed to `setaparam` must be allocated with **`zalloc`**.
> - Every **element** of that array must be allocated with **`ztrdup`**.
> - **Never** `zhalloc`. **Never** `dupstring`. **Never** Rust's `alloc::alloc`.
> - **Never** a stack buffer. **Never** an interior pointer into a Rust `Vec<u8>`.
>
> `zhalloc` and `dupstring` allocate from Zsh's **pool allocator**. Pool memory is rewound and recycled by `popheap()` around every command execution. Pointers to pool memory stored in shell parameters point at recycled memory the moment the next command runs, causing `SIGSEGV` in `sepjoin` and glibc `free(): invalid size` aborts.

---

## 1. Boundary Architecture

```
   Zsh Interactive Shell Process
   ┌─────────────────────────────────────────────────────────────────┐
   │                                                                 │
   │  Zsh Core Storage & Tables                                      │
   │  ┌──────────────┐             ┌───────────────────┐             │
   │  │ param table  │             │ static unmetafied │             │
   │  │ (BUFFER,     │             │ buffer (one for   │             │
   │  │  CURSOR,     │             │  getsparam_u)     │             │
   │  │  region_     │             └───────────────────┘             │
   │  │  highlight)  │                   ▲                           │
   │  └──────┬───────┘                   │ read-only, copy out       │
   │         │                           │ immediately into Vec<u8>  │
   │         │ setaparam                 │                           │
   │         ▼                           │                           │
   │  ┌──────────────┐                   │                           │
   │  │ region_      │                   │                           │
   │  │ highlights[] ├───────────────────┘                           │
   │  │ (C array)    │                                               │
   │  └──────────────┘                                               │
   │                                                                 │
   │  Zsh Exported C Symbols ──────────┐                             │
   │  (localkeymap, menucmp, ...)      │                             │
   └───────────────────────────────────┼─────────────────────────────┘
                                       │ dlsym(RTLD_DEFAULT, ...)
   ┌───────────────────────────────────┼─────────────────────────────┐
   │  zshpulse/core.so (cdylib)        ▼                             │
   │                                                                 │
   │  crates/zsh-abi (FFI Boundary)                                  │
   │  - Api::resolve(probe())                                        │
   │  - Api::is_menu_active()   [reads localkeymap & menucmp]        │
   │  - Api::read_param_bytes() [calls getsparam_u]                  │
   │  - Api::set_param_array()  [calls zalloc, ztrdup, setaparam]    │
   │  - guard::guard()          [catch_unwind panic isolation]       │
   │                                                                 │
   │  crates/zsh-module                                              │
   │  - Builtins: zsh-pulse-highlight, zsh-pulse-complete            │
   │  - Highlighter state, bounded completion, history index         │
   └─────────────────────────────────────────────────────────────────┘
```

### Data Handoff Rules

| Direction | Rule |
|---|---|
| **Zsh → Rust** | Copy bytes out **immediately**. Never free the Zsh pointer. Never retain the raw pointer beyond the function call. |
| **Rust → Zsh** | If Zsh will take ownership, memory must be allocated with `zalloc`/`ztrdup`. If Zsh only reads it (e.g. static builtin tables), it must be `'static` and live for the life of the process. |

---

## 2. Dynamic Symbol Resolution (`dlsym`)

All internal Zsh symbols are resolved at module load time via `dlsym(RTLD_DEFAULT, ...)` in [`crates/zsh-abi/src/probe.rs`](../crates/zsh-abi/src/probe.rs).
**Zero Zsh symbols are linked at compile time.** The `zsh-abi` crate links cleanly and passes unit tests in standard Rust testing environments.

| Symbol | Signature | Rust Function Pointer Type | Responsibility |
|---|---|---|---|
| `addbuiltins` | `int addbuiltins(char const *, Builtin, int)` | `AddBuiltinsFn` | Registers `zsh-pulse-highlight` and `zsh-pulse-complete` builtins. |
| `getsparam` | `char *getsparam(char *)` | `GetSparamFn` | Tests parameter existence (`Api::param_exists`). |
| `getsparam_u` | `char *getsparam_u(char *)` | `GetSparamFn` | Reads `$BUFFER` and `$CURSOR` in unmetafied raw bytes. |
| `setaparam` | `Param setaparam(char *, char **)` | `SetaparamFn` | Publishes formatted `region_highlight` array. |
| `zalloc` | `void *zalloc(size_t)` | `ZAllocFn` | Allocates the C array buffer handed to `setaparam`. |
| `ztrdup` | `char *ztrdup(const char *)` | `ZTrdupFn` | Duplicates each string element in the array handed to Zsh. |
| `localkeymap` | `Keymap` (global pointer) | `*mut c_void` | Detects active local keymaps (`menuselect`). |
| `menucmp` | `int` / `struct menu_info *` | `*mut c_void` | Detects active menu completion structures in `complist`. |

### Deliberately Excluded Symbols
- `zhalloc`: Allocates from Zsh pool memory; recycled by `popheap()`.
- `dupstring`: Equal to `zhalloc` + `strcpy`; never valid for parameter storage.
A unit test in `crates/zsh-abi/src/probe.rs` asserts that pool allocators are **never** present in the required resolution set.

---

## 3. Safe `Api` Method Wrappers

[`crates/zsh-abi/src/api.rs`](../crates/zsh-abi/src/api.rs) wraps raw function pointers in safe Rust interfaces:

### 3.1 `Api::is_menu_active(&self) -> bool`
Safely inspects global pointers `localkeymap` and `menucmp`. If either pointer is non-null, menu completion is actively running. Calling completion or redraw routines during this state is prevented to avoid `SIGSEGV` in `complist.so`.

### 3.2 `Api::read_param_bytes(&self, name: &CStr, out: &mut Vec<u8>) -> bool`
Queries `getsparam_u`, copies the unmetafied bytes into a pre-allocated Rust buffer, and clears trailing delimiters. Once warm, this executes with **zero heap allocations**.

### 3.3 `Api::set_param_array(&self, name: &CStr, rows: &[impl AsRef<str>]) -> Result<(), SetParamError>`
1. Calls `zalloc` for `(rows.len() + 1) * sizeof(char *)`.
2. Iterates over `rows`, calling `ztrdup` for each NUL-terminated row.
3. Sets the terminal null terminator `*tail = null_mut()`.
4. Invokes `setaparam(name, array)`. Ownership of both the array and elements is permanently handed over to Zsh.

---

## 4. The Allocation & Deallocation Lifecycle

```c
// zsh-src/Src/utils.c:4121
mod_export void freearray(char **s) {
    char **t = s;
    while (*s)
        zsfree(*s++);
    free(t);
}
```

When Zsh replaces the value of `region_highlight`, it calls `freearray`:
1. Every element is released via `zsfree()` (which resolves to `free()`). Because elements were allocated with `ztrdup`, this is valid.
2. The outer array pointer is released with `free()`. Because it was allocated with `zalloc` (plain `malloc`), this is valid.

---

## 5. Panic Isolation Boundary (`catch_unwind`)

A Rust panic must **never** unwind across the `extern "C"` boundary into Zsh's C frames, as unwinding across C ABI frames results in undefined behavior and process abortion.

All module entry points (`zsh_pulse_highlight_builtin`, `zsh_pulse_complete_builtin`, `setup_`, `boot_`, `cleanup_`, `finish_`) are wrapped inside `guard::guard`:

```rust
// crates/zsh-abi/src/guard.rs
pub fn guard<F, R>(f: F) -> R
where
    F: FnOnce() -> R + UnwindSafe,
    R: Default,
{
    match std::panic::catch_unwind(f) {
        Ok(result) => result,
        Err(_) => {
            // Panic captured cleanly: returns default error code without crashing Zsh
            R::default()
        }
    }
}
```

This behavior is validated by automated regression tests in `crates/zsh-module/tests/zsh_integration.rs` simulating thousands of panics.

