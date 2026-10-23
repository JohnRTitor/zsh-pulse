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
#
# The redraw hook also tells the module what the shell knows about the
# command word of the line. The shell — not the module — knows whether a
# word is an alias, a function, a builtin, a command on `$PATH` or a
# reserved word: those are the shell's own tables, and the module cannot
# reach them. The module colours the command word green when the shell
# knows it and red when it does not, the way `fast-syntax-highlighting`
# does.

# The shell's own tables of commands.
#
# `$aliases`, `$galiases`, `$functions`, `$builtins`, `$commands`,
# `$saliases` and `$reswords` are the shell's own answer to "do you know
# this word?". They are part of zsh's core; the `zsh/parameter` module
# provides them on the versions that do not have them, and is loaded once
# here rather than on every keystroke.
if ! (( ${+aliases} && ${+galiases} && ${+functions} && ${+builtins} \
        && ${+commands} && ${+saliases} && ${+reswords} )); then
  zmodload zsh/parameter 2>/dev/null
fi

# The answers that have been worked out already.
#
# The lookup is a hash lookup, but it is done on every keystroke, and the
# command word rarely changes while the user types the arguments — so the
# answers are remembered.
#
# The value of an entry holds the time the answer was worked out and
# whether the shell knew the word, packed into one number: the time in the
# high half, the answer in the low one.
#
# An answer is only trusted for a short while (see
# `SHELL_PULSE_COMMAND_CACHE_TTL`). A word the shell knew may stop being
# known — the user may `unalias` it or remove the function — and an answer
# that was trusted for ever would keep the word green after the shell had
# forgotten it. The same is true in the other direction: a command may be
# installed while the session runs, and a remembered "not known" would be
# wrong until the answer expired.
typeset -gA SHELL_PULSE_COMMAND_CACHE

# How long an answer is trusted.
#
# Two seconds is short enough that a change in the shell's own tables is
# picked up at once, and long enough that the lookup is not repeated on
# every keystroke of a word the user is still typing.
#
# The shell's own clock is used (`SECONDS`), which is always there: the
# module that provides a wall clock would be one more thing to load.
typeset -gi SHELL_PULSE_COMMAND_CACHE_TTL=2

# How many answers are remembered.
#
# The cache must not grow without bound over a long session. When it is
# full it is emptied, which costs one lookup per word again and nothing
# more.
typeset -gi SHELL_PULSE_COMMAND_CACHE_MAX=256

# How deep the search into command substitutions goes.
#
# A substitution inside a substitution is already unusual; four levels is
# more than a line of shell usually holds, and the bound is what keeps a
# pathological line from recursing for ever.
typeset -gi SHELL_PULSE_MAX_SUBSTITUTION_DEPTH=4

# How deep the search into command substitutions currently is.
typeset -gi SHELL_PULSE_SUBSTITUTION_DEPTH=0

# The longest line the adapter will look at.
#
# The module does not highlight a line longer than 4 KiB
# (`crates/zsh-module/src/lib.rs`, `MAX_LINE`), so the adapter must not pay
# for the split either: a pasted line of a megabyte would be split and
# walked on *every* keystroke, for a line that is never highlighted — and
# that is the difference between a slow line and an unresponsive shell.
typeset -gi SHELL_PULSE_MAX_LINE=4096

# The answers for the line being edited.
#
# One argument per command word: `--command-known=<word>` when the shell
# knows the word, `--command-unknown=<word>` when it does not.
typeset -ga SHELL_PULSE_COMMAND_ARGS

# Can the shell be asked what it knows?
#
# If `zsh/parameter` could not be loaded the tables are missing, and the
# shell cannot say whether it knows a word. The answer in that case is
# *nothing*, not "no": a word that was never asked about must not be
# painted red, because red means "the shell does not know this command",
# and the shell was never asked.
shell-pulse-highlight-can-ask() {
  (( ${+aliases} && ${+galiases} && ${+functions} && ${+builtins} \
     && ${+commands} && ${+saliases} && ${+reswords} ))
}

# The command words of the line being edited, and what the shell knows
# about them.
#
# The answers are left in `SHELL_PULSE_COMMAND_ARGS`, one argument per
# command word. Every command word is answered, not just the first: in
# `ls | grep x` there are two commands, and the second one is just as much
# a command as the first.
#
# The word in the answer is the raw text as the user typed it, byte for
# byte: the module compares it with the command word it found in the line
# and ignores an answer that does not match exactly, so a disagreement
# between the shell's word splitting and the Rust lexer can never paint the
# wrong word.
#
# The line is walked the way the shell itself walks it: `FOO=bar cmd` runs
# `cmd`, `cmd > out` runs `cmd`, and `cmd | other` runs two commands.
#
# The answer is looked up in the shell's own tables, so the lookup does not
# fork: a fork on every keystroke would be visible to the user.
shell-pulse-highlight-command() {
  emulate -L zsh
  setopt extendedglob noglob

  # No answers at all when the shell cannot be asked. The module then
  # leaves every command word alone.
  SHELL_PULSE_COMMAND_ARGS=()
  if ! shell-pulse-highlight-can-ask; then
    return 0
  fi

  # A line the module will not highlight is not worth splitting either.
  if (( ${#BUFFER} > SHELL_PULSE_MAX_LINE )); then
    return 0
  fi

  # The words of the line, as the shell itself splits them: `${(z)...}` is
  # zsh's own lexer. The words keep their quoting, which is what the
  # module has to see: it compares the word it found with the word the
  # shell was asked about, byte for byte.
  #
  # The glob is switched off for the split: a word that contains `*` must
  # not be matched against the filesystem just to be looked up.
  local -a words
  words=( ${(z)BUFFER} )

  shell-pulse-highlight-words "${words[@]}"
}

# Walk a list of words and answer for every command word among them.
#
# The line is walked the way the shell itself walks it: `FOO=bar cmd` runs
# `cmd`, `cmd > out` runs `cmd`, and `cmd | other` runs two commands.
#
# The patterns are kept in variables, and compared with the `~` flag: a
# pattern in a variable is not a pattern until it is asked to be one.
# Written out here they would be redirections — `<` and `>` are
# redirections inside `[[ ... ]]`, and `|` is a pipe — and the `#` in the
# assignment pattern would start a comment.
#
# A redirection operator is made of nothing but the characters zsh allows
# in one — a file-descriptor number, `<`, `>` and `&` — and has at least
# one `<` or `>` in it. An operator is made of nothing but `|`, `;`,
# `&`, `(` and `)`.
#
# The assignment pattern is the name of a parameter, an `=` and anything.
# The `#` says that the name may be one character long: a bracket
# expression always matches at least one character, so `*` would not do.
#
# The `+=` is an assignment as well: `FOO+=bar cmd` runs `cmd`, and the
# Rust lexer knows that (`crates/lexer/src/scanner.rs`, `looks_like_assignment`).
# If the two sides disagreed about what an assignment is, the command
# word after it would never be answered and would never be coloured.
shell-pulse-highlight-words() {
  # The options are set locally rather than emulated: the caller has
  # already emulated zsh, and `emulate` resets every option in the shell,
  # which is not free on the interactive thread.
  setopt localoptions extendedglob noglob

  local assignment='[A-Za-z_][A-Za-z0-9_]#(+=|=)*'
  local has_non_redirect_char='*[^0-9<>&]*'
  local has_redirect_char='*[<>]*'
  local has_non_operator_char='*[^|;&()]*'
  local has_ampersand_gt='*>&*'
  local has_ampersand_lt='*<&*'
  local digits='[0-9]#'

  local -a words
  words=( "$@" )

  integer i=1
  integer nwords=${#words}
  # Whether the next word is expected to start a command.
  integer want_command=1
  while (( i <= nwords )); do
    local word=${words[i]}
    if [[ -z $word ]]; then
      # An empty word — `""` is a word, but it is not a command.
      (( i++ ))
      continue
    fi
    # A word may hold a command of its own — `$(cmd)` does — and the
    # module finds that command, so it has to be answered as well. The
    # test is here rather than inside the function: a zsh function call is
    # not free, and this runs for every word of the line.
    if [[ $word == *'$('* || $word == *'`'* ]]; then
      shell-pulse-highlight-inside "$word"
    fi
    if [[ $word != ${~has_non_redirect_char} && $word == ${~has_redirect_char} ]]; then
      # A redirection: `<`, `>`, `2>`, `&>`, `2>&`...
      (( i++ ))
      if [[ $word == ${~has_ampersand_gt} || $word == ${~has_ampersand_lt} ]]; then
        # A file-descriptor duplication — `2>&1` — which writes to no
        # file. The number it duplicates is the next word, and zsh's own
        # lexer splits the two apart: `2>&1` is the words `2>&` and `1`.
        if (( i <= nwords )) && [[ ${words[i]} == ${~digits} ]]; then
          (( i++ ))
        fi
      elif (( i <= nwords )); then
        # The file it redirects to is the next word.
        (( i++ ))
      fi
      # A redirection does not end the command position: `> out cmd` still
      # runs `cmd`.
      continue
    fi
    if [[ $word != ${~has_non_operator_char} ]]; then
      # An operator — `|`, `;`, `&&`, `||`, `&` — so the next word starts
      # a new command.
      (( i++ ))
      want_command=1
      continue
    fi
    if (( want_command )); then
      if [[ $word == ${~assignment} ]]; then
        # An assignment: it sets a variable and leaves the command alone.
        # `FOO=bar cmd` runs `cmd`.
        (( i++ ))
        continue
      fi
      # This is a command word.
      shell-pulse-highlight-answer "$word"
      want_command=0
      (( i++ ))
      continue
    fi
    # An ordinary argument.
    (( i++ ))
  done
}

# Answer for the command inside a command substitution.
#
# `$(cmd)` and `` `cmd` `` hold a command of their own, and the module
# finds it: the answer has to be about the word *inside* the substitution,
# not about the whole `$(cmd)`, because that is the text the module
# compares its own command word with.
#
# Only a substitution that makes up the whole word is looked inside. The
# text between the `$(` and the `)` is then exactly the command, and the
# words of that text are found the same way the words of the line are.
shell-pulse-highlight-inside() {
  local word=$1
  local inner

  if [[ $word == '$('*')' ]]; then
    inner=${word#'$('}
    inner=${inner%')'}
  elif [[ ${#word} -gt 1 && $word == '`'*'`' ]]; then
    inner=${word#'`'}
    inner=${inner%'`'}
  else
    return 0
  fi

  # The nesting is bounded: a substitution inside a substitution inside a
  # substitution is already more than a line of shell usually holds, and an
  # unbounded recursion would be a way to hang the shell.
  (( SHELL_PULSE_SUBSTITUTION_DEPTH >= SHELL_PULSE_MAX_SUBSTITUTION_DEPTH )) && return 0
  (( SHELL_PULSE_SUBSTITUTION_DEPTH++ ))

  local -a inner_words
  inner_words=( ${(z)inner} )
  shell-pulse-highlight-words "${inner_words[@]}"

  (( SHELL_PULSE_SUBSTITUTION_DEPTH-- ))
}

# Record what the shell knows about one command word.
#
# The answer is put into `SHELL_PULSE_COMMAND_ARGS` as an argument for the
# module: `--command-known=<word>` or `--command-unknown=<word>`.
#
# The word is looked up without its quoting, because that is the word the
# shell would actually run; the answer keeps the quoting, because that is
# the text the module has to match against the line.
shell-pulse-highlight-answer() {
  local word=$1
  local -i kind
  shell-pulse-highlight-known "${(Q)word}"
  kind=$?
  case $kind in
    2) SHELL_PULSE_COMMAND_ARGS+=( "--command-reserved=$word" ) ;;
    3) SHELL_PULSE_COMMAND_ARGS+=( "--command-global-alias=$word" ) ;;
    1) SHELL_PULSE_COMMAND_ARGS+=( "--command-known=$word" ) ;;
    *) SHELL_PULSE_COMMAND_ARGS+=( "--command-unknown=$word" ) ;;
  esac
}

# Does the shell know this word as a command, and what kind is it?
#
# Return codes:
#   2 - reserved word
#   3 - global alias
#   1 - command on $PATH, builtin, function, alias, or suffix alias
#   0 - not known
#
# The answer comes from the shell's own tables, so it is exactly what the
# shell would do.
shell-pulse-highlight-known() {
  # No `emulate` here: the function only looks names up in hash tables,
  # which no option changes, and it runs on every keystroke.
  local word=$1

  # An empty word is not a command.
  if [[ -z $word ]]; then
    return 0
  fi

  # The answer that was worked out before, if it is still fresh.
  if (( ${+SHELL_PULSE_COMMAND_CACHE[(e)$word]} )); then
    local entry=${SHELL_PULSE_COMMAND_CACHE[(e)$word]}
    if (( SECONDS - entry / 4 < SHELL_PULSE_COMMAND_CACHE_TTL )); then
      return $(( entry % 4 ))
    fi
  fi

  integer kind=0
  if (( $reswords[(Ie)$word] )); then
    kind=2
  elif (( ${+galiases[(e)$word]} )); then
    kind=3
  elif (( ${+aliases[(e)$word]} )) ||
       (( ${+functions[(e)$word]} )) ||
       (( ${+builtins[(e)$word]} )) ||
       (( ${+commands[(e)$word]} )) ||
       (( ${+saliases[(e)${word##*.}]} )); then
    kind=1
  fi

  if (( ${#SHELL_PULSE_COMMAND_CACHE} >= SHELL_PULSE_COMMAND_CACHE_MAX )); then
    SHELL_PULSE_COMMAND_CACHE=()
  fi
  SHELL_PULSE_COMMAND_CACHE[(e)$word]=$(( SECONDS * 4 + kind ))

  return $kind
}

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
#
# `--command-known=<word>` and `--command-unknown=<word>` tell the module
# what the shell knows about the command words of the line. There is one
# pair per command word, because a line can hold more than one command:
# `ls | grep x` runs two.
#
# The word is part of the answer: the module applies it only if the word it
# found is the word the shell was asked about, so a disagreement between the
# shell and the lexer cannot end with the wrong word coloured red. A command
# word the shell was not asked about is left alone entirely.
shell-pulse-highlight-redraw() {
  local -a args
  shell-pulse-highlight-command
  args=( --region-highlight "${SHELL_PULSE_COMMAND_ARGS[@]}" )
  # The arguments are quoted: a command word may hold a character that is
  # a glob pattern — `*`, `?`, `[` — and with `GLOB_SUBST` set in the
  # user's configuration an unquoted expansion would be matched against
  # the filesystem, and a word with no match would abort the call.
  shell-pulse-highlight "${args[@]}"

  # Real-time ghost autosuggestion from shell-pulse (sub-10us latency)
  if (( ! PENDING && ! KEYS_QUEUED_COUNT )); then
    if [[ $CURSOR -ge $#BUFFER && -n $BUFFER ]]; then
      shell-pulse-complete --autosuggest
      if [[ -n $_shell_pulse_autosuggestion ]]; then
        POSTDISPLAY="$_shell_pulse_autosuggestion"
      else
        unset POSTDISPLAY
      fi
    elif [[ -z $BUFFER ]]; then
      unset POSTDISPLAY
    fi
  fi

  # Real-time type-ahead completion auto-listing (up to 16 lines below prompt)
  if (( ! PENDING && ! KEYS_QUEUED_COUNT )); then
    if [[ -n $BUFFER && "$BUFFER" != "$_SHELL_PULSE_LAST_TYPEAHEAD" ]]; then
      _SHELL_PULSE_LAST_TYPEAHEAD="$BUFFER"
      if [[ -n $widgets[shell-pulse-typeahead] ]]; then
        zle shell-pulse-typeahead 2>/dev/null
      fi
    elif [[ -z $BUFFER && -n $_SHELL_PULSE_LAST_TYPEAHEAD ]]; then
      _SHELL_PULSE_LAST_TYPEAHEAD=""
      zle -Rc 2>/dev/null
    fi
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
  _SHELL_PULSE_LAST_TYPEAHEAD=""
  zle -Rc 2>/dev/null
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
