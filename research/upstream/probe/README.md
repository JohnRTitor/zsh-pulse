# Native zsh module proof-of-concept

Establishes that a Rust `cdylib` can serve as a zsh loadable module against a
**stock** zsh, with no zsh headers and no zsh source tree at build time.

`module_poc.c` is hand-declared C (structs and symbols transcribed from
zsh 5.9.2 `Src/zsh.h`, `Src/module.c`, `Src/params.c`). A Rust `cdylib` needs
exactly the same `#[no_mangle] extern "C"` exports; the ABI does not change.

## Reproduce

```sh
cc -shared -fPIC -O2 -o pulse.so module_poc.c
mkdir -p mp && cp pulse.so mp/
zsh -fc 'module_path=(mp $module_path); zmodload p5; BUFFER="git commit"; CURSOR=10; p5-hl'
```

Expected: `pulse BUFFER=[git commit] CURSOR=[10]`.

## Findings encoded here

1. Entry symbols are `setup_` / `features_` / `enables_` / `boot_` /
   `cleanup_` / `finish_` — **unmangled** on builds that define
   `DYNAMIC_NAME_CLASH_OK` (`Src/module.c:1774-1777`). The `setup_<name>` form
   is silently ignored.
2. `zmodload` rejects absolute paths, `.so` suffixes, and dot-prefixed names
   (`modname_ok`, `Src/module.c:2177-2186`). Use `mymod/probe` -> `mymod/probe.so`.
3. `addbuiltins(char const *, Builtin *, int)` — three arguments.
4. `getsparam()` returns `char *`, **not** `char **` (`Src/params.c:3037`).
   Declaring it wrongly segfaults zsh.
5. An `enables_` that does not fill `*enables` segfaults zsh, because
   `Src/module.c:2113-2117` walks `arrlen(features)` entries.
6. Measured marginal cost of a native call reading `BUFFER`: **~112 ns**
   (200k iterations). See `../PERFORMANCE_COMPARISON.md` §2.
