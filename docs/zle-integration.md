# ZLE Integration Design — `shell-pulse`

How `shell-pulse` interfaces with the Zsh Line Editor (ZLE), manages redraw hooks, displays real-time typeahead completion, and guarantees menu-selection stability.

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

`shell-pulse` hooks `zle-line-pre-redraw` using Zsh's standard `add-zle-hook-widget`. This ensures that existing widgets registered by other plugins (e.g. `zsh-autocomplete` or custom prompts) are chained rather than clobbered.

### 1.2 Redraw Hook Responsibilities
On each invocation of `zsh-pulse-highlight-redraw`:
1. **Menu Guard**: If menu completion is currently active (`KEYMAP == menuselect` or `zsh-pulse-complete --is-menu-active`), unsets `$POSTDISPLAY` and returns immediately.
2. **Typeahead Auto-Listing**: If completion is enabled and keys are not pending, invokes `_zsh_pulse_typeahead_widget` to display up to 16 completion matches below the prompt.
3. **Native Syntax Highlighting**: Invokes `zsh-pulse-highlight --region-highlight` with cached command status arguments, publishing updated spans into `region_highlight`.
4. **Inline Autosuggestions**: If autosuggestions are enabled and cursor is at end of line, invokes `zsh-pulse-complete --autosuggest` and populates `$POSTDISPLAY`.

---

## 2. Real-Time Typeahead Auto-Listing

### 2.1 The `list-choices` Completion Widget
- **Widget**: `_zsh_pulse_typeahead_widget` bound to `zle -C zsh-pulse-typeahead list-choices`
- **Behavior**: Provides instant preview of completions beneath the prompt as the user types, without waiting for `Tab`.
- **Latency**: Executes in < 10 µs by querying `zsh-pulse-complete --max=16` directly in Rust, avoiding Zsh's complex `compsys` shell script cascades.

### 2.2 Re-Entrancy & Corruption Guards
Calling completion widgets while Zsh is already in menu selection (`complist.so`) causes memory corruption in Zsh's internal heap and triggers `SIGSEGV`.
To ensure safety, `_zsh_pulse_typeahead_widget` is guarded:
- Verifies `$_ZSH_PULSE_IN_TYPEAHEAD == 0`.
- Verifies `! PENDING && ! KEYS_QUEUED_COUNT`.
- Exits immediately if `is_menu_active` is true.
- Sets `compstate[insert]=''` to strictly guarantee that auto-listing never inserts text into `$BUFFER`.

---

## 3. Menu Selection & Complist Stabilization

### 3.1 C-Level Menu-Active State Inspection
Standard Zsh scripts often check `[[ $KEYMAP == menuselect ]]`. However, during menu transitions, `$KEYMAP` may lag or report `"main"` / `"emacs"`.
`shell-pulse` resolves this by querying internal Zsh C global symbols via `Api::is_menu_active`:
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

### 4.1 Tab Toggle Ergonomics (`_zsh_pulse_tab`)
- **Normal state**: Pressing `Tab` toggles completion preview. If the typeahead menu is visible, `Tab` dismisses it (`_ZSH_PULSE_MENU_HIDDEN=1`). If hidden, `Tab` re-expands completions.
- **Inside `menuselect`**: Pressing `Tab` invokes `_zsh_pulse_menuselect_abort`, restoring the original buffer and cleanly closing the menu without inserting unwanted characters.

### 4.2 Down-Arrow Selection (`down-line-or-select`)
- If a typeahead preview menu is currently visible below the prompt, pressing `Down-Arrow` (`^[[B`, `^[OB`, `^N`, Vi `j`) immediately enters `menu-select`, highlighting the first candidate for rapid navigation.
- If no completion list is visible, performs prefix history search forward.

### 4.3 History Search Navigation (`up-line-or-search` & `Ctrl-R`)
- `Up-Arrow` (`^[[A`, `^[OA`, `^P`, Vi `k`) performs Fish-style prefix history search (`up-line-or-beginning-search`) matching the text currently in `$BUFFER`.
- `Ctrl-R` automatically detects if `fzf-history-widget` or `fzf` is available. If present, it preserves and invokes the official `fzf-history-widget` without clobbering user setup. When absent (or with `ZSH_PULSE_USE_FZF=0`), it invokes native `zsh-pulse-history-search`, opening an interactive multi-line fuzzy history search menu.

### 4.4 Ghost Text Acceptance
- `Right-Arrow` (`^[[C`), `End` (`^E`), `Ctrl-F`: Appends the full `$POSTDISPLAY` suggestion to `$BUFFER`.
- `Alt-Right`, `Alt-F`, `Ctrl-Right`: Appends the next whitespace-delimited word of ghost text to `$BUFFER`.

---

## 5. Lifecycle Hooks

`shell-pulse` installs three core ZLE lifecycle hooks via `add-zle-hook-widget`:

| Hook | Widget | Role |
|---|---|---|
| `zle-line-pre-redraw` | `zsh-pulse-highlight-redraw` | Runs typeahead preview, publishes syntax highlighting, and updates ghost text. |
| `zle-line-init` | `zsh-pulse-highlight-line-init` | Resets incremental highlight cache and clears typeahead buffer state for new command lines. |
| `zle-line-finish` | `zsh-pulse-highlight-line-finish` | Tears down active typeahead flags and hides menus when command is submitted. |

