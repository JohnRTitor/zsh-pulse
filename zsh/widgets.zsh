# zsh-pulse — the zsh side of the plugin: the hooks.
#
# Sourced by `init.zsh` once the module has been loaded.

# If neither highlight nor completion builtins are compiled into the module, exit cleanly.
if (( ! $+builtins[zsh-pulse-highlight] && ! $+builtins[zsh-pulse-complete] )); then
  return 0
fi

# If highlighting, typeahead completion, and autosuggestions are all disabled, exit cleanly.
if [[ ${ZSH_PULSE_HIGHLIGHT:-1} == (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) && \
      ${ZSH_PULSE_COMPLETION:-1} == (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) && \
      ${ZSH_PULSE_AUTOSUGGEST:-1} == (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) ]]; then
  return 0
fi
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
# reach them. The module colours the command word cyan when the shell
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
# `ZSH_PULSE_COMMAND_CACHE_TTL`). A word the shell knew may stop being
# known — the user may `unalias` it or remove the function — and an answer
# that was trusted for ever would keep the word cyan after the shell had
# forgotten it. The same is true in the other direction: a command may be
# installed while the session runs, and a remembered "not known" would be
# wrong until the answer expired.
typeset -gA ZSH_PULSE_COMMAND_CACHE

# How long an answer is trusted.
#
# Two seconds is short enough that a change in the shell's own tables is
# picked up at once, and long enough that the lookup is not repeated on
# every keystroke of a word the user is still typing.
#
# The shell's own clock is used (`SECONDS`), which is always there: the
# module that provides a wall clock would be one more thing to load.
typeset -gi ZSH_PULSE_COMMAND_CACHE_TTL=2

# How many answers are remembered.
#
# The cache must not grow without bound over a long session. When it is
# full it is emptied, which costs one lookup per word again and nothing
# more.
typeset -gi ZSH_PULSE_COMMAND_CACHE_MAX=256

# How deep the search into command substitutions goes.
#
# A substitution inside a substitution is already unusual; four levels is
# more than a line of shell usually holds, and the bound is what keeps a
# pathological line from recursing for ever.
typeset -gi ZSH_PULSE_MAX_SUBSTITUTION_DEPTH=4

# How deep the search into command substitutions currently is.
typeset -gi ZSH_PULSE_SUBSTITUTION_DEPTH=0

# How deep the search into a CLI subcommand goes.
#
# The first level is the subcommand itself (`git status`); the second is a
# member of a selected group (`git remote add`). Two levels cover the
# common CLI grammars without turning the adapter into a command schema.
typeset -gi ZSH_PULSE_MAX_SUBCOMMAND_DEPTH=2

#
# The module does not highlight a line longer than 4 KiB
# (`crates/zsh-module/src/lib.rs`, `MAX_LINE`), so the adapter must not pay
# for the split either: a pasted line of a megabyte would be split and
# walked on *every* keystroke, for a line that is never highlighted — and
# that is the difference between a slow line and an unresponsive shell.
typeset -gi ZSH_PULSE_MAX_LINE=4096

# The answers for the line being edited.
#
# One argument per command word: `--command-known=<word>` when the shell
# knows the word, `--command-unknown=<word>` when it does not.
typeset -ga ZSH_PULSE_COMMAND_ARGS

# Can the shell be asked what it knows?
#
# If `zsh/parameter` could not be loaded the tables are missing, and the
# shell cannot say whether it knows a word. The answer in that case is
# *nothing*, not "no": a word that was never asked about must not be
# painted red, because red means "the shell does not know this command",
# and the shell was never asked.
zsh-pulse-highlight-can-ask() {
  (( ${+aliases} && ${+galiases} && ${+functions} && ${+builtins} \
     && ${+commands} && ${+saliases} && ${+reswords} ))
}

# The command words of the line being edited, and what the shell knows
# about them.
#
# The answers are left in `ZSH_PULSE_COMMAND_ARGS`, one argument per
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
zsh-pulse-highlight-command() {
  emulate -L zsh
  setopt extendedglob noglob

  # No answers at all when the shell cannot be asked. The module then
  # leaves every command word alone.
  ZSH_PULSE_COMMAND_ARGS=()
  if ! zsh-pulse-highlight-can-ask; then
    return 0
  fi

  # A line the module will not highlight is not worth splitting either.
  if (( ${#BUFFER} > ZSH_PULSE_MAX_LINE )); then
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

  zsh-pulse-highlight-words "${words[@]}"
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
zsh-pulse-highlight-words() {
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
  # Whether the current simple command is a precommand — `sudo`, `env`,
  # `command`, ... — whose own command word is still to come.
  integer in_precommand=0
  # Whether `--` has ended the precommand's options.
  integer precommand_end_of_options=0
  # The CLI family whose subcommand is still to come, or empty when no
  # subcommand is expected.
  local subcommand_family=''
  # How many subcommand levels have been found: 0 is the first level, 1 is
  # a member of a selected group.
  integer subcommand_depth=0
  while (( i <= nwords )); do
    local word=${words[i]}
    if [[ -z $word ]]; then
      # An empty word — `""` is a word, but it is not a command.
      (( i++ ))
      continue
    fi
    # A word may hold a command of its own — `$(cmd)`, backticks, or a
    # process substitution `<(cmd)`/`>(cmd)` — and the module finds that
    # command, so it has to be answered as well. The test is here rather than
    # inside the function: a zsh function call is not free, and this runs for
    # every word of the line.
    if [[ $word == *'$('* || $word == *'`'* || $word == '<('*')' || $word == '>('*')' ]]; then
      zsh-pulse-highlight-inside "$word"
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
      in_precommand=0
      precommand_end_of_options=0
      subcommand_family=''
      subcommand_depth=0
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
      zsh-pulse-highlight-answer "$word"
      want_command=0
      subcommand_family=''
      subcommand_depth=0
      if zsh-pulse-highlight-is-precommand "$word"; then
        # `sudo true` runs `true`. Every word after the precommand is a
        # candidate for being the command it runs, so every non-option
        # word is answered until the next operator. The module knows
        # which of them is really the command: it parses the options and
        # their values. The adapter deliberately does not, because an
        # option table in shell code would be a second, slower copy of
        # the one in `precommand.rs`.
        in_precommand=1
        precommand_end_of_options=0
      else
        # A command family whose first argument is a subcommand. The
        # group table is not consulted here: a group is never a command
        # (`git remote` without `git` is not a command).
        local command_name=${(Q)word}
        if [[ -n ${ZSH_PULSE_SUBCOMMAND_FAMILIES[$command_name]:-} ]]; then
          subcommand_family=$command_name
        fi
      fi
      (( i++ ))
      continue
    fi
    if (( in_precommand )); then
      if [[ $word == '--' ]]; then
        precommand_end_of_options=1
        (( i++ ))
        continue
      fi
      if (( ! precommand_end_of_options )) && [[ $word == -* && $word != - ]]; then
        # An option. Its value — if it takes one — is answered by the
        # next turn; the module ignores an answer about a word that is
        # not the command.
        (( i++ ))
        continue
      fi
      if [[ $word == ${~assignment} ]]; then
        # `env FOO=bar cmd` sets a variable and runs `cmd`.
        (( i++ ))
        continue
      fi
      # A word the precommand may run — or one of its arguments. The
      # module decides which; an answer about an argument is harmless,
      # because the module never asks for one there.
      zsh-pulse-highlight-answer "$word"
      (( i++ ))
      continue
    fi
    if [[ -n $subcommand_family ]]; then
      if [[ $word == '--' ]]; then
        # The end of the options. What follows is an operand, not the
        # subcommand: `git -- status` does not run `git status`.
        subcommand_family=''
        (( i++ ))
        continue
      fi
      if [[ $word == +* && $subcommand_family == cargo ]]; then
        # `cargo +nightly build` selects a toolchain before the
        # subcommand; it is not the subcommand.
        (( i++ ))
        continue
      fi
      if [[ $word == -* && $word != - ]]; then
        # An option. When it takes its value in the next word, that value
        # is not the subcommand either (`git -C /tmp status`).
        if zsh-pulse-highlight-option-takes-value "$subcommand_family" "$word"; then
          (( i++ ))
        fi
        (( i++ ))
        continue
      fi
      if zsh-pulse-highlight-is-subcommand "$subcommand_family" "$word"; then
        ZSH_PULSE_COMMAND_ARGS+=( "--subcommand=$word" )
        if (( subcommand_depth < ZSH_PULSE_MAX_SUBCOMMAND_DEPTH )) \
           && zsh-pulse-highlight-is-subcommand-group "$subcommand_family" "$word"; then
          # `git remote add`: the group is a subcommand, and its members
          # are subcommands too.
          subcommand_family+=" ${(Q)word}"
          (( subcommand_depth++ ))
          (( i++ ))
          continue
        fi
      fi
      # Not an allowed subcommand, or a leaf that has no allowed
      # subcommands of its own. Nothing later in this simple command can
      # be a subcommand.
      subcommand_family=''
      (( i++ ))
      continue
    fi

    # An ordinary argument.
    (( i++ ))
  done
}

# Whether the word is a command that runs another command.
#
# The list is zsh-patina's (`assets/Packages/ShellScript/Bash.sublime-syntax`
# in the reference tree), and it is kept here because the adapter is what
# decides how far the command position extends. The *meaning* of each
# precommand — which options take a value, where its command word is — is
# in the module (`crates/zsh-module/src/precommand.rs`), so the two lists
# have to agree only about the names.
#
# `time` and `nocorrect` are reserved words rather than commands, but they
# still stand in front of a command and the word after them runs.
zsh-pulse-highlight-is-precommand() {
  case ${(Q)1} in
    (-|builtin|command|exec|doas|env|nice|nohup|noglob|sudo|sudoedit|time|nocorrect)
      return 0
      ;;
  esac
  return 1
}

# The command families whose first argument is a subcommand.
#
# This is knowledge about command-line tools, not about the shell: no
# shell table can say that `git status` has a subcommand. The list is
# deliberately bounded — five families most people type every day, not a
# catalogue of every CLI — and a word that is not in it is left alone.
#
# The adapter answers a word only when it is in the family's list. The
# module then applies the answer to that word and nothing else, so a
# command whose grammar this table does not know simply keeps the
# unhighlighted argument it had before.
typeset -gA ZSH_PULSE_SUBCOMMAND_FAMILIES
ZSH_PULSE_SUBCOMMAND_FAMILIES=(
  git 'add am apply archive bisect blame branch bundle checkout cherry-pick
       clean clone commit config describe diff difftool fetch format-patch
       fsck gc grep help init log maintenance merge mergetool mv notes pull
       push range-diff rebase reflog remote reset restore revert rm shortlog
       show sparse-checkout stash status submodule switch tag worktree'
  cargo 'add bench build check clean clippy doc fetch fix fmt generate-lockfile
         help info init install locate-project login logout metadata new owner
         package pkgid publish read-manifest remove report rm rustc rustdoc
         search test tree uninstall update vendor verify-project version yank'
  docker 'attach build builder commit compose container context cp create diff
          events exec export history image images import info inspect kill load
          login logout logs manifest network node pause plugin port ps pull push
          rename restart rm rmi run save search secret service stack start stats
          stop swarm system tag top trust unpause update version volume wait'
  kubectl 'annotate api-resources api-versions apply attach auth autoscale
           certificate cluster-info completion config cordon cp create delete
           describe diff drain edit exec explain expose get kustomize label logs
           options patch plugin port-forward proxy replace rollout run scale set
           taint top uncordon version wait'
  npm 'access adduser audit bugs cache ci clean-install completion config dedupe
       deprecate diff dist-tag docs doctor edit exec explain explore find-dupes
       fund help init install install-ci-test install-test link login logout ls
       org outdated owner pack ping pkg prefix profile prune publish query
       rebuild repo restart root run-script search set-script shrinkwrap star
       stars start stop team test token uninstall unlink unpublish update
       version view whoami'
)

# Selected groups whose first-level subcommand has subcommands of its own.
#
# `git remote add` is three words, and `remote` and `add` are both
# subcommands. The table is keyed by the family and the first-level word;
# only the pairs listed here are searched for a second level.
typeset -gA ZSH_PULSE_SUBCOMMAND_GROUPS
ZSH_PULSE_SUBCOMMAND_GROUPS=(
  'git remote' 'add get-url prune remove rename set-head set-url show update'
  'git stash' 'apply branch clear create drop list pop push save show store'
  'git config' 'edit get list remove-section rename-section set unset'
  'git submodule' 'add deinit foreach init set-branch set-url status summary sync update'
  'git worktree' 'add list lock move prune remove repair unlock'
  'git bisect' 'bad good log next replay reset run skip start terms visualize'
  'git notes' 'add append copy edit list merge prune remove show'
  'git sparse-checkout' 'add check-rules disable init list reapply set'
  'docker builder' 'build prune'
  'docker buildx' 'bake build create du inspect ls prune rm stop use version'
  'docker compose' 'build config cp create down events exec images kill logs ls pause port ps pull push restart rm run start stop top unpause up version wait'
  'docker container' 'attach commit cp create diff exec export inspect kill logs ls pause port prune rename restart rm run start stats stop top unpause update wait'
  'docker context' 'create export import inspect ls rm update use'
  'docker image' 'build history import inspect load ls prune pull push rm save tag'
  'docker manifest' 'annotate create inspect push rm'
  'docker network' 'connect create disconnect inspect ls prune rm'
  'docker plugin' 'create disable enable inspect install ls push rm set upgrade'
  'docker secret' 'create inspect ls rm'
  'docker service' 'create inspect logs ls ps rm rollback scale update'
  'docker stack' 'deploy ls ps rm services'
  'docker swarm' 'ca init join leave unlock unlock-key update'
  'docker system' 'df events info prune'
  'docker volume' 'create inspect ls prune rm'
  'kubectl auth' 'can-i reconcile'
  'kubectl certificate' 'approve deny'
  'kubectl config' 'current-context delete-cluster delete-context delete-user get-clusters get-contexts get-users rename-context set set-cluster set-context set-credentials unset use-context view'
  'kubectl rollout' 'history pause restart resume status undo'
)

# The options that take a value as the next word, per family.
#
# The adapter skips an option before looking for the subcommand, because
# `git -C /tmp status` has its subcommand after the option's value. A
# short option is listed one character at a time; a value attached to the
# option (`-C/tmp`) and a long option with `=` (`--git-dir=/tmp`) are
# self-contained and need no entry here.
#
# This table is not an option schema. It covers the global options that
# commonly stand between the command and its subcommand; an option it does
# not know is treated as one without a value, which may hide a subcommand
# behind an option's value. A missing highlight is the safe direction.
typeset -gA ZSH_PULSE_SUBCOMMAND_SHORT_OPTIONS
ZSH_PULSE_SUBCOMMAND_SHORT_OPTIONS=(
  git 'Cc'
  cargo 'ZFjp'
  docker 'Hl'
  kubectl 'nsv'
  npm 'w'
)

typeset -gA ZSH_PULSE_SUBCOMMAND_LONG_OPTIONS
ZSH_PULSE_SUBCOMMAND_LONG_OPTIONS=(
  git '--git-dir --work-tree --namespace --super-prefix --config-env --attr-source'
  cargo '--manifest-path --config --target-dir --color --message-format
         --jobs --features --profile --target --bin --example --test --bench
         --package --edition --rust-version'
  docker '--host --config --log-level --context --tlscacert --tlscert --tlskey'
  kubectl '--kubeconfig --context --cluster --user --namespace --server --token
           --certificate-authority --client-certificate --client-key
           --request-timeout --as --as-group --as-uid --cache-dir
           --tls-server-name --username --password --v --vmodule'
  npm '--prefix --registry --cache --userconfig --loglevel --workspace --omit
       --include --save-prefix --tag --otp --scope --location --before --since'
)

# The same tables, as sets of `family/name` keys.
#
# The lists above are the readable form; a set turns "is this word in the
# list?" into one associative lookup. The set is built once, when the
# adapter is sourced.
typeset -gA ZSH_PULSE_SUBCOMMANDS
typeset -gA ZSH_PULSE_SUBCOMMAND_LONG_OPTION_NAMES

zsh-pulse-highlight-init-subcommands() {
  emulate -L zsh
  setopt extendedglob noglob

  local family name key
  for family in ${(k)ZSH_PULSE_SUBCOMMAND_FAMILIES}; do
    for name in ${=ZSH_PULSE_SUBCOMMAND_FAMILIES[$family]}; do
      key="$family/$name"
      ZSH_PULSE_SUBCOMMANDS[$key]=1
    done
  done
  for family in ${(k)ZSH_PULSE_SUBCOMMAND_GROUPS}; do
    for name in ${=ZSH_PULSE_SUBCOMMAND_GROUPS[$family]}; do
      key="$family/$name"
      ZSH_PULSE_SUBCOMMANDS[$key]=1
    done
  done
  for family in ${(k)ZSH_PULSE_SUBCOMMAND_LONG_OPTIONS}; do
    for name in ${=ZSH_PULSE_SUBCOMMAND_LONG_OPTIONS[$family]}; do
      key="$family/$name"
      ZSH_PULSE_SUBCOMMAND_LONG_OPTION_NAMES[$key]=1
    done
  done
}
zsh-pulse-highlight-init-subcommands

# Whether the word is a subcommand of the family.
#
# The word is compared without its quoting, because `git "status"` runs
# `git status`; the answer keeps the quoting, so that the module can match
# it against the word in the line.
zsh-pulse-highlight-is-subcommand() {
  setopt localoptions extendedglob
  local family=$1 name=${(Q)2}
  local key="$family/$name"
  [[ -n ${ZSH_PULSE_SUBCOMMANDS[$key]:-} ]]
}

# Whether the word is a group whose members are subcommands too.
zsh-pulse-highlight-is-subcommand-group() {
  setopt localoptions extendedglob
  local family=$1 name=${(Q)2}
  local key="$family $name"
  [[ -n ${ZSH_PULSE_SUBCOMMAND_GROUPS[$key]:-} ]]
}

# Whether the option takes its value as the next word.
#
# A bundled short option ends the bundle where its value begins:
# `-C/tmp` carries the value, `-nC /tmp` does not.
zsh-pulse-highlight-option-takes-value() {
  setopt localoptions extendedglob
  local family=$1 word=$2 key
  # A group's options are the family's global options (`git remote -v`).
  family=${family%% *}

  if [[ $word == --* ]]; then
    # `--name=value` carries its own value.
    [[ $word == *=* ]] && return 1
    key="$family/$word"
    [[ -n ${ZSH_PULSE_SUBCOMMAND_LONG_OPTION_NAMES[$key]:-} ]]
    return
  fi

  local short=${ZSH_PULSE_SUBCOMMAND_SHORT_OPTIONS[$family]:-}
  local characters=${word#-}
  local index character
  for (( index = 1; index <= ${#characters}; index++ )); do
    character=${characters[index]}
    # The table holds letters only; anything else cannot be a value-taking
    # option, and treating it as a pattern would match too much.
    [[ $character == [A-Za-z] ]] || continue
    if [[ $short == *$character* ]]; then
      # A value attached to the last character is part of this word; a
      # value-taking option with nothing after it consumes the next word.
      (( index == ${#characters} ))
      return
    fi
  done
  return 1
}

# Answer for the command inside a command or process substitution.
#
# `$(cmd)`, `` `cmd` ``, `<(cmd)`, and `>(cmd)` hold a command of their own,
# and the module finds it: the answer has to be about the word *inside* the
# substitution, not about the whole substitution, because that is the text the
# module compares its own command word with.
#
# Only a substitution that makes up the whole word is looked inside. The
# text between the opener and the final `)`/backtick is then walked the same
# way the words of the line are.
zsh-pulse-highlight-inside() {
  local word=$1
  local inner

  if [[ $word == '$('*')' ]]; then
    inner=${word#'$('}
    inner=${inner%')'}
  elif [[ ${#word} -gt 1 && $word == '`'*'`' ]]; then
    inner=${word#'`'}
    inner=${inner%'`'}
  elif [[ $word == '<('*')' ]]; then
    inner=${word#'<('}
    inner=${inner%')'}
  elif [[ $word == '>('*')' ]]; then
    inner=${word#'>('}
    inner=${inner%')'}
  else
    return 0
  fi

  # The nesting is bounded: a substitution inside a substitution inside a
  # substitution is already more than a line of shell usually holds, and an
  # unbounded recursion would be a way to hang the shell.
  (( ZSH_PULSE_SUBSTITUTION_DEPTH >= ZSH_PULSE_MAX_SUBSTITUTION_DEPTH )) && return 0
  (( ZSH_PULSE_SUBSTITUTION_DEPTH++ ))

  local -a inner_words
  inner_words=( ${(z)inner} )
  zsh-pulse-highlight-words "${inner_words[@]}"

  (( ZSH_PULSE_SUBSTITUTION_DEPTH-- ))
}

# Record what the shell knows about one command word.
#
# The answer is put into `ZSH_PULSE_COMMAND_ARGS` as an argument for the
# module: `--command-known=<word>` or `--command-unknown=<word>`.
#
# The word is looked up without its quoting, because that is the word the
# shell would actually run; the answer keeps the quoting, because that is
# the text the module has to match against the line.
zsh-pulse-highlight-answer() {
  local word=$1
  local -i kind
  # `-` is the precommand that runs a command with the default options.
  # It is not in any of the shell's tables, so the lookup would call it
  # unknown and paint it red; it is a keyword of the grammar, and is
  # answered as one.
  if [[ $word == '-' ]]; then
    ZSH_PULSE_COMMAND_ARGS+=( "--command-reserved=$word" )
    return 0
  fi
  zsh-pulse-highlight-known "${(Q)word}"
  kind=$?
  case $kind in
    2) ZSH_PULSE_COMMAND_ARGS+=( "--command-reserved=$word" ) ;;
    3) ZSH_PULSE_COMMAND_ARGS+=( "--command-global-alias=$word" ) ;;
    1) ZSH_PULSE_COMMAND_ARGS+=( "--command-known=$word" ) ;;
    *) ZSH_PULSE_COMMAND_ARGS+=( "--command-unknown=$word" ) ;;
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
zsh-pulse-highlight-known() {
  # No `emulate` here: the function only looks names up in hash tables,
  # which no option changes, and it runs on every keystroke.
  local word=$1

  # An empty word is not a command.
  if [[ -z $word ]]; then
    return 0
  fi

  # The answer that was worked out before, if it is still fresh.
  if (( ${+ZSH_PULSE_COMMAND_CACHE[(e)$word]} )); then
    local entry=${ZSH_PULSE_COMMAND_CACHE[(e)$word]}
    if (( SECONDS - entry / 4 < ZSH_PULSE_COMMAND_CACHE_TTL )); then
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

  if (( ${#ZSH_PULSE_COMMAND_CACHE} >= ZSH_PULSE_COMMAND_CACHE_MAX )); then
    ZSH_PULSE_COMMAND_CACHE=()
  fi
  ZSH_PULSE_COMMAND_CACHE[(e)$word]=$(( SECONDS * 4 + kind ))

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
typeset -g _ZSH_PULSE_IN_TYPEAHEAD=0

zsh-pulse-highlight-redraw() {
  # Self-heal menu active state if complist has exited or aborted
  if [[ $KEYMAP != menuselect ]]; then
    if (( ! $+builtins[zsh-pulse-complete] )) || ! zsh-pulse-complete --is-menu-active 2>/dev/null; then
      _ZSH_PULSE_MENU_ACTIVE=0
    fi
  fi

  # When menu selection or completion is active, immediately unset ghost autosuggestions
  # and skip both typeahead and highlight mutation to prevent screen flicker and cursor desynchronization.
  if [[ $KEYMAP == menuselect || $_ZSH_PULSE_MENU_ACTIVE == 1 ]] ||
     { (( $+builtins[zsh-pulse-complete] )) && zsh-pulse-complete --is-menu-active 2>/dev/null; }; then
    unset POSTDISPLAY
    return 0
  fi

  # Reset history searching state if any non-history-search widget was invoked
  if [[ ${LASTWIDGET##.} != (up-line-or-search|down-line-or-select) ]]; then
    _zsh_pulse_hist_searching=0
  fi

  # Real-time type-ahead completion auto-listing below prompt (like zsh-autocomplete).
  # MUST NEVER run if:
  # 1. We are already inside a typeahead widget call (prevent re-entrancy recursion).
  # 2. We are in menu selection (KEYMAP == menuselect, or localkeymap/menucmp).
  # 3. A completion/menu/search widget is currently running or was the last widget.
  # Calling any completion widget while complist.so is in menu-select corrupts Zsh's C heap and causes SIGSEGV.
  if [[ ${ZSH_PULSE_COMPLETION:-1} != (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) ]]; then
    if (( ! PENDING && ! KEYS_QUEUED_COUNT && ${+functions[compdef]} )); then
      if (( _ZSH_PULSE_IN_TYPEAHEAD == 0 )); then
        # `$WIDGET` is this hook's own name while the hook runs, so it cannot
        # tell us which widget triggered the redraw: it is always
        # `zsh-pulse-highlight-redraw` here. Excluding `zsh-pulse-*` on
        # `$WIDGET` therefore excluded the hook itself and silently disabled
        # typeahead. `$LASTWIDGET` is the widget that caused the redraw, and
        # that is the one the guard must look at.
        if [[ ${LASTWIDGET##.} != (menu-select|*menu*|*complete*|zsh-pulse-tab|zsh-pulse-history-search|*select*|*search*) ]]; then
          if [[ -n $BUFFER && "$BUFFER" != "$_ZSH_PULSE_LAST_TYPEAHEAD" ]]; then
            _ZSH_PULSE_LAST_TYPEAHEAD="$BUFFER"
            if [[ -n $widgets[zsh-pulse-typeahead] ]]; then
              local -a _saved_rh=( "${region_highlight[@]}" )
              _ZSH_PULSE_IN_TYPEAHEAD=1
              zle zsh-pulse-typeahead 2>/dev/null
              _ZSH_PULSE_IN_TYPEAHEAD=0
              region_highlight=( "${_saved_rh[@]}" )
            fi
          elif [[ -z $BUFFER && -n $_ZSH_PULSE_LAST_TYPEAHEAD ]]; then
            _ZSH_PULSE_LAST_TYPEAHEAD=""
            zle -Rc 2>/dev/null
          fi
        fi
      fi
    fi
  fi

  # Native Rust syntax highlighting (sub-microsecond)
  # ALWAYS executed after typeahead so region_highlight is published freshly for ZLE redraw.
  if [[ ${ZSH_PULSE_HIGHLIGHT:-1} != (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) ]] && (( $+builtins[zsh-pulse-highlight] )); then
    local -a args
    zsh-pulse-highlight-command
    args=( --region-highlight "${ZSH_PULSE_COMMAND_ARGS[@]}" )
    # The arguments are quoted: a command word may hold a character that is
    # a glob pattern — `*`, `?`, `[` — and with `GLOB_SUBST` set in the
    # user's configuration an unquoted expansion would be matched against
    # the filesystem, and a word with no match would abort the call.
    zsh-pulse-highlight "${args[@]}"
  fi

  # Real-time ghost autosuggestion from zsh-pulse (sub-10us latency)
  if [[ ${ZSH_PULSE_AUTOSUGGEST:-1} != (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) ]]; then
    if (( ! PENDING && ! KEYS_QUEUED_COUNT && ! _zsh_pulse_hist_searching )); then
      if [[ $CURSOR -ge $#BUFFER && -n $BUFFER ]] && (( $+builtins[zsh-pulse-complete] )); then
        zsh-pulse-complete --autosuggest 2>/dev/null
        if [[ -n $_zsh_pulse_autosuggestion ]]; then
          POSTDISPLAY="$_zsh_pulse_autosuggestion"
        else
          unset POSTDISPLAY
        fi
      else
        unset POSTDISPLAY
      fi
    else
      unset POSTDISPLAY
    fi
  else
    unset POSTDISPLAY
  fi
}

# The end-of-line hook: cleanly tear down typeahead state when command is submitted.
zsh-pulse-highlight-line-finish() {
  _ZSH_PULSE_LAST_TYPEAHEAD=""
  _ZSH_PULSE_IN_TYPEAHEAD=0
  _ZSH_PULSE_TYPEAHEAD_VISIBLE=0
  _ZSH_PULSE_MENU_ACTIVE=0
  _zsh_pulse_hist_searching=0
  _zsh_pulse_hist_orig_buffer=""
  _zsh_pulse_hist_orig_no=0
  _zsh_pulse_hist_cursor=0
}

# The start-of-line hook.
zsh-pulse-highlight-line-init() {
  if [[ -n $_ZSH_PULSE_LAST_TYPEAHEAD ]]; then
    _ZSH_PULSE_LAST_TYPEAHEAD=""
    zle -Rc 2>/dev/null
  fi
  _ZSH_PULSE_TYPEAHEAD_VISIBLE=0
  _ZSH_PULSE_IN_TYPEAHEAD=0
  _ZSH_PULSE_MENU_ACTIVE=0
  _zsh_pulse_hist_searching=0
  _zsh_pulse_hist_orig_buffer=""
  _zsh_pulse_hist_orig_no=0
  _zsh_pulse_hist_cursor=0
  zsh-pulse-highlight --reset
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
  zle -N zle-line-pre-redraw zsh-pulse-highlight-redraw
  zle -N zle-line-init zsh-pulse-highlight-line-init
  zle -N zle-line-finish zsh-pulse-highlight-line-finish
else
  # `add-zle-hook-widget` *autoloads* a widget it has not seen before, on
  # the assumption that the widget is a file in `$fpath`. Our functions are
  # already defined, and autoloading them would throw their bodies away and
  # leave the shell looking for a file called `zsh-pulse-highlight-redraw`.
  # Registering them as widgets first makes it leave them alone.
  zle -N zsh-pulse-highlight-redraw zsh-pulse-highlight-redraw
  zle -N zsh-pulse-highlight-line-init zsh-pulse-highlight-line-init
  zle -N zsh-pulse-highlight-line-finish zsh-pulse-highlight-line-finish

  add-zle-hook-widget zle-line-pre-redraw zsh-pulse-highlight-redraw
  add-zle-hook-widget zle-line-init zsh-pulse-highlight-line-init
  add-zle-hook-widget line-finish zsh-pulse-highlight-line-finish
fi
