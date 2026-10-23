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

# Silence completion beeps.
unsetopt list_beep 2>/dev/null

# Load menu selection module.
zmodload zsh/complist 2>/dev/null

# Configure completion menu styling and group headings.
zstyle ':completion:*' menu select
zstyle ':completion:*' group-name ''
zstyle ':completion:*:descriptions' format '%B%F{yellow}[%d]%f%b'
zstyle ':completion:*:messages' format '%F{red}%d%f'
zstyle ':completion:*:warnings' format '%F{red}No matches for: %d%f'

# -----------------------------------------------------------------------------
# 1. Hybrid Completer Integration
# -----------------------------------------------------------------------------

_shell_pulse_completer() {
  emulate -L zsh
  setopt extendedglob noglob

  # If completing an explicit path or a path fragment containing a slash,
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
      (( ${#dirs[@]} > 0 )) && compadd -U -V directories -X '%B%F{blue}[directories]%f%b' -d dir_descs -a dirs
      (( ${#files[@]} > 0 )) && compadd -U -V files -X '%B%F{cyan}[files]%f%b' -d file_descs -a files
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
bindkey '^[[A' up-line-or-search
bindkey '^[OA' up-line-or-search
bindkey '^P' up-line-or-search

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
bindkey '^[[B' down-line-or-select
bindkey '^[OB' down-line-or-select
bindkey '^N' down-line-or-select

# -----------------------------------------------------------------------------
# 4. Tab & Shift-Tab Bindings
# -----------------------------------------------------------------------------

bindkey '^I' expand-or-complete
bindkey "$terminfo[kcbt]" reverse-menu-complete 2>/dev/null
bindkey '^[[Z' reverse-menu-complete

# -----------------------------------------------------------------------------
# 5. Fish-Style Autosuggestion Acceptance (Full Line & Word-by-Word)
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
bindkey '^[[C' forward-char
bindkey '^[OC' forward-char
bindkey '^F' forward-char
bindkey '^E' end-of-line

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
bindkey '^[f' forward-word
bindkey '^[^[[C' forward-word
bindkey '^[[1;3C' forward-word
bindkey '^[[1;5C' forward-word

# -----------------------------------------------------------------------------
# 6. Recent Directories Tracking
# -----------------------------------------------------------------------------

autoload -Uz chpwd_recent_dirs cdr add-zsh-hook 2>/dev/null
if (( $+functions[chpwd_recent_dirs] )); then
  add-zsh-hook chpwd chpwd_recent_dirs
fi
