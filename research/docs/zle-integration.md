# ZLE integration design — `shell-pulse`

How the plugin gets its colours onto the user's screen, and what happens when
it cannot.

Companion documents: [architecture](architecture.md) ·
[performance](performance.md) ·
[Rust↔Zsh boundary](rust-zsh-boundary.md) ·
[known limitations](known-limitations.md)

---

## 1. The integration point

### 1.1 The hook

The plugin installs exactly one ZLE widget: `zle-line-pre-redraw`.

zsh calls it "whenever the input line is about to be redrawn, providing an
opportunity to update the region_highlight array"
([`zsh-src/Doc/Zsh/zle.yo:1164-1168`](../upstream/zsh-src/Doc/Zsh/zle.yo#L1164)).
That is precisely the job.

The call chain in zsh is:

| Step | Where |
|---|---|
| the line editor decides to redraw | [`zsh-src/Src/Zle/zle_main.c:1167`](../upstream/zsh-src/Src/Zle/zle_main.c#L1167) (also `:1360`, `:1984`) |
| `redrawhook()` looks up the widget | [`zsh-src/Src/Zle/zle_main.c:1066`](../upstream/zsh-src/Src/Zle/zle_main.c#L1066) |
| it runs it | [`zsh-src/Src/Zle/zle_main.c:1069`](../upstream/zsh-src/Src/Zle/zle_main.c#L1069), `execzlefunc` at `:1087` |

### 1.2 Why this hook and not another

| Alternative | Why not |
|---|---|
| wrap every widget (what FSH does) | the cost then scales with the user's widget count — 8 278 on the compatibility target ([`../upstream/PERFORMANCE_FAILURES.md` §5](../upstream/PERFORMANCE_FAILURES.md)) |
| a bound key | does not fire on cursor movement, completion or a redraw caused by anything else |
| `zle-line-init` | fires once per line, not per keystroke |
| `zle -F` on a file descriptor | a valid design for *asynchronous* producers; not needed for a synchronous highlighter, and it adds a wakeup path |

The hook is called once per redraw, so the cost is one native call per
redraw, not per widget and not per key.

### 1.3 How it is installed

[`zsh/init.zsh`](../../zsh/init.zsh) is the whole adapter — 63 lines.

```zsh
shell-pulse-highlight-redraw() {
  shell-pulse-highlight --region-highlight
}

zle -N zle-line-pre-redraw shell-pulse-highlight-redraw
```

`zle -N` **replaces** any widget of the same name, and zsh allows exactly one
widget per name. Other plugins use this hook too, so the adapter *chains*
rather than overwrites
([`zsh/init.zsh:51-61`](../../zsh/init.zsh#L51)):

```zsh
if (( ${+widgets[zle-line-pre-redraw]} )); then
  typeset -g _shell_pulse_previous_pre_redraw=${widgets[zle-line-pre-redraw]}
  shell-pulse-highlight-redraw() {
    shell-pulse-highlight --region-highlight
    if (( ${+_shell_pulse_previous_pre_redraw} )); then
      $_shell_pulse_previous_pre_redraw
    fi
  }
fi
```

The previous widget is remembered in a global (`typeset -g`) so that the
redefinition of the function closes over it. The behaviour is asserted by
`the_adapter_chains_an_existing_pre_redraw_hook` in
[`crates/zsh-module/tests/zsh_integration.rs`](../../crates/zsh-module/tests/zsh_integration.rs).

**Known rough edge:** if the user loads the plugin *twice*, the second load
chains the first load's wrapper, so the builtin is called twice per redraw.
The second call finds that nothing changed and returns without touching zsh
(see §4.1), so the cost is one extra native call, not a double highlight.

### 1.4 The `--region-highlight` flag

The builtin takes an optional `--region-highlight` argument. Without it, the
module **reads** the line and changes nothing
([`crates/zsh-module/src/lib.rs:188-194`](../../crates/zsh-module/src/lib.rs#L188)).

This is deliberate. `region_highlight` is a *parameter* in the user's shell as
well as a ZLE special parameter. If the module wrote to it unconditionally, it
would either do nothing (the special parameter does not exist outside a
widget) or — worse — overwrite a global array of that name that the user or
another plugin owns.

Two tests pin this down:

- `the_regions_are_not_published_unless_the_caller_asks` — a global
  `region_highlight=(SENTINEL)` is left untouched.
- `the_highlighter_does_not_create_a_global_region_highlight` — calling the
  builtin with the flag, outside a widget, does not create a global parameter
  (`EXISTS=0`).

---

## 2. `region_highlight`

### 2.1 What zsh expects

`region_highlight` is a **special array parameter** with a custom getter and
setter:

| | |
|---|---|
| declaration | [`zsh-src/Src/Zle/zle_params.c:169`](../upstream/zsh-src/Src/Zle/zle_params.c#L169) — `{ "region_highlight", PM_ARRAY, GSU(region_highlight_gsu), NULL }` |
| the getter/setter pair | [`zsh-src/Src/Zle/zle_params.c:132-133`](../upstream/zsh-src/Src/Zle/zle_params.c#L132) |
| the setter | [`zsh-src/Src/Zle/zle_refresh.c:488-586`](../upstream/zsh-src/Src/Zle/zle_refresh.c#L488) |
| the documentation | [`zsh-src/Doc/Zsh/zle.yo:964-1003`](../upstream/zsh-src/Doc/Zsh/zle.yo#L964) |

Each element is a whitespace-separated string:

```
<start> <end> <highlight-spec> [memo=<token>]
```

| Field | Meaning |
|---|---|
| `start` | "A start offset in the same units as `CURSOR`" ([`zle.yo:980`](../upstream/zsh-src/Doc/Zsh/zle.yo#L980)) |
| `end` | "An end offset in the same units as `CURSOR`" ([`zle.yo:981`](../upstream/zsh-src/Doc/Zsh/zle.yo#L981)); the region is `[start, end)` — `end` is the position of the first character *not* highlighted ([`zsh-src/Src/Zle/zle.h:446-450`](../upstream/zsh-src/Src/Zle/zle.h#L446)) |
| `start`/`end` may be preceded by `P` | means the offsets include the `PREDISPLAY` string ([`zle.yo:977-979`](../upstream/zsh-src/Doc/Zsh/zle.yo#L977)) |
| highlight spec | the same syntax as a `zle_highlight` context ([`zle.yo:982-985`](../upstream/zsh-src/Doc/Zsh/zle.yo#L982)), parsed by `match_highlight()` ([`zsh-src/Src/prompt.c:2030`](../upstream/zsh-src/Src/prompt.c#L2030)) |
| `memo=` | a free-form token, preserved verbatim, that lets a plugin recognise its own entries ([`zle.yo:986-998`](../upstream/zsh-src/Doc/Zsh/zle.yo#L986)), parsed at [`zle_refresh.c:551-581`](../upstream/zsh-src/Src/Zle/zle_refresh.c#L551) |

The colour names are zsh's own
([`zsh-src/Src/prompt.c:1884-1886`](../upstream/zsh-src/Src/prompt.c#L1884)):
`black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`.

### 2.2 What the plugin writes

The attribute for each style is chosen in
[`crates/zsh-module/src/region.rs:40-56`](../../crates/zsh-module/src/region.rs#L40):

| Style | Attribute |
|---|---|
| command | `fg=cyan,bold` |
| assignment | `fg=yellow` |
| single-quoted | `fg=green` |
| double-quoted | `fg=green` |
| escape | `fg=magenta` |
| comment | `fg=blue` |
| operator | `fg=red` |
| redirect | `fg=red` |
| substitution | `fg=magenta` |
| parameter expansion | `fg=magenta` |
| glob | `fg=yellow` |
| here-document body | `fg=green` |
| **ordinary argument** | **none** — the argument is left exactly as the user typed it |

An ordinary argument is deliberately *not* highlighted: a command line where
every word is coloured is harder to read, not easier.

Every published entry carries `memo=shell-pulse`
([`crates/zsh-module/src/region.rs:34`](../../crates/zsh-module/src/region.rs#L34)),
so that another plugin — or a future version of this one — can tell whose
entries they are.

A row therefore looks like:

```
0 4 fg=cyan,bold memo=shell-pulse
```

### 2.3 The rows are built in one reused buffer

[`crates/zsh-module/src/region.rs:63-149`](../../crates/zsh-module/src/region.rs#L63)
keeps the rows NUL-separated in a single `Vec<u8>` that is reused for every
keystroke. A keystroke does not allocate one string per highlighted region,
which on the interactive thread is exactly what must not happen.

The decimal numbers are written by hand
([`crates/zsh-module/src/region.rs:161-173`](../../crates/zsh-module/src/region.rs#L161))
because `format!` would allocate.

### 2.4 The region limit

At most **256** regions are published per push
([`crates/zsh-module/src/region.rs:156`](../../crates/zsh-module/src/region.rs#L156)):

```rust
pub const MAX_ENTRIES: usize = 256;
```

The reason is in the comment, and it is the right reason: zsh parses every
region specification on every redraw
([`zsh-src/Src/Zle/zle_refresh.c:513-586`](../upstream/zsh-src/Src/Zle/zle_refresh.c#L513)),
so the *number of regions* is the cost, not the length of the line.

The truncation is **reported**, not silent: `Entries::build` returns a `bool`
and the module logs it when `SHELL_PULSE_DEBUG` is set
([`crates/zsh-module/src/lib.rs:205-208`](../../crates/zsh-module/src/lib.rs#L205)).

---

## 3. Offsets: bytes in, characters out

This is the part that is easy to get wrong, and the code is careful about it.

| | |
|---|---|
| the lexer works in | **byte** offsets into the line |
| `region_highlight` wants | **character** offsets — "the same units as `CURSOR`" |

The conversion is in
[`crates/highlight/src/lib.rs:274-303`](../../crates/highlight/src/lib.rs#L274)
(`char_offsets_into`): one `u32` per byte offset, giving the number of
complete characters in `text[..i]`. For a pure-ASCII line byte offsets *are*
character offsets, and the map is not built at all
([`crates/zsh-module/src/lib.rs:199-203`](../../crates/zsh-module/src/lib.rs#L199))
— building it would cost more than the whole lexer.

Two properties of the map are asserted by tests:

- it must be monotonic and the right length even for invalid UTF-8
  (`char_offsets_survive_invalid_utf8`); a wrong map silently misplaces
  *every* highlight after the bad byte;
- a stray continuation byte is counted as a character of its own rather than
  being attributed to the previous one.

The end-to-end proof is
`the_highlighter_reports_character_offsets_not_byte_offsets` in
[`crates/zsh-module/tests/zsh_integration.rs`](../../crates/zsh-module/tests/zsh_integration.rs):
for `echo é | grep x`, the `|` is reported at character 7, not byte 8.

### The `P` flag

The module never emits `P` and never reads `PREDISPLAY`. Without the flag,
zsh subtracts zero from the offsets
([`zsh-src/Src/Zle/zle_utils.c:216-220`](../upstream/zsh-src/Src/Zle/zle_utils.c#L216)),
so the regions address the line buffer itself. Predisplay text is therefore
not highlighted — which is the correct behaviour, since the flag exists
precisely for the case where the predisplay *should* be highlighted.

---

## 4. Inside a real widget

### 4.1 The special parameters are per widget call

zsh creates the ZLE special parameters (`BUFFER`, `CURSOR`, `LBUFFER`,
`region_highlight`, …) for **every widget call**:

```c
/* zsh-src/Src/Zle/zle_main.c:1534 */
makezleparams(0);
```

`makezleparams` ([`zsh-src/Src/Zle/zle_params.c:194-215`](../upstream/zsh-src/Src/Zle/zle_params.c#L194))
creates each parameter with `PM_SPECIAL | PM_REMOVABLE | PM_LOCAL`, so
`region_highlight` is a **local** parameter of the widget's scope.

Two consequences, and they pull in opposite directions:

1. The parameter is **recreated** for every widget call, and it is a *local*
   parameter of that scope. It does not *store* a value: its getter derives
   the current regions from the C array
   ([`zsh-src/Src/Zle/zle_refresh.c:430-481`](../upstream/zsh-src/Src/Zle/zle_refresh.c#L430))
   and its setter **replaces** that C array
   ([`zsh-src/Src/Zle/zle_refresh.c:488-586`](../upstream/zsh-src/Src/Zle/zle_refresh.c#L488)).
   So a write is destructive: it replaces *every* user region, not only
   ours. And outside a widget the parameter does not exist at all, so
   writing to it then would create a *global* parameter of that name in the
   user's shell — which is why the module checks that it exists first.
2. The C array that actually drives the display is **not** recreated. It is
   the file-scope `struct region_highlight *region_highlights`
   ([`zsh-src/Src/Zle/zle_refresh.c:227`](../upstream/zsh-src/Src/Zle/zle_refresh.c#L227)),
   and it survives from one widget call to the next — but **not** from one
   line to the next: `zleread()` ends with `set_region_highlight(NULL, NULL)`
   (`zsh-src/Src/Zle/zle_main.c:1398`), which drops every user region. The
   adapter therefore installs a `zle-line-init` hook that tells the module a
   new line is starting, so that a recalled command (whose text matches the
   cached one) is published again instead of being drawn unhighlighted.

So "do nothing when nothing changed" is **correct**: the display array still
holds the previous regions, and re-writing the parameter is not needed to keep
them.

> A note on the module's own comment, which says the parameter "is always
> empty when the builtin runs"
> ([`crates/zsh-module/src/lib.rs:129-135`](../../crates/zsh-module/src/lib.rs#L129)).
> More precisely, the parameter does not hold a value of its own — reading it
> derives the regions from the C array. The conclusion the module draws from
> it (check that the parameter exists before writing to it) is the right one
> either way.

That is the whole reason for the two short-circuits in
`Highlighter::update`:

```rust
// crates/zsh-module/src/lib.rs:174-186
if outcome.unchanged && self.pushed {
    return;                       // the line did not change
}
self.spans.update_from_tokens(self.cache.tokens());
if self.spans == self.pushed_spans && self.pushed {
    return;                       // the styling did not change
}
```

The first test that covers this is `an_unchanged_line_is_not_pushed_again`:
after a push, a sentinel is written into the array, the builtin is called
again with the same line, and the sentinel is still there — the module did
not write.

### 4.2 The order of operations inside a redraw

```
zsh decides to redraw
  └─ redrawhook()                              zle_main.c:1066
       └─ execzlefunc(...)  → the widget call  zle_main.c:1087
            ├─ makezleparams(0)                zle_main.c:1534
            │     creates BUFFER / CURSOR / region_highlight for this call
            └─ our hook function runs
                 └─ shell-pulse-highlight --region-highlight
                  ├─ read CURSOR, read BUFFER      (getsparam_u)
                  ├─ re-lex, build spans, build rows
                  └─ setaparam("region_highlight", rows)
                      └─ set_region_highlight()   zle_refresh.c:488
                            ├─ parses each row into struct region_highlight
                            └─ freearray(av)        zle_refresh.c:586
  └─ the line is drawn using region_highlights[]
```

Note the direction of the last step: the module writes the *parameter*; zsh
parses it into the C array and draws from the C array. The module never
touches `region_highlights` and cannot.

### 4.3 What the plugin does inside a widget

| | |
|---|---|
| reads | `BUFFER` (unmetafied, via `getsparam_u`), `CURSOR` (parsed to an `i64`) |
| writes | `region_highlight` — and only when `--region-highlight` was passed **and** the parameter exists |
| never writes | `BUFFER`, `CURSOR`, `LBUFFER`, `RBUFFER`, `MARK`, `PREDISPLAY`, `POSTDISPLAY` |
| never creates | a global `region_highlight` |

`CURSOR` is read **before** `BUFFER`, because both come from zsh's single
static unmetafied buffer
([`crates/zsh-module/src/lib.rs:316-323`](../../crates/zsh-module/src/lib.rs#L316),
and zsh's own comment at
[`zsh-src/Src/params.c:3049-3054`](../upstream/zsh-src/Src/params.c#L3049)):
`getsparam_u` returns a pointer to a static buffer, so the bytes must be
copied before the next read.

---

## 5. The guards

Three limits keep a pathological line from eating a keystroke. All three
live in [`crates/zsh-module/src/lib.rs`](../../crates/zsh-module/src/lib.rs).

| Guard | Value | Where | What happens when it trips |
|---|---|---|---|
| line length | 4 KiB (`MAX_LINE`) | `:101` | the highlights are **cleared**, not left stale |
| wall-clock budget | 1 ms (`BUDGET`) | `:92` | the highlights are **cleared**; the next keystroke is cheap because the cache is already updated |
| region count | 256 (`region::MAX_ENTRIES`) | `region.rs:156` | the extra regions are dropped, and the truncation is reported |

The budget is 100× the *target* per-keystroke cost (10 µs) and about 2 000×
the *measured* one (0.86 µs, [performance report §1](performance.md)), so it
only trips on a line that is already pathological.

**Clearing, not leaving stale.** When the *line length* or the *budget* is
exceeded, and when the line cannot be lexed at all, the module calls
`Highlighter::clear`, which writes an empty array. The alternative — leaving
the previous regions in place — would show the styling of a line that is no
longer on the screen. The behaviour is asserted by
`a_line_that_cannot_be_parsed_is_left_unhighlighted`.

The *region count* guard is different: it drops the extra regions and keeps
the first 256, because the line is still perfectly highlightable — it is only
the number of regions that is capped.

### What is *not* guarded

There is **no paste detection**. zsh-syntax-highlighting detects a paste and
skips highlighting it
([`../upstream/PERFORMANCE_FAILURES.md` §6](../upstream/PERFORMANCE_FAILURES.md));
this plugin does not. A large paste is caught by the 4 KiB line limit and by
the 1 ms budget, but a paste *under* 4 KiB is highlighted in full, once, on
the redraw after the paste. See
[known limitations](known-limitations.md).

---

## 6. The fallback

There are three fallbacks, at three different levels.

### 6.1 The module cannot be loaded

```zsh
if ! zmodload shellpulse/highlight 2>/dev/null; then
  return 0
fi
```

([`zsh/init.zsh:28-31`](../../zsh/init.zsh#L28)). No module, no hook, no
damage: zsh's own highlighting is used and the adapter does nothing at all.

The `2>/dev/null` swallows zsh's "module not found" message on purpose; the
adapter is designed to be invisible when it cannot work.

### 6.2 The module loads but this zsh lacks a required symbol

`setup_` probes all six required symbols and returns −1 if any is missing
([`crates/zsh-module/src/lib.rs:568-592`](../../crates/zsh-module/src/lib.rs#L568)),
which makes zsh abort the load cleanly. The adapter then takes the same path
as 6.1.

This is the "export variance" risk from the plan, retired by probing rather
than by link-time failure: a shared object cannot fail to link, so the
failure has to be detected at load time.

### 6.3 A panic inside the module

Every entry point runs inside `guard()`
([`crates/zsh-abi/src/guard.rs:47-58`](../../crates/zsh-abi/src/guard.rs#L47)),
which catches the panic, sets a sticky "degraded" flag and returns a benign
value. The builtin then returns 0 immediately on every subsequent call
([`crates/zsh-module/src/lib.rs:298-300`](../../crates/zsh-module/src/lib.rs#L298)),
so the shell stays usable and the user gets a diagnosable message rather than
a lost session.

The workspace is built with `panic = "unwind"` on purpose
([`Cargo.toml`](../../Cargo.toml)); abort-on-panic would make this mechanism
ineffective.

---

## 7. What the user sees

| Line | Result |
|---|---|
| `git commit --amend` | `git` in cyan bold; the rest unstyled |
| `echo "hello"` | `echo` in cyan bold; `"hello"` in green |
| `FOO=bar cmd` | `FOO=bar` in yellow; `cmd` in cyan bold |
| `ls *.rs` | `ls` in cyan bold; `*.rs` in yellow; the `*` and the word merged into one run |
| `echo hi # a comment` | `echo` cyan bold; the comment in blue |
| a line the lexer cannot represent | no colours at all, and no stale colours from the previous line |

The last row is the design rule: **a wrong highlight is worse than no
highlight**, so the failure mode is visible and uncoloured.

---

## 8. Verification

| Claim | Where it is checked |
|---|---|
| the module loads into a bare zsh | `module_loads_into_a_bare_zsh` |
| the builtin is registered under the exact name | `builtin_is_registered_under_its_exact_name` |
| no garbage names appear in the builtin table | `no_garbage_names_appear_in_the_builtin_table` |
| the regions are correct | `the_highlighter_fills_region_highlight` |
| the offsets are characters, not bytes | `the_highlighter_reports_character_offsets_not_byte_offsets` |
| no global `region_highlight` is created | `the_highlighter_does_not_create_a_global_region_highlight` |
| nothing is published without the flag | `the_regions_are_not_published_unless_the_caller_asks` |
| an unchanged line is not pushed again | `an_unchanged_line_is_not_pushed_again` |
| a changed line is pushed again | `a_changed_line_is_pushed_again` |
| an unparseable line is left unhighlighted | `a_line_that_cannot_be_parsed_is_left_unhighlighted` |
| the adapter chains an existing hook | `the_adapter_chains_an_existing_pre_redraw_hook` |
| **the colours actually reach the terminal** | `the_typed_command_is_highlighted_on_the_terminal` in [`crates/zsh-module/tests/zsh_pty.rs`](../../crates/zsh-module/tests/zsh_pty.rs) |
| **the keystroke-to-redraw latency of the real editor loop** | [`bench/bench_zpty.zsh`](../../bench/bench_zpty.zsh) — see [performance §3.5](performance.md) |

The last one is the only one that proves the *display* works rather than the
*computation*. It drives a real interactive zsh under a pseudo-terminal and
asserts on the bytes zsh writes to the terminal — `ESC[1m ESC[36m` for
`fg=cyan,bold`, and `ESC[0m` to close the region.

That test is **skippable**: it needs `zsh/zpty`, a pseudo-terminal, and a
terminal type with the standard xterm colour capabilities. When any of them
is missing the test prints why and passes. On the machine these documents were
written on, all three of its tests ran and passed.
