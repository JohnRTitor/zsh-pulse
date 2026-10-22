# shell-pulse — the zsh side of the plugin.
#
# Source this from your `.zshrc`:
#
#     source /path/to/shell-pulse/zsh/init.zsh
#
# The native module must be installed as `shellpulse/highlight.so` on
# `$module_path`; `bench/install.sh` installs it where this file looks for
# it by default.
#
# Everything here degrades quietly. If the module cannot be loaded — an
# unsupported zsh, a missing symbol, a build for another architecture —
# zsh's own highlighting is used and this file does nothing.

# Where the module lives.
#
# `${${(%):-%x}:A:h:h}` is used rather than `$0` because `$0` is the *shell's*
# name when `FUNCTION_ARGZERO` is unset — which would make this file load a
# module from an arbitrary directory.
#
# Override this before sourcing to install the module somewhere else.
: ${SHELL_PULSE_MODULE_PATH:=${${(%):-%x}:A:h:h}/bench/mp}

if [[ -d $SHELL_PULSE_MODULE_PATH ]]; then
  module_path=("$SHELL_PULSE_MODULE_PATH" $module_path)
fi

if ! zmodload shellpulse/highlight 2>/dev/null; then
  # The fallback path: no module, no damage, no highlighting from us.
  return 0
fi

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
  shell-pulse-highlight --region-highlight
  # Any hook that was already installed is called after ours.
  if (( ${+_shell_pulse_previous_pre_redraw} )); then
    $_shell_pulse_previous_pre_redraw
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
    $_shell_pulse_previous_line_init
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
# way to see what a widget does, so the chain cannot be built.
if zmodload zsh/zleparameter 2>/dev/null; then
  if (( ${+widgets[zle-line-pre-redraw]} )); then
    _shell_pulse_previous=${widgets[zle-line-pre-redraw]}
    case $_shell_pulse_previous in
      user:shell-pulse-highlight-*)
        # This file has already been sourced; do not chain ourselves.
        ;;
      user:*)
        typeset -g _shell_pulse_previous_pre_redraw=${_shell_pulse_previous#user:}
        ;;
    esac
    unset _shell_pulse_previous
  fi

  if (( ${+widgets[zle-line-init]} )); then
    _shell_pulse_previous=${widgets[zle-line-init]}
    case $_shell_pulse_previous in
      user:shell-pulse-highlight-*)
        ;;
      user:*)
        typeset -g _shell_pulse_previous_line_init=${_shell_pulse_previous#user:}
        ;;
    esac
    unset _shell_pulse_previous
  fi
fi

zle -N zle-line-pre-redraw shell-pulse-highlight-redraw
zle -N zle-line-init shell-pulse-highlight-line-init
