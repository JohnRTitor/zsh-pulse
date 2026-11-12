# ZLE Integration Design — `zsh-pulse`

How `zsh-pulse` interfaces with the Zsh Line Editor (ZLE), manages redraw hooks, displays real-time typeahead completion, and guarantees menu-selection stability.

Companion documents: [architecture](architecture.md) ·
[performance](performance.md) ·
[Rust↔Zsh boundary](rust-zsh-boundary.md) ·
[known limitations](known-limitations.md) ·
[Features catalog](../FEATURES.md) ·
[Milestones archive](archive/tasks-milestones.md)

---

## 1. The Redraw Integration Point

### 1.1 The `zle-line-pre-redraw` Hook
ZLE provides a dedicated hook executed immediately before the input buffer is drawn to the terminal screen: `zle-line-pre-redraw`.
As documented in Zsh's manual (`zsh-src/Doc/Zsh/zle.yo:1164-1168`), this hook is specifically provided as the canonical opportunity to update the `region_highlight` array.

`zsh-pulse` hooks `zle-line-pre-redraw` using Zsh's standard `add-zle-hook-widget`. This ensures that existing widgets registered by other plugins (e.g. `zsh-autocomplete` or custom prompts) are chained rather than clobbered.

### 1.2 Redraw Hook Responsibilities
On each invocation of `zsh-pulse-highlight-redraw`:
1. **Menu Guard**: If menu completion is currently active (`KEYMAP == menuselect` or `zsh-pulse-complete --is-menu-active`), unsets `$POSTDISPLAY` and returns immediately.
2. **Typeahead Preview**: If completion is enabled, not in a history search, `PENDING == 0`, `KEYS_QUEUED_COUNT == 0`, and `_ZSH_PULSE_IN_TYPEAHEAD == 0`, invokes `_zsh_pulse_typeahead_widget` when the buffer changed to preview native compsys candidates below the prompt. The preview is synchronous and runs on the ZLE thread.
3. **Native Syntax Highlighting**: Invokes `zsh-pulse-highlight --region-highlight` with cached command status arguments, publishing updated spans into `region_highlight`.
4. **Inline Autosuggestions**: If autosuggestions are enabled and cursor is at end of line, invokes `zsh-pulse-complete --autosuggest` and populates `$POSTDISPLAY`.

---

## 2. Native Compsys Typeahead Preview

### 2.1 The `list-choices` Completion Widget
- **Widget**: `_zsh_pulse_typeahead_widget` bound to `zle -C zsh-pulse-typeahead list-choices`
- **Behavior**: Provides a preview of Zsh's own completion candidates beneath the prompt as the user types, without waiting for `Tab`.
- **Candidate Source**: Runs `_main_complete` synchronously in `curcontext="list-choices:::"` with `compstate[insert]=''`, `compstate[old_list]=''`, `compstate[pattern_insert]=''`, `compstate[list]='list force packed'`, and `compstate[list_max]=0`. This yields the same native compsys results (commands, options, subcommands, descriptions, and group headings) that Tab shows, while guaranteeing the preview never inserts text into `$BUFFER`.
- **Line Cap & Truncation**: The preview is capped by rendered lines: `min(ZSH_PULSE_TYPEAHEAD_LINES (default 16), LINES - BUFFERLINES - 1)`. Group headings and the `(MORE)` marker count toward the cap. A `compadd`/`_describe` wrapper (adapted from zsh-autocomplete's `.autocomplete__async`, see [ATTRIBUTION.md](../ATTRIBUTION.md)) trims matches, stops further tags once full, and adds `(MORE)` when truncated.
- **Cost**: This path is synchronous native completion, not the Rust candidate engine. Latency depends on the active completion functions/tags and may be milliseconds or more; native completion functions may fork, and the truncation wrapper uses command substitution. Zero-fork/sub-microsecond claims apply only to Rust highlighting/autosuggest/history paths.

### 2.2 Re-Entrancy & Corruption Guards
Calling completion widgets while Zsh is already in menu selection (`complist.so`) causes memory corruption in Zsh's internal heap and triggers `SIGSEGV`.
To ensure safety, the redraw hook and widget are guarded:
- The redraw hook only invokes typeahead when `_ZSH_PULSE_IN_TYPEAHEAD == 0`, `! PENDING`, and `! KEYS_QUEUED_COUNT`.
- The widget exits immediately if `KEYMAP == menuselect`, `_ZSH_PULSE_MENU_ACTIVE == 1`, or `zsh-pulse-complete --is-menu-active` reports an active menu.
- `_ZSH_PULSE_TYPEAHEAD_VISIBLE` is set only when a preview is actually shown, and is cleared on accept/abort/line-init/line-finish/reset paths.

---

## 3. Menu Selection & Complist Stabilization

### 3.1 C-Level Menu-Active State Inspection
Standard Zsh scripts often check `[[ $KEYMAP == menuselect ]]`. However, during menu transitions, `$KEYMAP` may lag or report `"main"` / `"emacs"`.
`zsh-pulse` resolves this by querying internal Zsh C global symbols via `Api::is_menu_active`:
- `localkeymap`: verifies if a local keymap override is set.
- `menucmp`: checks if the internal `struct menu_info` pointer is non-null.

Both `zsh-pulse-complete --is-menu-active` and `zsh-pulse-highlight --is-menu-active` return exit code 0 when an interactive menu is active.

### 3.2 Menu Selection Event Handling & Safe Builtin Delegation
In `menuselect`, navigation and lifecycle events require distinct handling:
1. **Grid Navigation Builtins**: Keys (`Up`, `Down`, `Left`, `Right`, `h`, `j`, `k`, `l`) are explicitly bound to standard builtins:
   - `up-line-or-history`
   - `down-line-or-history`
   - `forward-char`
   - `backward-char`
   Zsh's `complist` module intercepts these specific built-in thingies inside its C `domenu()` event loop to move the selection highlight across candidate grid rows and columns without exiting menu mode.
2. **Safe Line Acceptance (`_zsh_pulse_menuselect_accept`)**:
   Pressing `Enter` (`^M`, `^J`, `\r`) in `menuselect` invokes `_zsh_pulse_menuselect_accept`. This resets `_ZSH_PULSE_MENU_ACTIVE=0` and delegates directly to the dot-prefixed internal builtin `.accept-line`, bypassing third-party user hook wrappers that could cause re-entrant redraw crashes.
3. **Safe Menu Abort (`_zsh_pulse_menuselect_abort`)**:
   Pressing `Tab` (`^I`), `Esc` (`\e`), `Ctrl-C` (`^C`), or `Ctrl-G` (`^G`) invokes `_zsh_pulse_menuselect_abort`, which restores the original command-line `$BUFFER` and `$CURSOR`, resets `_ZSH_PULSE_MENU_ACTIVE=0`, and sends `send-break`.

---

## 4. Keybinding Integration & Widgets

### 4.1 Tab Full-List Expansion (`_zsh_pulse_tab`)
- **Normal state**: The list shown while typing is a capped preview. Pressing `Tab` clears preview state (`_ZSH_PULSE_LAST_TYPEAHEAD`, `_ZSH_PULSE_TYPEAHEAD_VISIBLE`) and `_lastcomp=()` so `expand-or-complete` does not reuse the preview as the current list, then expands the full, uncapped native completion list on the first press. There is no hide/show toggle.
- **Inside `menuselect`**: Pressing `Tab` invokes `_zsh_pulse_menuselect_abort`, restoring the original buffer and cleanly closing the menu without inserting unwanted characters.

### 4.2 Down-Arrow Selection (`down-line-or-select`)
- If `_ZSH_PULSE_TYPEAHEAD_VISIBLE=1` and the buffer still matches the previewed buffer, pressing `Down-Arrow` (`^[[B`, `^[OB`, `^N`, Vi `j`) immediately enters menu selection, highlighting the first candidate for rapid navigation.
- If no preview is eligible, performs prefix history search forward.

### 4.3 History Search Navigation (`up-line-or-search` & `Ctrl-R`)
- `Up-Arrow` (`^[[A`, `^[OA`, `^P`, Vi `k`) performs Fish-style prefix history search (`up-line-or-beginning-search`) matching the text currently in `$BUFFER`.
- `Ctrl-R` automatically detects if `fzf-history-widget` or `fzf` is available. If present, it preserves and invokes the official `fzf-history-widget` without clobbering user setup. When absent (or with `ZSH_PULSE_USE_FZF=0`), it invokes native `zsh-pulse-history-search`, opening an interactive multi-line fuzzy history search menu.

### 4.4 Ghost Text Acceptance
- `Right-Arrow` (`^[[C`), `End` (`^E`), `Ctrl-F`: Appends the full `$POSTDISPLAY` suggestion to `$BUFFER`.
- `Alt-Right`, `Alt-F`, `Ctrl-Right`: Appends the next whitespace-delimited word of ghost text to `$BUFFER`.

---

## 5. Lifecycle Hooks

`zsh-pulse` installs three core ZLE lifecycle hooks via `add-zle-hook-widget`:

| Hook | Widget | Role |
|---|---|---|
| `zle-line-pre-redraw` | `zsh-pulse-highlight-redraw` | Runs typeahead preview, publishes syntax highlighting, and updates ghost text. |
| `zle-line-init` | `zsh-pulse-highlight-line-init` | Resets incremental highlight cache and clears typeahead buffer state for new command lines. |
| `zle-line-finish` | `zsh-pulse-highlight-line-finish` | Tears down active typeahead flags and hides menus when command is submitted. |

