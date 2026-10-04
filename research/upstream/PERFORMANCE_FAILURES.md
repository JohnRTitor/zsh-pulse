# Performance Failures Observed in Upstream

Failure modes that must become explicit design requirements. Each is traced in
source, not merely reported from an issue thread.

---

## 1. zsh-autosuggestions — unbounded history work per keystroke

**Where:** `zsh-autosuggestions.zsh:637-662`

```zsh
local prefix="${1//(#m)[\\*?[\]<>()|^~#]/\\$MATCH}"
local pattern="$prefix*"
...
typeset -g suggestion="${history[(r)$pattern]}"
```

The `(r)` subscript flag means "most recent value matching the pattern". zsh
implements this by scanning the history array **from the end until it finds a
match**, glob-compiling `$pattern` each time. Cost is `O(history length)` per
keystroke and grows without bound as history accrues.

Compounding it, the suggestion is fetched through a **fork per keystroke**:

```zsh
builtin exec {_ZSH_AUTOSUGGEST_ASYNC_FD}< <(
    echo $sysparams[pid]
    _zsh_autosuggest_fetch_suggestion "$1"
    echo -nE "$suggestion"
)
```
`zsh-autosuggestions.zsh:790-798`

The asynchrony moves latency off the keystroke path but the scan still runs,
once per keystroke, in a forked subshell. A latency-*hiding* fix, not a latency
fix.

**Requirement:** history lookup must be O(1)/O(log n) in history size, served
from an index built off the hot path.

---

## 2. zsh-autocomplete — a full second shell forked to measure one integer

**Where:** `Functions/Init/.autocomplete__async:225`, `:267`, `:276-286`

The plugin needs to know how tall the completion menu would be and whether it
fits in `TMOUT`. It answers by spawning **another zsh under `zpty`** and reading
back a single integer (`compstate[list_lines]`). The payload is one number; the
cost is a whole interactive shell, on the completion path.

**Requirement:** never fork a shell to compute a scalar. If menu height must be
known, compute it in-process from the candidate list.

---

## 3. zsh-autocomplete — per-keystroke constant work in the interactive shell

**Where:** `Functions/Init/.autocomplete__async:94-95`, `:127`, `:137`, `:345`

Every keystroke, in the *interactive* shell rather than a worker: backpressure
checks `(( KEYS_QUEUED_COUNT || PENDING ))` (`:94-95`); an array re-glob (`:127`);
a **full copy of `region_highlight`** (`:137`); a `$( )` command substitution
just to read the worker's reply (`:345`); `emulate -L zsh` plus 11 `setopt`
(`:2-4`). None of this is the completion work — it is fixed overhead charged to
every keystroke.

**Requirement:** the keystroke path must touch only memory it already owns. No
---

## 5. FSH — cost scales with the user's widget table

**Where:** `fsh/fast-syntax-highlighting.plugin.zsh:242-284`

```zsh
widgets_to_bind=(${${(k)widgets}:#(.*|run-help|which-command|beep|...)})
```

Every user and completion widget is wrapped. On the compatibility target
(16,551 completion functions, 8,278 widgets) that is a large per-keystroke
iteration over the widget table on top of ~40 µs of fixed overhead.

**Requirement:** integrate through `zle-line-pre-redraw` (or one bound widget),
never by re-wrapping the user's widget table.

---

## 6. zsh-syntax-highlighting — quadratic token handling

**Where:** `z-sy-h/main-highlighter.zsh:586-592` (full `${(z)}` tokenisation),
`:688` (`proc_buf` re-chop per token), `:1350-1353` (per-character scan per word)

Each token is re-sliced out of the buffer as a fresh string and each word is
scanned character by character, so work grows super-linearly with token length.
No timer, no budget, no incremental reuse; the only guards are paste detection
(`:214-215`) and opt-in `ZSH_HIGHLIGHT_MAXLENGTH` (`:211`).

**Requirement:** one forward pass with no per-token string allocation, plus a
wall-clock budget so a pathological line can never eat a keystroke.

---

## 7. Our own — a hand-transcribed ABI can segfault the user's shell

Observed directly during this research. Declaring `getsparam()` as returning
`char **` when zsh returns `char *` (`Src/params.c:3037-3045`) produced a
pointer into the middle of the buffer's bytes; printing it dereferenced garbage
and **crashed zsh with SIGSEGV**. Separately, an `enables_` implementation that
did not fill `*enables` also crashed, because `Src/module.c:2113-2117` walks
`enables` for `arrlen(features)` entries.

**Requirement:** transcribe the ABI from source with a test per symbol; resolve
every symbol at load time via `dlsym` and refuse to load rather than crash later;
run ABI tests in a subshell so a crash is a test failure.

---

## 8. ble.sh — documentation outruns evidence

ble.sh is careful and its incremental parser is genuinely good, but it ships **no
reproducible end-to-end completion or highlighting benchmark**. Its 20
micro-benchmarks cover data structures, not the editor loop, and numeric timings
live on an external wiki. Known-problem notes live in Japanese in-code comments.

**Requirement:** every performance claim ships with a runnable harness in the
repository, and the harness is validated against a deliberate failure — as §1 of
`PERFORMANCE_COMPARISON.md` was, where the first FSH run measured the
unchanged-buffer fast path and was discarded and redone.
`emulate`, no `$( )`, no array rebuilds, no globs.

---

## 4. ble.sh — candidate limits disabled by default

**Where:** `ble.sh/lib/core-complete-def.sh:44` — `bleopt/declare -v complete_limit ''`

The default is the empty string, i.e. **no limit**. Limits exist (13
`test-limit` sites) but apply only on auto-completion paths
(`complete_limit_auto=2000`), never to an explicit `<TAB>`. In a 10,000-entry
directory the glob is fully expanded, the array materialised several times, all
10k candidates yielded, and a menu constructed. Worst-case TAB latency is
unbounded and **untested in the repository**.

**Requirement:** bound the candidate set explicitly, make the bound
configurable, and report what was dropped. Never truncate silently.