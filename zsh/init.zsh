# zsh-pulse — the zsh side of the plugin: loading it.
#
# Source this from your `.zshrc`:
#
#     source /path/to/zsh-pulse/zsh/init.zsh
#
# The native module must be installed as `zshpulse/core.so` on
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
: ${ZSH_PULSE_MODULE_PATH:=${${(%):-%x}:A:h:h}/bench/mp}

if [[ -d $ZSH_PULSE_MODULE_PATH ]]; then
  module_path=("$ZSH_PULSE_MODULE_PATH" $module_path)
fi

if ! zmodload zshpulse/core 2>/dev/null; then
  # The fallback path: no module, no damage, no highlighting from us.
  return 0
fi

# The hooks themselves live in their own file, so that they can be read (and
# changed) without touching the loading.
#
# Feature toggles (set to 0, false, off, or no to disable):
#   ZSH_PULSE_HIGHLIGHT    - Syntax highlighting (default: 1)
#   ZSH_PULSE_COMPLETION   - Typeahead completion, bounded scanner & menu (default: 1)
#   ZSH_PULSE_HISTORY      - Up/Down history search & Ctrl-R interactive menu (default: 1)
#   ZSH_PULSE_AUTOSUGGEST  - Fish-style ghost text autosuggestions (default: 1)

if [[ ${ZSH_PULSE_HIGHLIGHT:-1} != (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) || \
      ${ZSH_PULSE_COMPLETION:-1} != (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) || \
      ${ZSH_PULSE_AUTOSUGGEST:-1} != (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) ]]; then
  source "${${(%):-%x}:A:h}/widgets.zsh"
fi

if [[ ${ZSH_PULSE_COMPLETION:-1} != (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) || \
      ${ZSH_PULSE_HISTORY:-1} != (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) ]]; then
  source "${${(%):-%x}:A:h}/completion.zsh"
fi
