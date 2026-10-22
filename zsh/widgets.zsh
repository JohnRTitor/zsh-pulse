# shell-pulse — the zsh side of the plugin: the hooks.
#
# Sourced by `init.zsh` once the module has been loaded.
#
# Two hooks are installed:
#
#   zle-line-pre-redraw  publish the highlights for the line being edited
#   zle-line-init        tell the module that a new line is starting
#
# Both are installed with zsh's own `add-zle-hook-widget`, which keeps the
# widgets that are already registered for a hook and calls them as well. That
# matters: `zle -N` would *replace* whatever was registered, so two plugins
# using the same hook would silently disable each other — and in practice
# that is exactly what happened, because zsh-autocomplete registers a
# wrapper for `zle-line-pre-redraw` after the `.zshrc` has been read.

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
}

# Install the hooks.
#
# `add-zle-hook-widget` is zsh's own function for this
# (`Functions/Misc/add-zle-hook-widget`): it registers a widget *alongside*
# the ones already registered for the hook and calls them all, in order.
# Using `zle -N` instead would replace them, and the plugin that was
# replaced would stop working — silently, and only after the first command
# line, because that is when the other plugin installs its own wrapper.
if ! autoload -Uz add-zle-hook-widget 2>/dev/null; then
  # No `add-zle-hook-widget` (a very old zsh): fall back to registering the
  # widgets directly. Another plugin's hook for the same name is lost, which
  # is the behaviour we are trying to avoid, but there is no better option.
  zle -N zle-line-pre-redraw shell-pulse-highlight-redraw
  zle -N zle-line-init shell-pulse-highlight-line-init
else
  # `add-zle-hook-widget` *autoloads* a widget it has not seen before, on
  # the assumption that the widget is a file in `$fpath`. Our functions are
  # already defined, and autoloading them would throw their bodies away and
  # leave the shell looking for a file called `shell-pulse-highlight-redraw`.
  # Registering them as widgets first makes it leave them alone.
  zle -N shell-pulse-highlight-redraw shell-pulse-highlight-redraw
  zle -N shell-pulse-highlight-line-init shell-pulse-highlight-line-init

  add-zle-hook-widget zle-line-pre-redraw shell-pulse-highlight-redraw
  add-zle-hook-widget zle-line-init shell-pulse-highlight-line-init
fi
