# shell-pulse completion and autosuggestions adapter.
#
# Provides:
# 1. Native Tab completion powered by shell-pulse's bounded Rust engine:
#    - Bounded readdir: immune to large directory freezes (such as /nix/store).
#    - In-memory history matching.
#    - Fuzzy and prefix candidate ranking.
#    - Arrow-key menu selection.
# 2. Ghost text autosuggestion ($POSTDISPLAY):
#    - Real-time type-ahead suggestions from command history.
#    - Accepted instantly via Right-Arrow (forward-char) or End.

# Enable menu selection if complist is available.
zmodload zsh/complist 2>/dev/null
zstyle ':completion:*' menu select

# The native shell-pulse completion widget.
_shell_pulse_complete_widget() {
  emulate -L zsh
  setopt extendedglob noglob

  # Query the native module for ranked completion candidates.
  shell-pulse-complete --max=32

  if (( ${#_shell_pulse_matches[@]} > 0 )); then
    # Add matches with formatted descriptions.
    compadd -U -l -d _shell_pulse_descriptions -a _shell_pulse_matches
  fi
}

zle -C shell-pulse-complete complete-word _shell_pulse_complete_widget
bindkey '^I' shell-pulse-complete

# Forward-char widget that accepts inline autosuggestion when cursor is at end of line.
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
