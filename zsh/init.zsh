# shell-pulse — the zsh side of the plugin: loading it.
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

# The hooks themselves live in their own file, so that they can be read (and
# changed) without touching the loading.
source "${${(%):-%x}:A:h}/widgets.zsh"
source "${${(%):-%x}:A:h}/completion.zsh"
