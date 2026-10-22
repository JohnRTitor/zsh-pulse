# shell-pulse — the zsh side of the plugin: the hooks.
#
# Sourced by `init.zsh` once the module has been loaded. It installs two
# ZLE hooks:
#
#   zle-line-pre-redraw  publish the highlights for the line being edited
#   zle-line-init        tell the module that a new line is starting
#
# Both are chained: if another plugin already installed a hook of the same
# name, it is called from ours rather than being replaced.

# The redraw hook.
#
# zsh calls `zle-line-pre-redraw` before every redraw of the command line,
# which is the only moment at which `region_highlight` can be updated
# (`zsh-src/Doc/Zsh/zle.yo:1164-1168`). One hook, one call: the cost is a
# single native call per redraw, not per widget and not per key.
#
# `--region-highlight` asks the module to publish the highlights. Without it
# the module only reads the line, so it can never overwrite a
# `region_highlight` array that belongs to the user or to another plugin.
shell-pulse-highlight-redraw() {
  # This runs from inside a widget, so `region_highlight` here is the ZLE
  # special parameter and not a global array of the same name.
  shell-pulse-highlight --region-highlight
  # Any hook that was already installed is called after ours.
  if (( ${+_shell_pulse_previous_pre_redraw} )); then
    "$_shell_pulse_previous_pre_redraw"
  fi
}

# The start-of-line hook.
#
# zsh empties the display array at the end of every line
# (`zsh-src/Src/Zle/zle_main.c:1398`), so the module has to be told that a
# new line is starting: otherwise it would believe it still holds the
# highlights of the previous line, and a recalled command — whose text
# matches the cached one — would be drawn with no highlighting at all.
shell-pulse-highlight-line-init() {
  shell-pulse-highlight --reset
  if (( ${+_shell_pulse_previous_line_init} )); then
    "$_shell_pulse_previous_line_init"
  fi
}

# Chain the hooks that are already installed rather than replacing them.
#
# zsh allows exactly one widget per name and `zle -N` replaces it. Other
# plugins use these hooks too, so an existing hook is kept and called from
# ours.
#
# `widgets[name]` is the function a widget runs, in the form `user:fn` for a
# user-defined widget (`zsh-src/Doc/Zsh/mod_zleparameter.yo:19-21`), so the
# prefix has to be stripped before the function can be called. A widget that
# is not user-defined — a builtin, or a completion widget — cannot be
# chained this way and is left alone.
#
# `zsh/zleparameter` provides the `widgets` array. Without it there is no
# way to see what a widget does, so the chain cannot be built — and the
# hook that was there is replaced rather than kept. That is said out loud,
# because a silently dropped hook is worse than a noisy one.
if zmodload zsh/zleparameter 2>/dev/null; then
  if (( ${+widgets[zle-line-pre-redraw]} )); then
    _shell_pulse_previous=${widgets[zle-line-pre-redraw]}
    case $_shell_pulse_previous in
      user:shell-pulse-highlight-redraw|user:shell-pulse-highlight-line-init)
        # This file has already been sourced; do not chain ourselves.
        ;;
      user:*)
        typeset -g _shell_pulse_previous_pre_redraw=${_shell_pulse_previous#user:}
        ;;
      *)
        typeset -g _shell_pulse_unchainable_pre_redraw=1
        ;;
    esac
    unset _shell_pulse_previous
  fi

  if (( ${+widgets[zle-line-init]} )); then
    _shell_pulse_previous=${widgets[zle-line-init]}
    case $_shell_pulse_previous in
      user:shell-pulse-highlight-redraw|user:shell-pulse-highlight-line-init)
        ;;
      user:*)
        typeset -g _shell_pulse_previous_line_init=${_shell_pulse_previous#user:}
        ;;
      *)
        typeset -g _shell_pulse_unchainable_line_init=1
        ;;
    esac
    unset _shell_pulse_previous
  fi
fi

# A hook that could not be chained is reported rather than dropped in
# silence: the user is losing another plugin's work.
if ! zmodload zsh/zleparameter 2>/dev/null; then
  print -u2 "shell-pulse: zsh/zleparameter is not available, so an existing" \
    "zle-line-pre-redraw hook cannot be chained and will be replaced."
elif [[ -n ${_shell_pulse_unchainable_pre_redraw:-} ]]; then
  print -u2 "shell-pulse: the existing zle-line-pre-redraw hook is not a" \
    "shell function and cannot be chained; it will be replaced."
fi

zle -N zle-line-pre-redraw shell-pulse-highlight-redraw
zle -N zle-line-init shell-pulse-highlight-line-init
