# shell-pulse completion, autosuggestions, and pager adapter.
#
# Provides:
# 1. Hybrid Completer Integration:
#    - Preserves 100% of native Zsh completers (_complete: git, cargo, docker, nix, flags, subcommands).
#    - Intercepts paths with shell-pulse's bounded Rust scanner to eliminate large directory hangs (/nix/store).
# 2. Category Grouping & Columnated Pager:
#    - Clean headings ([directories], [files], [commands], [history], [options]).
#    - Arrow-key menu selection via zsh/complist.
# 3. Arrow-Key Menu Navigation (Fish & zsh-autocomplete parity):
#    - Down-Arrow (↓ / Ctrl-N): Enters active completion menu immediately.
#    - Up-Arrow (↑ / Ctrl-P): Opens interactive multi-line history search menu.
# 4. Fish-Style Autosuggestions ($POSTDISPLAY):
#    - Full line acceptance: Right-Arrow (→), End, Ctrl-F, Ctrl-E.
#    - Word-by-word acceptance: Alt-Right, Alt-F, Ctrl-Right.
# 5. Tab & Shift-Tab:
#    - Tab: Expands or enters completion menu.
#    - Shift-Tab: Cycles backwards (reverse-menu-complete).
# 6. Recent Directories:
#    - Tracks visited directories for fast jump completions.

# Silence completion beeps and prevent typeahead confirmation prompts.
unsetopt list_beep 2>/dev/null
LISTMAX=1000
zstyle ':completion:*' list-prompt ''

# Load menu selection module.
zmodload zsh/complist 2>/dev/null

# Configure completion menu styling and group headings.
# automenu allows Tab to insert unambiguous prefix first, then enter menu select.
zstyle ':completion:*' menu select automenu
zstyle ':completion:*' group-name ''
zstyle ':completion:*:descriptions' format '%B%F{yellow}[%d]%f%b'
zstyle ':completion:*:messages' format '%F{red}%d%f'
zstyle ':completion:*:warnings' format '%F{red}No matches for: %d%f'

# Real-time type-ahead completion widget (lists choices below prompt without inserting).
_shell_pulse_typeahead_widget() {
  emulate -L zsh
  setopt extendedglob noglob
  compstate[insert]=''
  compstate[list]='list force'
  _main_complete
}

zle -C shell-pulse-typeahead list-choices _shell_pulse_typeahead_widget

# -----------------------------------------------------------------------------
# 1. Hybrid Completer Integration
# -----------------------------------------------------------------------------

_shell_pulse_completer() {
  emulate -L zsh
  setopt extendedglob noglob

  # If completing an explicit path or a path fragment containing a slash or tilde,
  # use shell-pulse's bounded filesystem scanner in Rust.
  # This guarantees immune execution on massive directories like /nix/store.
  if [[ "$PREFIX" == /* || "$PREFIX" == ./* || "$PREFIX" == ../* || "$PREFIX" == ~* || "$PREFIX" == */* ]]; then
    shell-pulse-complete --paths-only --max=32
    if (( ${#_shell_pulse_matches[@]} > 0 )); then
      local -a dirs files
      local -a dir_descs file_descs
      local i
      for (( i=1; i<=${#_shell_pulse_matches[@]}; i++ )); do
        if [[ "${_shell_pulse_kinds[i]}" == "directory" ]]; then
          dirs+=("${_shell_pulse_matches[i]}")
          dir_descs+=("${_shell_pulse_descriptions[i]}")
        else
          files+=("${_shell_pulse_matches[i]}")
          file_descs+=("${_shell_pulse_descriptions[i]}")
        fi
      done
      # Directories do not append a trailing space (-S ''); files add an auto-removable space (-q -S ' ')
      (( ${#dirs[@]} > 0 )) && compadd -U -S '' -V directories -X '%B%F{blue}[directories]%f%b' -d dir_descs -a dirs
      (( ${#files[@]} > 0 )) && compadd -U -q -S ' ' -V files -X '%B%F{cyan}[files]%f%b' -d file_descs -a files
      return 0
    fi
  fi

  # Fall through to _complete, preserving git, cargo, docker, flags, and all native tools.
  return 1
}

# Prepend shell-pulse completer into Zsh's completer list.
zstyle ':completion:*' completer _shell_pulse_completer _complete _approximate

# -----------------------------------------------------------------------------
# 2. Interactive History Search Menu (Up-Arrow / Ctrl-P)
# -----------------------------------------------------------------------------

_shell_pulse_history_widget() {
  emulate -L zsh
  setopt extendedglob noglob
  shell-pulse-complete --history-only --max=32
  if (( ${#_shell_pulse_matches[@]} > 0 )); then
    IPREFIX=""
    PREFIX="$BUFFER"
    SUFFIX=""
    ISUFFIX=""
    compadd -U -V history -X '%B%F{magenta}[history search]%f%b' -d _shell_pulse_descriptions -a _shell_pulse_matches
  fi
}

zle -C shell-pulse-history-search menu-select _shell_pulse_history_widget

_shell_pulse_up_line_or_search() {
  if [[ $LBUFFER == *$'\n'* ]]; then
    zle .up-line
  else
    zle shell-pulse-history-search
  fi
}

zle -N up-line-or-search _shell_pulse_up_line_or_search

# -----------------------------------------------------------------------------
# 3. Down-Arrow / Ctrl-N: Enter Completion Menu
# -----------------------------------------------------------------------------

_shell_pulse_down_line_or_select() {
  if [[ $RBUFFER == *$'\n'* ]]; then
    zle .down-line
  else
    zle menu-select -w 2>/dev/null || zle .down-line
  fi
}

zle -N down-line-or-select _shell_pulse_down_line_or_select

# -----------------------------------------------------------------------------
# 4. Fish-Style Autosuggestion Acceptance (Full Line & Word-by-Word)
# -----------------------------------------------------------------------------

# Forward-char / Right-Arrow widget: accepts the entire ghost text at end of line.
_shell_pulse_forward_char_or_accept() {
  if [[ -n $POSTDISPLAY && $CURSOR -ge $#BUFFER ]]; then
    BUFFER="$BUFFER$POSTDISPLAY"
    CURSOR=$#BUFFER
    unset POSTDISPLAY
  else
    zle .forward-char
  fi
}

zle -N forward-char _shell_pulse_forward_char_or_accept
zle -N end-of-line _shell_pulse_forward_char_or_accept

# Forward-word / Alt-Right / Alt-F: accepts the next single word of ghost text (Fish feature).
_shell_pulse_forward_word_or_accept() {
  if [[ -n $POSTDISPLAY && $CURSOR -ge $#BUFFER ]]; then
    if [[ $POSTDISPLAY =~ '^([[:space:]]*[^[:space:]]+[[:space:]]*)' ]]; then
      local word="$MATCH"
      BUFFER="$BUFFER$word"
      CURSOR=$#BUFFER
      POSTDISPLAY="${POSTDISPLAY#$word}"
      zle -R
      return
    fi
  fi
  zle .forward-word
}

zle -N forward-word _shell_pulse_forward_word_or_accept

# -----------------------------------------------------------------------------
# 5. Key Bindings (main, emacs, viins, vicmd, menuselect)
# -----------------------------------------------------------------------------

_shell_pulse_bind_all() {
  local widget="$1"
  shift
  local key
  for key; do
    bindkey -M main "$key" "$widget" 2>/dev/null || bindkey "$key" "$widget" 2>/dev/null
    bindkey -M emacs "$key" "$widget" 2>/dev/null
    bindkey -M viins "$key" "$widget" 2>/dev/null
  done
}

# Up-Arrow / History search
_shell_pulse_bind_all up-line-or-search '^[[A' '^[OA' '^P'
bindkey -M vicmd 'k' up-line-or-search 2>/dev/null
bindkey -M vicmd '^[[A' up-line-or-search 2>/dev/null
bindkey -M vicmd '^[OA' up-line-or-search 2>/dev/null

# Down-Arrow / Menu selection
_shell_pulse_bind_all down-line-or-select '^[[B' '^[OB' '^N'
bindkey -M vicmd 'j' down-line-or-select 2>/dev/null
bindkey -M vicmd '^[[B' down-line-or-select 2>/dev/null
bindkey -M vicmd '^[OB' down-line-or-select 2>/dev/null

# Tab & Shift-Tab
_shell_pulse_bind_all expand-or-complete '^I'
_shell_pulse_bind_all reverse-menu-complete '^[[Z'
[[ -n "$terminfo[kcbt]" ]] && _shell_pulse_bind_all reverse-menu-complete "$terminfo[kcbt]"
bindkey -M menuselect '^[[Z' reverse-menu-complete 2>/dev/null
[[ -n "$terminfo[kcbt]" ]] && bindkey -M menuselect "$terminfo[kcbt]" reverse-menu-complete 2>/dev/null

# Ghost text acceptance
_shell_pulse_bind_all forward-char '^[[C' '^[OC' '^F'
_shell_pulse_bind_all end-of-line '^E'
_shell_pulse_bind_all forward-word '^[f' '^[^[[C' '^[[1;3C' '^[[1;5C'

unfunction _shell_pulse_bind_all 2>/dev/null

# -----------------------------------------------------------------------------
# 6. Recent Directories Tracking
# -----------------------------------------------------------------------------

autoload -Uz chpwd_recent_dirs cdr add-zsh-hook 2>/dev/null
if (( $+functions[chpwd_recent_dirs] )); then
  add-zsh-hook chpwd chpwd_recent_dirs
fi
