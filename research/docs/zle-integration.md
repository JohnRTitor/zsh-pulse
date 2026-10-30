# ZLE integration design — `zsh-pulse`

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

The adapter is two files. [`zsh/init.zsh`](../../zsh/init.zsh) loads the
module and does nothing else, and
[`zsh/widgets.zsh`](../../zsh/widgets.zsh) holds the hooks.

`init.zsh` gives up quietly if the module cannot be loaded:

```zsh
if ! zmodload zshpulse/core 2>/dev/null; then
  return 0
fi
```

`widgets.zsh` defines the two widgets — `zsh-pulse-highlight-redraw` and
`zsh-pulse-highlight-line-init` — and registers them with zsh's own
`add-zle-hook-widget`:

```zsh
add-zle-hook-widget zle-line-pre-redraw zsh-pulse-highlight-redraw
add-zle-hook-widget zle-line-init zsh-pulse-highlight-line-init
```

`add-zle-hook-widget` **keeps** the widgets that are already registered for
the hook and calls them as well. `zle -N` would *replace* them, and zsh
allows exactly one widget per name — so two plugins using the same hook
would silently disable each other. That is not hypothetical: zsh-autocomplete
registers its own wrapper for `zle-line-pre-redraw` *after* the `.zshrc`
has been read, which is exactly when it would have disabled this one. The
behaviour is asserted by `the_adapter_chains_an_existing_pre_redraw_hook` in
[`crates/zsh-module/tests/zsh_integration.rs`](../../crates/zsh-module/tests/zsh_integration.rs).

**Known rough edge:** if the user loads the plugin *twice*, the same function
is registered with the hook twice, so the builtin is called twice per redraw.
The second call finds that nothing changed and returns without touching zsh
(see §4.1), so the cost is one extra native call, not a double highlight.

### 1.4 The `--region-highlight` flag

The builtin takes an optional `--region-highlight` argument. Without it, the
module **reads** the line and changes nothing
([`crates/zsh-module/src/lib.rs:248-261`](../../crates/zsh-module/src/lib.rs#L248)).

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

### 1.5 The command-word arguments

The module cannot see the shell's tables of aliases, functions, builtins,
commands, suffix aliases and reserved words: they live in the shell, and a
loaded module has no way to read them. So the **adapter** asks the shell, on
every keystroke, and passes the answer to the module in the argument
vector.

Two arguments carry the answer:

| Argument | Meaning |
|---|---|
| `--command-known=<word>` | the shell knows `<word>` as a command |
| `--command-unknown=<word>` | the shell does not know `<word>` as a command |

Both are repeatable, and **every** command word of the line is answered —
not only the first one. `ls | grep x` gets an answer for `ls` and for
`grep`. A command inside a command substitution is answered as well:
`echo $(ls)` gets an answer for `echo` and for `ls`, and the answer is
about the word *inside* the substitution, because that is the text the
module compares its own command word with.

#### The word is part of the answer

The word is the raw text of the command word, byte for byte, **with its
quoting preserved**: `'ls'` is the four bytes `'ls'`, not the two bytes
`ls`. The module compares the word it found with the word the shell was
asked about, byte for byte, and applies the answer only to a command word
whose bytes are identical. An answer that does not match is **ignored**.

That rule is what makes the protocol safe. The shell and the lexer do not
always agree about where the command word is — the shell splits the line
its own way and the lexer scans it its own way — and a disagreement would
mean colouring the wrong word. Rather than risk that, the answer is dropped
and the word is left exactly as the user typed it. A wrong colour is worse
than no colour.

#### A command word with no answer is never painted red

If the shell was not asked about a command word — because the adapter found
none, or because the answer was about a different word — the module does
not colour it red. Red means "the shell does not know this command", and
the shell was never asked. The word is still looked up on the filesystem
like every other literal word, so it may come out magenta; otherwise it is
drawn exactly as the user typed it.

This is also what keeps a missing `zsh/parameter` harmless. When the
shell's tables are not available the adapter reports nothing at all, so a
command word cannot be turned red by the mere absence of a module.

#### Finding the command words

The adapter's job is to find **every** command word in the line, the way the
shell itself finds them. It splits the line with the shell's own lexer —
`${(z)BUFFER}` — and then walks the words, skipping:

* **assignments**: `FOO=bar cmd` runs `cmd`, so `FOO=bar` is not a command
  word. The same is true of `FOO+=bar cmd`, and the adapter has to know
  that: the Rust lexer knows it too
  ([`crates/lexer/src/scanner.rs`](../../crates/lexer/src/scanner.rs),
  `looks_like_assignment`), and if the two sides disagreed about what an
  assignment is, the command word after it would never be answered and
  would never be coloured;
* **redirection operators and their targets**: `cmd > out` writes to the
  file `out`, so neither the operator nor the target is a command word;
* **operators**: `|`, `;`, `&`, `(` and `)` end one command and begin
  another, so the word after one is a command word.

A word may also hold a command of its own: `$(cmd)` does. The adapter
looks inside a substitution that makes up the **whole** word, splits the
text between the `$(` and the `)` with the shell's own lexer, and answers
for the command words in it. A substitution that does not make up the whole
word — `x$(cmd)y` — is not looked inside. The nesting is bounded at four
levels (`ZSH_PULSE_MAX_SUBSTITUTION_DEPTH`), so a pathological line
cannot make the adapter recurse for ever.

The name that is looked up is the word *without* its quoting, because that
is the name the shell would run; the word that is sent back to the module is
the raw text, because that is what the module has to match.

The lookup itself is a **hash lookup in the shell's own tables** —
`$aliases`, `$galiases`, `$functions`, `$builtins`, `$commands`, `$saliases`
and `$reswords`, from `zsh/parameter`. It does **not** fork: a fork on
every keystroke would be visible to the user, and the tables are already in
the shell's memory. The reference implementation answers the same question
from the same tables
([`../upstream/fsh/fast-highlight:299-313`](../upstream/fsh/fast-highlight#L299))
and only falls back to a fork for a word that is in none of them
([`../upstream/fsh/fast-highlight:320-321`](../upstream/fsh/fast-highlight#L320)).

#### The line the adapter will look at

The adapter refuses to look at a line longer than **4 KiB**
(`ZSH_PULSE_MAX_LINE`): the length of `$BUFFER` is compared with the
limit *before* the line is split
([`zsh/widgets.zsh:141-143`](../../zsh/widgets.zsh#L141)), and a line that
is too long is left alone.

The reason is that the module does not highlight such a line either
(`MAX_LINE`,
[`crates/zsh-module/src/lib.rs:102`](../../crates/zsh-module/src/lib.rs#L102)).
Splitting it and walking every word of it would cost a full split and a
walk of every word on *every* keystroke, for a line that is never
highlighted — and a pasted megabyte would have made the shell unresponsive.

The two limits are the same number, so for an ASCII line the two sides
agree about which lines are worth looking at. They do not count the same
thing, though: the adapter counts **characters** (`${#BUFFER}`) and the
module counts **bytes**. A line of multi-byte characters can therefore be
split by the adapter and refused by the module — which costs the adapter a
split, and changes nothing about what is drawn.

#### The answers are cached for two seconds

The lookup is a hash lookup, but it is done on every keystroke, and the
command word rarely changes while the user types the arguments — so the
answers are remembered (`ZSH_PULSE_COMMAND_CACHE`). An answer is only
trusted for **two seconds** (`ZSH_PULSE_COMMAND_CACHE_TTL`).

Both the positive and the negative answers are cached, and both expire. A
word the shell knew may stop being known — the user may `unalias` it or
remove the function — and an answer trusted for ever would keep the word
green after the shell had forgotten it. The same is true in the other
direction: a command may be installed while the session runs, and a
remembered "not known" would be wrong until the answer expired. The cache
holds at most 256 answers; when it is full it is emptied.

#### The arguments are quoted

The adapter quotes every array it passes on:

| call | array |
|---|---|
| `zsh-pulse-highlight-words "${words[@]}"` | the words of the line |
| `zsh-pulse-highlight-known "${(Q)word}"` | the name to look up, with the quoting removed |
| `zsh-pulse-highlight "${args[@]}"` | the argument vector for the builtin |

A command word may hold a character that is a glob pattern (`*`, `?`, `[`),
and with `GLOB_SUBST` set in the user's configuration an unquoted expansion
would be matched against the filesystem, and a word with no match would
abort the call. Quoting the arrays means that the guarantee that a command
word is never taken for a glob pattern does not depend on the options that
happen to be set two frames up the call stack.

`zsh-pulse-highlight-known` also returns early for an **empty word**:
`''` is a word, but it is not a name the shell could know, and the guard
keeps the subscript lookups below from being asked about an empty key.

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
[`crates/zsh-module/src/region.rs:91-120`](../../crates/zsh-module/src/region.rs#L91):

| Style | Attribute |
|---|---|
| a command the shell knows | `fg=green` |
| a command the shell does not know | `fg=red,bold` |
| an option (`-l`, `--amend`) | `fg=cyan` |
| a word naming an existing file | `fg=magenta` |
| a word naming an existing directory | `fg=magenta,underline` |
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

These are the reference implementation's colours rather than this project's
invention: `command` is `fg=green`, `unknown-token` is `fg=red,bold`,
`path` is `fg=magenta`, `path-to-dir` is `fg=magenta,underline`, and both
option styles are `fg=cyan`
([`../upstream/fsh/fast-highlight:58-93`](../upstream/fsh/fast-highlight#L58)).
zsh's own attribute parser accepts every one of them: `match_highlight`
([`../upstream/zsh-src/Src/prompt.c:2031`](../upstream/zsh-src/Src/prompt.c#L2031))
reads the colour after `fg=` and the attributes after the comma, from the
`highlights[]` table
([`../upstream/zsh-src/Src/prompt.c:1896`](../upstream/zsh-src/Src/prompt.c#L1896)).

An ordinary argument is deliberately *not* highlighted: a command line where
every word is coloured is harder to read, not easier. The two exceptions
are the words that say something about the machine rather than about the
command — an option, and a word that names a file that exists.

A command word the shell does not know is red — unless it names a file that
exists, in which case it is magenta, because the filesystem is consulted
before the word is called unknown. The order the two answers are asked in
is described in [architecture §3](architecture.md).

Every published entry carries `memo=zsh-pulse`
([`crates/zsh-module/src/region.rs:34`](../../crates/zsh-module/src/region.rs#L34)),
so that another plugin — or a future version of this one — can tell whose
entries they are.

A row therefore looks like:

```
0 4 fg=green memo=zsh-pulse
```

### 2.3 The rows are built in one reused buffer

[`crates/zsh-module/src/region.rs:127-262`](../../crates/zsh-module/src/region.rs#L127)
keeps the rows NUL-separated in a single `Vec<u8>` that is reused for every
keystroke. A keystroke does not allocate one string per highlighted region,
which on the interactive thread is exactly what must not happen.

The decimal numbers are written by hand
([`crates/zsh-module/src/region.rs:281-293`](../../crates/zsh-module/src/region.rs#L281))
because `format!` would allocate.

### 2.4 The region limit

At most **32** regions are published per push
([`crates/zsh-module/src/region.rs:276`](../../crates/zsh-module/src/region.rs#L276)):

```rust
pub const MAX_ENTRIES: usize = 32;
```

The reason is in the comment, and it is the right reason: zsh parses every
region specification on every redraw
([`zsh-src/Src/Zle/zle_refresh.c:513-586`](../upstream/zsh-src/Src/Zle/zle_refresh.c#L513)),
so the *number of regions* is the cost, not the length of the line.

The number is **measured, not guessed**: the pty harness can type a line with
N regions and measure the per-keystroke latency, and at 32 regions the added
latency is about 10 µs — the budget this project set for a keystroke. The
measurement is in the [performance report §3.5](performance.md). It used to
be 256, which was a guess, and at that count the added latency was over
200 µs.

The truncation is **reported**, not silent: the entry builder returns a `bool`
and the module logs it when `ZSH_PULSE_DEBUG` is set
([`crates/zsh-module/src/lib.rs:327-329`](../../crates/zsh-module/src/lib.rs#L327)).

---

## 3. Offsets: bytes in, characters out

This is the part that is easy to get wrong, and the code is careful about it.

| | |
|---|---|
| the lexer works in | **byte** offsets into the line |
| `region_highlight` wants | **character** offsets — "the same units as `CURSOR`" |

The conversion is in
[`crates/highlight/src/lib.rs:294`](../../crates/highlight/src/lib.rs#L294)
(`char_offsets_into`): one `u32` per byte offset, giving the number of
complete characters in `text[..i]`. For a pure-ASCII line byte offsets *are*
character offsets, and the map is not built at all
([`crates/zsh-module/src/lib.rs:274-278`](../../crates/zsh-module/src/lib.rs#L274))
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
> ([`crates/zsh-module/src/lib.rs:147-153`](../../crates/zsh-module/src/lib.rs#L147)).
> More precisely, the parameter does not hold a value of its own — reading it
> derives the regions from the C array. The conclusion the module draws from
> it (check that the parameter exists before writing to it) is the right one
> either way.

That is the whole reason for the two short-circuits in
`Highlighter::update`:

```rust
// crates/zsh-module/src/lib.rs:216-246
if outcome.unchanged && self.pushed {
    return;                       // the line did not change
}
…
unsafe { build_spans(spans, paths, scratch, api, buffer, tokens, answers) };
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
                 └─ zsh-pulse-highlight --region-highlight
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
| reads (from the arguments) | the shell's answers about the command words: `--command-known=<word>`, `--command-unknown=<word>` (§1.5) |
| writes | `region_highlight` — and only when `--region-highlight` was passed **and** the parameter exists |
| never writes | `BUFFER`, `CURSOR`, `LBUFFER`, `RBUFFER`, `MARK`, `PREDISPLAY`, `POSTDISPLAY` |
| never creates | a global `region_highlight` |

`CURSOR` is read **before** `BUFFER`, because both come from zsh's single
static unmetafied buffer
([`crates/zsh-module/src/lib.rs:1510-1515`](../../crates/zsh-module/src/lib.rs#L1510),
and zsh's own comment at
[`zsh-src/Src/params.c:3049-3054`](../upstream/zsh-src/Src/params.c#L3049)):
`getsparam_u` returns a pointer to a static buffer, so the bytes must be
copied before the next read.

---

## 5. The guards

Four limits keep a pathological line from eating a keystroke. Three live in
[`crates/zsh-module/src/lib.rs`](../../crates/zsh-module/src/lib.rs) and one
in the adapter.

| Guard | Value | Where | What happens when it trips |
|---|---|---|---|
| line length | 4 KiB (`MAX_LINE`) | `:102` | the highlights are **cleared**, not left stale |
| wall-clock budget | 1 ms (`BUDGET`) | `:93` | the highlights are **cleared**; the next keystroke is cheap because the cache is already updated |
| region count | 32 (`region::MAX_ENTRIES`) | `region.rs:276` | the extra regions are dropped, and the truncation is reported |
| the adapter's line cap | 4 KiB (`ZSH_PULSE_MAX_LINE`) | `zsh/widgets.zsh:90` | the line is **not split** and its words are not walked; the module would not have highlighted it either |

The adapter's cap is the fourth one, and it is the one that keeps a pasted
megabyte cheap: without it, every keystroke of such a line would cost a
full split of it and a walk of every word in it, for a line the module
never highlights.

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
the first 32, because the line is still perfectly highlightable — it is only
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
if ! zmodload zshpulse/core 2>/dev/null; then
  return 0
fi
```

([`zsh/init.zsh:28-31`](../../zsh/init.zsh#L28)). No module, no hook, no
damage: zsh's own highlighting is used and the adapter does nothing at all.

The `2>/dev/null` swallows zsh's "module not found" message on purpose; the
adapter is designed to be invisible when it cannot work.

### 6.2 The module loads but this zsh lacks a required symbol

`setup_` probes all six required symbols and returns −1 if any is missing
([`crates/zsh-module/src/lib.rs:2471-2496`](../../crates/zsh-module/src/lib.rs#L2471)),
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
([`crates/zsh-module/src/lib.rs:1472-1475`](../../crates/zsh-module/src/lib.rs#L1472)),
so the shell stays usable and the user gets a diagnosable message rather than
a lost session.

The workspace is built with `panic = "unwind"` on purpose
([`Cargo.toml`](../../Cargo.toml)); abort-on-panic would make this mechanism
ineffective.

---

## 7. What the user sees

| Line | Result |
|---|---|
| `git commit --amend` | `git` in green; `--amend` in cyan |
| `echo "hello"` | `echo` in green; `"hello"` in green |
| `FOO=bar cmd` | `FOO=bar` in yellow; `cmd` in green |
| `FOO+=bar cmd` | `FOO+=bar` in yellow; `cmd` in green |
| `ls *.rs` | `ls` in green; `*.rs` in yellow; the `*` and the word merged into one run |
| `cat ./Cargo.toml` | `cat` in green; `./Cargo.toml` in magenta |
| `cat Cargo.toml` | `cat` in green; `Cargo.toml` in magenta — a bare file name names a file too |
| `nosuchcommand` | red and bold, because the shell does not know that word |
| `echo $(ls)` | `echo` in green; `ls` in green — the command inside the substitution is answered as well |
| `echo hi # a comment` | `echo` green; the comment in blue |
| a line the lexer cannot represent | no colours at all, and no stale colours from the previous line |

The last row is the design rule: **a wrong highlight is worse than no
highlight**, so the failure mode is visible and uncoloured. The same rule is
why a command word the shell was not asked about is left alone rather than
guessed at.

---

## 8. Verification

| Claim | Where it is checked |
|---|---|
| the module loads into a bare zsh | `module_loads_into_a_bare_zsh` |
| the builtin is registered under the exact name | `builtin_is_registered_under_its_exact_name` |
| no garbage names appear in the builtin table | `no_garbage_names_appear_in_the_builtin_table` |
| the regions are correct | `the_highlighter_fills_region_highlight` |
| a command the shell knows is green | `a_command_the_shell_knows_is_green` |
| a command the shell does not know is red | `a_command_the_shell_does_not_know_is_red` |
| an answer about another word is not applied | `an_answer_about_another_word_is_not_applied` |
| an option is cyan | `an_option_is_cyan` |
| an existing file is magenta | `an_existing_file_is_magenta` |
| an existing directory is magenta and underlined | `an_existing_directory_is_magenta_and_underlined` |
| a bare file name is magenta too | `a_bare_file_name_is_magenta` |
| a bare directory name is magenta and underlined | `a_bare_directory_name_is_magenta_and_underlined` |
| a word that names nothing is left alone | `an_argument_that_names_nothing_is_not_highlighted` |
| a word that does not exist is not a path | `a_word_that_does_not_exist_is_not_a_path` |
| a tilde is the home directory | `a_tilde_is_the_home_directory` |
| the command inside a substitution is coloured | `the_command_inside_a_substitution_is_coloured` |
| a leading assignment does not hide the command | `a_leading_assignment_does_not_hide_the_command` |
| an append assignment does not hide the command | `an_append_assignment_does_not_hide_the_command` |
| every command word of a pipeline is coloured | `every_command_word_of_a_pipeline_is_coloured` |
| a command word without an answer is left alone | `a_command_word_without_an_answer_is_left_alone` |
| the offsets are characters, not bytes | `the_highlighter_reports_character_offsets_not_byte_offsets` |
| no global `region_highlight` is created | `the_highlighter_does_not_create_a_global_region_highlight` |
| nothing is published without the flag | `the_regions_are_not_published_unless_the_caller_asks` |
| an unchanged line is not pushed again | `an_unchanged_line_is_not_pushed_again` |
| a changed line is pushed again | `a_changed_line_is_pushed_again` |
| an unparseable line is left unhighlighted | `a_line_that_cannot_be_parsed_is_left_unhighlighted` |
| the adapter chains an existing hook | `the_adapter_chains_an_existing_pre_redraw_hook` |
| **the colours actually reach the terminal** | `the_typed_command_is_highlighted_on_the_terminal` in [`crates/zsh-module/tests/zsh_pty.rs`](../../crates/zsh-module/tests/zsh_pty.rs) |
| **the keystroke-to-redraw latency of the real editor loop** | [`bench/bench_zpty.zsh`](../../bench/bench_zpty.zsh) — see [performance §3.5](performance.md) |

The pty test is the only one that proves the *display* works rather than
the *computation*. It drives a real interactive zsh under a pseudo-terminal
and asserts on the **screen the shell drew**: the harness reconstructs the
screen from the bytes zsh writes — the text, the cursor motions, the erases
and the attributes in force — and checks the attributes of the characters of
the command word.

The bytes alone are not the screen. zsh's line editor draws the line in
pieces and moves the cursor between them, so the text of the line can be
broken up — and even reordered — in the byte stream, and which of the two
happens depends on how the input reached the shell. Asserting on the
reconstructed screen is therefore both more reliable and a stronger claim:
the test can say that the characters of the command word were drawn green,
not merely that the bytes for green appeared somewhere.

That test is **skippable**: it needs `zsh/zpty`, a pseudo-terminal, and a
terminal type with the standard xterm colour capabilities. When any of them
is missing the test prints why and passes. The file holds eight tests, and
every one of them skips that way when the environment cannot provide what
it needs — so a green test run does not by itself prove that the ZLE path
is covered.
