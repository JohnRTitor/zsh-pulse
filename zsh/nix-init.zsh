# zsh-pulse — the entry point for a Nix installation.
#
# Source this from a `.zshrc`:
#
#     source /nix/store/…-zsh-pulse/share/zsh-pulse/init.zsh
#
# It points the adapter at the module that was built beside it and then loads
# the adapter, which installs the `zle-line-pre-redraw` hook.
#
# The paths are filled in when the package is installed.

export ZSH_PULSE_MODULE_PATH="@out@/lib"
source "@out@/share/zsh-pulse/zsh/init.zsh"
