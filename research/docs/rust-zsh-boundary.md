# The Rust↔Zsh boundary and ownership rules — `shell-pulse`

Everything in this document is about one thing: **which side of the
Rust↔Zsh boundary owns which byte of memory, and what must be true for the
user's shell not to be corrupted.**

Companion documents: [architecture](architecture.md) ·
[performance](performance.md) ·
[ZLE integration](zle-integration.md) ·
[known limitations](known-limitations.md)

---

## 0. THE RULE

> ### If zsh will free it, it must come from `zalloc` / `ztrdup`.
>
> * The **array** that is handed to `setaparam` must be allocated with
>   **`zalloc`**.
> * Every **element** of that array must be allocated with **`ztrdup`**.
> * **Never** `zhalloc`. **Never** `dupstring`. **Never** Rust's allocator.
> **Never** a stack array. **Never** a pointer into a `Vec<u8>`.
>
> `zhalloc` and `dupstring` allocate from zsh's **pool**, and pool memory is
> rewound and recycled by `popheap()`
> ([`zsh-src/Src/mem.c:443`](../upstream/zsh-src/Src/mem.c#L443)), which zsh
> runs around every command
> ([`zsh-src/Src/exec.c:1275`](../upstream/zsh-src/Src/exec.c#L1275)). A
> parameter holding pool memory points at recycled memory the moment the
> next command runs. Getting this wrong produced a `SIGSEGV` inside
> `sepjoin` and a glibc `free(): invalid size` abort.
>
> The rule is enforced in three places:
>
> 1. The capability probe **requires** `zalloc` and `ztrdup`
>    ([`crates/zsh-abi/src/probe.rs:25-32`](../../crates/zsh-abi/src/probe.rs#L25))
>    and a unit test asserts that the pool allocators are **not** in the
>    required set
>    ([`crates/zsh-abi/src/probe.rs:117-125`](../../crates/zsh-abi/src/probe.rs#L117)).
> 2. The only two places that allocate are
>    [`Api::set_param_string`](../../crates/zsh-abi/src/api.rs#L278) and
>    [`Api::set_param_array`](../../crates/zsh-abi/src/api.rs#L313), and
>    both use `zalloc` + `ztrdup` and nothing else.
> 3. Two subprocess tests write the same parameter 25 and 200 times, run a
>    command in between (which is what triggers zsh's pushheap/popheap pair)
>    and read the value back.
>
> zsh's own idiom is identical:
> `setaparam("signals", sigptr = zalloc((TRAPCOUNT + 1) * sizeof(char *)))`
> followed by `*sigptr++ = ztrdup_metafy(*t++)`
> ([`zsh-src/Src/params.c:1013-1017`](../upstream/zsh-src/Src/params.c#L1013)).

---

## 1. The shape of the boundary

```
   zsh process
   ┌────────────────────────────────────────────────────────────┐
   │                                                            │
   │   zsh's own storage                                       │
   │   ┌──────────────┐        ┌───────────────────┐            │
   │   │ param table  │        │ static unmetafied │            │
   │   │ (BUFFER,     │        │ buffer (one for    │            │
   │   │  CURSOR,     │        │  getsparam_u)     │            │
   │   │  region_     │        └───────────────────┘            │
   │   │  highlight)  │              ▲                         │
   │   └──────┬───────┘              │ read-only, copy out      │
   │          │                      │ immediately              │
   │          │ setaparam            │                         │
   │          ▼                      │                         │
   │   ┌──────────────┐              │                         │
   │   │ region_      │              │                         │
   │   │ highlights[]  │◄─────────────┘ (the display array)   │
   │   │ (C array)    │                                        │
   │   └──────────────┘                                        │
   │                                                            │
   │   zsh's exported symbols ─────────┐                       │
   │   (dlsym(RTLD_DEFAULT, …))         │                       │
   └────────────────────────────────────┼───────────────────────┘
                                        │
   ┌────────────────────────────────────┼───────────────────────┐
   │  shellpulse/highlight.so           ▼                       │
   │                                                            │
   │   zsh-abi:  Api { addbuiltins, getsparam, getsparam_u,     │
   │                  setaparam, zalloc, ztrdup }               │
   │                                                            │
   │   zsh-module:  builtin handler, Highlighter, LexCache      │
   └────────────────────────────────────────────────────────────┘
```

Two directions, and the rules are different in each:

| Direction | Rule |
|---|---|
| **zsh → Rust** | Copy the bytes out **immediately**. Never free them. Never keep the pointer. |
| **Rust → zsh** | If zsh will free it, it must be `zalloc`/`ztrdup`. If zsh will only read it, it must live at least as long as zsh holds the pointer. |

---

## 2. Every zsh symbol the module resolves

All six are resolved with `dlsym(RTLD_DEFAULT, …)` during `setup_`, before
any other zsh API is touched
([`crates/zsh-abi/src/probe.rs:72-84`](../../crates/zsh-abi/src/probe.rs#L72),
[`crates/zsh-abi/src/api.rs:203-222`](../../crates/zsh-abi/src/api.rs#L203)).
**There is no link-time dependency on zsh at all** — the `zsh-abi` crate
links and unit-tests normally in a non-zsh process.

| # | Symbol | Signature (as zsh declares it) | zsh source | Rust type | Used for |
|---|---|---|---|---|---|
| 1 | `addbuiltins` | `int addbuiltins(char const *nam, Builtin binl, int size)` | [`Src/module.c:543`](../upstream/zsh-src/Src/module.c#L543) | `AddBuiltinsFn` | registering the `shell-pulse-highlight` builtin |
| 2 | `getsparam` | `char *getsparam(char *s)` | [`Src/params.c:3037`](../upstream/zsh-src/Src/params.c#L3037) | `GetSparamFn` | asking whether a parameter exists (`Api::param_exists`) |
| 3 | `getsparam_u` | `char *getsparam_u(char *s)` | [`Src/params.c:3049`](../upstream/zsh-src/Src/params.c#L3049) | `GetSparamFn` | reading `BUFFER` and `CURSOR` unmetafied |
| 4 | `setaparam` | `Param setaparam(char *s, char **aval)` | [`Src/params.c:3556`](../upstream/zsh-src/Src/params.c#L3556) | `SetaparamFn` | writing `region_highlight` |
| 5 | `zalloc` | `void *zalloc(size_t size)` | [`Src/mem.c:959`](../upstream/zsh-src/Src/mem.c#L959) | `ZAllocFn` | allocating the value array |
| 6 | `ztrdup` | `char *ztrdup(const char *s)` | [`Src/string.c:62`](../upstream/zsh-src/Src/string.c#L62) | `ZTrdupFn` | allocating each element of the array |

### The two that are *not* resolved, deliberately

| Symbol | Why not |
|---|---|
| `zhalloc` | allocates from zsh's **pool**; memory is recycled by `popheap()` ([`Src/mem.c:577`](../upstream/zsh-src/Src/mem.c#L577)). Never legal for a parameter value. |
| `dupstring` | `zalloc`? No — it is `zhalloc` + `strcpy` ([`Src/string.c:33`](../upstream/zsh-src/Src/string.c#L33)). Superficially identical to `ztrdup`, and **wrong**. |

A unit test asserts their absence from the required set
([`crates/zsh-abi/src/probe.rs:117-125`](../../crates/zsh-abi/src/probe.rs#L117)),
so nobody can add one by accident.

---

## 3. Every call the module makes

| # | Call | Made from | Arguments | What happens to the arguments |
|---|---|---|---|---|
| 1 | `dlsym(RTLD_DEFAULT, name)` | `probe.rs:76`, `api.rs:59`, `raw.rs:171` | a NUL-terminated symbol name | the name is a Rust `CString`, freed by Rust; the **result** is a function address that is never freed and never owned by anybody |
| 2 | `addbuiltins(name, table, 1)` | `zsh-module/src/lib.rs:2489` | a `&'static CStr` name and `&mut [Builtin; 2]` | **zsh keeps the pointer.** See §5. |
| 3 | `getsparam(name)` | `api.rs:238`, `:274` | a `&'static CStr` name | returns a pointer **into zsh's own storage**; copied out at once, never freed by us |
| 4 | `getsparam_u(name)` | `api.rs:344`, `:372` | a `&'static CStr` name | returns a pointer to a **static buffer owned by zsh**; copied out at once, never freed by us |
| 5 | `zalloc(size)` | `api.rs:470`, `:504` | a byte count | the result is **owned by zsh** once it is handed to `setaparam` |
| 6 | `ztrdup(string)` | `api.rs:479`, `:514` | a NUL-terminated string | the result is **owned by zsh** once it is handed to `setaparam` |
| 7 | `setaparam(name, array)` | `api.rs:487`, `:520` | a name and a `zalloc`'d, NUL-terminated array | **takes ownership of the array and of every element** |

### The parameters that cross

| Parameter | Direction | When | Notes |
|---|---|---|---|
| `BUFFER` | read | every keystroke | via `getsparam_u`; unmetafied bytes, copied into a reused `Vec<u8>` |
| `CURSOR` | read | every keystroke | via `getsparam_u`, then parsed to an `i64` by `parse_i64`; **no allocation** |
| `region_highlight` | read (existence only) | every keystroke | `getsparam` is used only to ask "does this parameter exist?" |
| `region_highlight` | **written** | when the styling changed | one `setaparam` per change; **nothing at all** when it did not |
| `SHELL_PULSE_STATE` | written | only when `SHELL_PULSE_ECHO` is set | a debugging aid, off by default |

### The argument vector

The builtin's arguments cross the boundary too, and they cross in the
*other* direction: **zsh owns them**.

zsh calls the handler through a plain C function pointer
([`zsh-src/Src/zsh.h:1436`](../upstream/zsh-src/Src/zsh.h#L1436)); the
`char **` vector is NUL-terminated and does **not** include the command
name. The module bounds its scan rather than trusting the terminator alone
(§8.3).

Two of those arguments are the shell's answers about the command words of
the line:

| Argument | Meaning |
|---|---|
| `--command-known=<word>` | the shell knows `<word>` as a command |
| `--command-unknown=<word>` | the shell does not know `<word>` as a command |

The word follows the `=` so that a word containing a space is still one
argument, and both arguments may be repeated — one answer for each command
word of the line. The protocol itself is described in
[ZLE integration §1.5](zle-integration.md).

**The module copies the answers into its own scratch.** The strings in the
argument vector belong to zsh: they are valid only for the duration of the
call, and the memory behind them is reused by the next call. A pointer into
that vector would dangle the moment the builtin returned, so the module
reads the bytes with `CStr::from_ptr` and copies them into a buffer of its
own — one buffer for the words, back to back, and one table of answers that
refer to ranges in it
([`crates/zsh-module/src/lib.rs`](../../crates/zsh-module/src/lib.rs),
`CommandAnswers`). The buffers are reused between keystrokes, so a keystroke
does not allocate once they are warm.

**The copy is bounded, in the number of answers and in their bytes.** The
number of arguments the module will look at is bounded
(`MAX_ARGUMENTS`, 64), and so is the number of bytes the words may take
together (`MAX_ANSWER_BYTES`, 4 KiB — the same bound as the line the module
is willing to highlight at all)
([`crates/zsh-module/src/lib.rs:1650`](../../crates/zsh-module/src/lib.rs#L1650)).
The words come from the line, and the line may be long: a pasted line of a
megabyte would have been copied into the module's own buffer — and kept
there for the rest of the session, because the buffer's capacity is
deliberately reused. An answer that does not fit is dropped, and the command
word it was about is left alone: a *missing* colour, never a wrong one.

The word is copied **byte for byte, with its quoting preserved**, and the
comparison with the word the lexer found is a comparison of bytes — not of
anything zsh owns. The module therefore never points at zsh's memory while
it is deciding what to publish, and it never keeps a pointer into the
argument vector across the call. See rule 3 in §7.

The arguments are read, never written, and never freed: zsh owns them for
the whole call.

---

## 4. Every allocation that crosses the boundary, and who frees it

| Allocation | Made by | Size | Freed by | When |
|---|---|---|---|---|
| the value array (the `char **` that `setaparam` receives) | `zalloc` | `sizeof(char *) * (rows + 1)` | **zsh**, with `free()` via `freearray` | when the parameter's value is replaced |
| each element (one region row) | `ztrdup` | the row's length + 1 | **zsh**, with `zsfree()` via `freearray` | with the array |
| the `Builtin` table | **Rust**, as a `static mut` | 2 × 64 bytes | never — zsh holds the pointer for the life of the module | — |
| the builtin's name string | **Rust**, as a `static CStr` | 19 bytes + NUL | never — zsh holds the pointer | — |
| the `BUFFER` bytes | **zsh** (static buffer) | — | never by us; we copy them | — |
| the `CURSOR` bytes | **zsh** (static buffer) | — | never by us; we parse them | — |
| the argument strings of the builtin | **zsh** | — | never by us; the command words are **copied** into the module's own scratch — at most 4 KiB of them — and compared there | — |

### The exact free path

`setaparam` → `assignaparam`
([`zsh-src/Src/params.c:3318`](../upstream/zsh-src/Src/params.c#L3318)) →
`setarrvalue`
([`zsh-src/Src/params.c:2903`](../upstream/zsh-src/Src/params.c#L2903)) →
the previous value is released with `freearray`
([`zsh-src/Src/utils.c:4121-4131`](../upstream/zsh-src/Src/utils.c#L4121)):

```c
mod_export void
freearray(char **s)
{
    char **t = s;

    DPUTS(!s, "freearray() with zero argument");

    while (*s)
	zsfree(*s++);
    free(t);
}
```

Two facts follow, and they are the whole rule:

* the **array** is released with `free()` → it must be plain `malloc` memory →
  `zalloc` is `malloc` + zsh's out-of-memory handling
  ([`zsh-src/Src/mem.c:959-975`](../upstream/zsh-src/Src/mem.c#L959));
* each **element** is released with `zsfree()` → in a zsh built without
  `ZSH_MEM` that is `free()`
  ([`zsh-src/Src/mem.c:1876-1879`](../upstream/zsh-src/Src/mem.c#L1876)) →
  the elements must also be plain `malloc` memory → `ztrdup` is
  `zalloc` + `strcpy`
  ([`zsh-src/Src/string.c:62-71`](../upstream/zsh-src/Src/string.c#L62)).

> **The rule is about *which allocator*, not about *which memory*.** In a zsh
> built with `ZSH_MEM`, zsh replaces `malloc` and `free` with its own
> allocator ([`zsh-src/Src/mem.c:1189`](../upstream/zsh-src/Src/mem.c#L1189),
> [`:1630-1633`](../upstream/zsh-src/Src/mem.c#L1630)) and `zsfree` becomes
> `zfree(p, strlen(p) + 1)`
> ([`:1641-1644`](../upstream/zsh-src/Src/mem.c#L1641)). `zalloc` calls
> `malloc`, so it returns memory from *that* allocator, and `freearray`
> releases it with the matching `free`/`zsfree`. The `zalloc`/`ztrdup` idiom
> is therefore correct in **both** builds.
>
> The corollary is the part that matters: memory that came from **Rust's**
> allocator must never be handed to zsh to free. In a `ZSH_MEM` build zsh's
> `free` is not libc's `free` at all, and because zsh exports its `malloc`
> and `free` into the global symbol scope, a module's own call to `malloc`
> may resolve to zsh's override. **No `ZSH_MEM` build has been tested
> here**; see [known limitations](known-limitations.md).

### The same rule applies to `region_highlight`

The ZLE special parameter's setter ends with `freearray(av)`
([`zsh-src/Src/Zle/zle_refresh.c:586`](../upstream/zsh-src/Src/Zle/zle_refresh.c#L586)),
so it takes ownership in exactly the same way. The module therefore uses
`Api::set_param_array`, which allocates the array with `zalloc` and every row
with `ztrdup` — never anything else.

---

## 5. The `Builtin` table: zsh keeps the pointer

This one is easy to get wrong, because `addbuiltin` takes its argument **by
value** and it is not obvious that the *pointer* survives.

```c
/* zsh-src/Src/module.c:408-417 */
static int
addbuiltin(Builtin b)
{
    Builtin bn = (Builtin) builtintab->getnode2(builtintab, b->node.nam);
    if (bn && (bn->node.flags & BINF_ADDED))
	return 1;
    if (bn)
	builtintab->freenode(builtintab->removenode(builtintab, b->node.nam));
    builtintab->addnode(builtintab, b->node.nam, b);
    return 0;
}
```

`Builtin` is a **pointer** type — `typedef struct builtin *Builtin`
([`zsh-src/Src/zsh.h:513`](../upstream/zsh-src/Src/zsh.h#L513)) — and
`addbuiltins` passes `&binl[n]`
([`zsh-src/Src/module.c:548`](../upstream/zsh-src/Src/module.c#L548)). The
hash table then stores that pointer
([`zsh-src/Src/hashtable.c:170-171`](../upstream/zsh-src/Src/hashtable.c#L174)).

**Consequences, all of which the module satisfies:**

1. The `struct builtin` array must **outlive the shell's use of it**. The
   module's table is a `static mut`
   ([`crates/zsh-module/src/lib.rs:1720-1747`](../../crates/zsh-module/src/lib.rs#L1720)),
   so it lives in the `.bss` of the cdylib for as long as the module is
   loaded.
2. The **name string** is not copied either: `hn->nam = nam`
   ([`zsh-src/Src/hashtable.c:171`](../upstream/zsh-src/Src/hashtable.c#L175))
   stores the pointer. The module's name is a `static CStr`
   ([`crates/zsh-module/src/lib.rs:1712`](../../crates/zsh-module/src/lib.rs#L1712)),
   so it lives just as long.
3. **zsh writes into the table**: `addbuiltins` sets
   `b->node.flags |= BINF_ADDED`
   ([`zsh-src/Src/module.c:555`](../upstream/zsh-src/Src/module.c#L557)) and
   `addhashnode2` writes `hn->nam`. That is why the table is `static mut` and
   why the module reads it back through `addr_of_mut!` rather than through a
   shared reference
   ([`crates/zsh-module/src/lib.rs:1758`](../../crates/zsh-module/src/lib.rs#L1758)).
4. The name zsh registers is `b->node.nam` **verbatim**. The `nam` argument
   of `addbuiltins` is only used in diagnostics
   ([`zsh-src/Src/module.c:552`](../upstream/zsh-src/Src/module.c#L553)). The
   module therefore registers the full name `shell-pulse-highlight` in
   `node.nam` and passes the same string as the diagnostic name.
5. The table must be **NULL-terminated**: `addbuiltins` scans `binl[n]` for
   `n < size`, and the module's own table has a second, all-zero entry as a
   terminator so that a mistake in the `size` argument cannot run off the
   end of the array. The test
   `builtin_table_is_null_terminated` asserts it.

---

## 6. The bugs that produced the rules

### 6.1 A Rust `&str` is not NUL-terminated

A Rust `&'static str` is a `(pointer, length)` pair. Handing one to zsh as a
`char *` makes zsh read past the end into whatever follows in `.rodata`.
During development this produced a builtin literally named
`shell-pulse-SHELL_PULSE_DEBUG` and corrupted zsh's builtin table.

The fix is structural: `CStr` only accepts C string literals
([`crates/zsh-abi/src/raw.rs:81-100`](../../crates/zsh-abi/src/raw.rs#L81)),
which Rust guarantees are NUL-terminated, and a test asserts the trailing
NUL really is there
([`crates/zsh-abi/src/raw.rs:236-245`](../../crates/zsh-abi/src/raw.rs#L236)).

**Rule: every string handed to zsh is a `c"…"` literal, never a `&str`.**

### 6.2 `setaparam` takes ownership

`assignaparam` hands the value array straight to `setarrvalue`, which stores
it in the parameter and releases the *previous* value with `freearray`.
Passing a Rust stack array segfaulted zsh: the values must be allocated by
`zalloc`/`ztrdup`.

### 6.3 The pool allocators look right and are not

`zhalloc` and `dupstring` are the obvious choices — they are what zsh's own
code uses everywhere — and they are wrong, because pool memory is recycled
by `popheap()` around every command. The observable failure was a `SIGSEGV`
inside `sepjoin` while reading the parameter back, and separately a glibc
`free(): invalid size` abort.

The Stage A research recorded the same class of bug from the other side: a
hand-transcribed `getsparam()` that was declared as returning `char **` when
zsh returns `char *` produced a pointer into the middle of the buffer's
bytes and **segfaulted zsh**
([`../upstream/PERFORMANCE_FAILURES.md` §7](../upstream/PERFORMANCE_FAILURES.md)).

### 6.4 `setaparam`'s return value is meaningless

`setaparam` is declared to return `int` but returns a `Param` pointer
truncated to `int`
([`zsh-src/Src/params.c:3556-3559`](../upstream/zsh-src/Src/params.c#L3556)),
so it always looks like garbage. The module ignores it
([`crates/zsh-abi/src/api.rs:487`](../../crates/zsh-abi/src/api.rs#L487)).

**Rule: never test the return value of `setaparam`.**

---

## 7. The rules, numbered

1. **Probe before use.** Resolve every required symbol during `setup_`. A
   missing symbol is a clean load failure (`setup_` returns −1, which makes
   zsh abort the load), never a later crash.
2. **Never unwind into zsh.** Every entry point runs inside `guard()`
   ([`crates/zsh-abi/src/guard.rs:47-58`](../../crates/zsh-abi/src/guard.rs#L47)).
3. **Never free anything zsh gave us.** zsh's parameter values, the static
   unmetafied buffer, the builtin's argument strings — none of them are ours.
   What the module needs from the argument strings is *copied* into its own
   scratch before it is used.
4. **Never hand zsh Rust-owned memory** in a parameter that zsh will take
   ownership of.
5. **If zsh will free it, it came from `zalloc` / `ztrdup`.** See §0.
6. **Never use the pool allocators.** `zhalloc`, `dupstring`, `hcalloc`.
7. **Copy out of zsh's static buffers immediately.** `getsparam_u` returns a
   pointer to a static buffer
   ([`zsh-src/Src/params.c:3053`](../upstream/zsh-src/Src/params.c#L3053));
   the next parameter read overwrites it. `CURSOR` is read before `BUFFER`
   for exactly this reason
   ([`crates/zsh-module/src/lib.rs:1510-1515`](../../crates/zsh-module/src/lib.rs#L1510)).
8. **Every string handed to zsh is NUL-terminated.** Use `c"…"` literals.
9. **Never test `setaparam`'s return value.**
10. **Anything zsh keeps a pointer to must outlive it.** The `Builtin` table
    and the builtin's name are `static` for that reason.
11. **Never keep a pointer into the argument vector.** The strings in it
    belong to zsh and are only valid for the duration of the call. Copy the
    bytes you need into your own scratch, and compare the copy.

---

## 8. The module ABI

### 8.1 The entry points

zsh looks up six symbols
([`zsh-src/Src/module.c:1554-1566`](../upstream/zsh-src/Src/module.c#L1554)):

| Symbol | Required? | Does this module export it? | What it does here |
|---|---|---|---|
| `setup_` | **yes** — zsh warns "no setup function" and fails the load ([`module.c:1806-1812`](../upstream/zsh-src/Src/module.c#L1806)) | yes | probes zsh, registers the builtin, returns −1 to refuse the load |
| `features_` | optional ([`module.c:1816-1823`](../upstream/zsh-src/Src/module.c#L1816)) | **no** | the module has no optional features |
| `enables_` | optional ([`module.c:1829-1835`](../upstream/zsh-src/Src/module.c#L1829)) | **no** | — |
| `boot_` | **yes** ([`module.c:1841-1847`](../upstream/zsh-src/Src/module.c#L1841)) | yes | starts the worker thread and round-trips one job, so a broken worker is found at load time |
| `cleanup_` | **yes** ([`module.c:1853-1859`](../upstream/zsh-src/Src/module.c#L1853)) | yes | nothing yet |
| `finish_` | **yes** ([`module.c:1868-1878`](../upstream/zsh-src/Src/module.c#L1868)) | yes | nothing yet |

`features_` and `enables_` are omitted **deliberately**. An `enables_` that
does not fill `*enables` crashes zsh, because zsh walks `enables` for
`arrlen(features)` entries
([`../upstream/PERFORMANCE_FAILURES.md` §7](../upstream/PERFORMANCE_FAILURES.md)).
Not exporting the symbol at all is the safe way to say "no optional
features".

### 8.2 Both symbol forms

With `DYNAMIC_NAME_CLASH_OK` (this zsh's build), zsh calls
`dlsym(handle, "setup_")` **unmangled**
([`zsh-src/Src/module.c:1774-1777`](../upstream/zsh-src/Src/module.c#L1774)).
Otherwise it builds `<name>` with `/` → `Qs` and `_` → `Qu`
([`zsh-src/Src/module.c:1780-1798`](../upstream/zsh-src/Src/module.c#L1780)),
which for `shellpulse/highlight` is `setup_shellpulseQshighlight`.

The module exports **both** forms, so one build works on either kind of zsh
([`crates/zsh-module/src/lib.rs:2560-2584`](../../crates/zsh-module/src/lib.rs#L2560)).
The one the host does not use is simply ignored.

### 8.3 The builtin handler signature

```c
typedef int (*HandlerFunc) (char *, char **, Options, int);   /* zsh.h:1436 */
```

`Options` is `typedef struct options *Options`
([`zsh-src/Src/zsh.h:538`](../upstream/zsh-src/Src/zsh.h#L538)); the module
declares it as `*mut c_char`
([`crates/zsh-abi/src/raw.rs:26`](../../crates/zsh-abi/src/raw.rs#L26)), which
is layout-compatible because both are pointers. The module never
dereferences it.

The `char **args` vector is NUL-terminated and does **not** include the
command name. The module bounds its scan at 64 arguments
([`crates/zsh-module/src/lib.rs:1637`](../../crates/zsh-module/src/lib.rs#L1637))
rather than trusting the terminator alone. The bytes the answers carry are
bounded as well — see §3.

### 8.4 A public entry point that does not touch zsh

`published_spans` is a public function of the `zsh-module` crate
([`crates/zsh-module/src/lib.rs:747`](../../crates/zsh-module/src/lib.rs#L747)):

```rust
pub fn published_spans(text: &[u8], answers: &[&[u8]]) -> Vec<HighlightSpan>
```

It is **not** a zsh entry point: it is not in the table in §8.1, zsh never
calls it, and it touches no zsh API at all. It lexes the text and styles it
from the answers it is handed. The paths are not looked up, because that
needs the shell's directory and there is no shell here — so a word that
names a file is styled the way the lexer styled it.

It exists so that the benchmark and the tests can inspect **the real
thing**: the spans the module publishes, rather than a stand-in built from
the lexer's tokens. The benchmark used to build its own spans, and the
number of regions it reported was therefore not the number of regions a
real line gets. The crate is built as a `cdylib` *and* an `rlib`
([`crates/zsh-module/Cargo.toml`](../../crates/zsh-module/Cargo.toml)) for
exactly this reason: a `cdylib` alone cannot be linked into an example.

---

## 9. Layout assumptions, and the tests that hold them

| Assumption | zsh source | Asserted by |
|---|---|---|
| `struct hashnode` is 24 bytes, align 8 | [`zsh.h:1226-1230`](../upstream/zsh-src/Src/zsh.h#L1226) | `hashnode_layout_matches_zsh` |
| `struct builtin` is 64 bytes, align 8 | [`zsh.h:1440-1448`](../upstream/zsh-src/Src/zsh.h#L1440) | `builtin_layout_matches_zsh` |
| field offsets: `node.nam` 8, `handlerfunc` 24, `minargs` 32, `maxargs` 36, `funcid` 40, `optstr` 48, `defopts` 56 | as above | `field_offsets_match_zsh` |
| `Options` is a pointer | [`zsh.h:538`](../upstream/zsh-src/Src/zsh.h#L538) | by construction |
| the module name is a legal zsh module name | [`module.c:2177-2186`](../upstream/zsh-src/Src/module.c#L2177) | `zmodload_name_is_a_legal_zsh_module_name` |
| the builtin name contains no illegal characters | — | `builtin_prefix_has_no_illegal_characters` |

The layout tests are in
[`crates/zsh-abi/src/raw.rs:176-253`](../../crates/zsh-abi/src/raw.rs#L176).
They are hard-coded to the sizes observed on x86_64 Linux against zsh 5.9.2.
**A different architecture or a different zsh would need them re-measured** —
they are a tripwire, not a proof.

---

## 10. Panic containment

zsh calls the entry points through plain C function pointers. A Rust panic
must never propagate across that boundary: unwinding into zsh's C frames is
undefined behaviour, and in practice it aborts the user's interactive shell.

Every entry point therefore runs inside `guard()`
([`crates/zsh-abi/src/guard.rs:47-58`](../../crates/zsh-abi/src/guard.rs#L47)),
which catches the panic, sets a sticky "degraded" flag and returns a benign
value. The builtin then returns 0 immediately on every subsequent call
([`crates/zsh-module/src/lib.rs:1472-1475`](../../crates/zsh-module/src/lib.rs#L1472)),
so the shell stays usable.

The workspace is built with `panic = "unwind"` on purpose
([`Cargo.toml`](../../Cargo.toml)); abort-on-panic would make the mechanism
ineffective. The guard catches *unwinding* panics only.

---

## 11. Checklist for anyone adding a new zsh call

1. **Does the symbol exist in every zsh we care about?** Add it to
   `REQUIRED_SYMBOLS` in
   [`crates/zsh-abi/src/probe.rs:25-32`](../../crates/zsh-abi/src/probe.rs#L25)
   and to `Api::resolve`
   ([`crates/zsh-abi/src/api.rs:203-222`](../../crates/zsh-abi/src/api.rs#L203)).
   A missing symbol must be a *clean load failure*, not a later crash.
2. **Transcribe the signature from the zsh source**, and cite the file and
   line. Do not guess it: guessing `char **` for `getsparam` segfaulted zsh.
3. **Who owns the memory?**
   - Does zsh take ownership? → `zalloc` for the array, `ztrdup` for the
     elements. Nothing else.
   - Does zsh only read it? → it must outlive zsh's use of it.
   - Does zsh own it? → copy it out immediately and never free it.
4. **Is every string NUL-terminated?** Use `c"…"`, not `&str`.
5. **Is the return value meaningful?** `setaparam`'s is not.
6. **Write a test that runs `zsh` in a subprocess** and asserts on the exit
   status, so an ABI mistake is a failing test rather than a dead shell.
7. **Add the symbol to the layout/ABI tests** if it introduces a struct.

---

## 12. What is not covered

* **No zsh built with `ZSH_MEM` has been tested.** See the caveat in §4.
* **Only x86_64 Linux has been tested.** The layout assertions are
  hard-coded for it.
* **Only zsh 5.9.2 has been tested.** The probe exists precisely because
  other builds export different symbols, but no other build has been run.
* **The `Builtin` table is written by zsh** (`BINF_ADDED`, `node.nam`) and
  read by the module's own tests. The module does not otherwise mutate it
  after `setup_`.
* **The `features_`/`enables_` path is untested**, because the module does
  not implement it.
