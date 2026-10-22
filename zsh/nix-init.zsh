# shell-pulse — the entry point for a Nix installation.
#
# Source this from a `.zshrc`:
#
#     source /nix/store/…-shell-pulse/share/shell-pulse/init.zsh
#
# It points the adapter at the module that was built beside it and then loads
# the adapter, which installs the `zle-line-pre-redraw` hook.
#
# The paths are filled in when the package is installed.

export SHELL_PULSE_MODULE_PATH="@out@/lib"
source "@out@/share/shell-pulse/zsh/init.zsh"
