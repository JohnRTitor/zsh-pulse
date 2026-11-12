# zsh-pulse completion, autosuggestions, and pager adapter.
#
# Provides:
# 1. Hybrid Completer Integration:
#    - Preserves 100% of native Zsh completers (_complete: git, cargo, docker, nix, flags, subcommands).
#    - Intercepts paths with zsh-pulse's bounded Rust scanner to eliminate large directory hangs (/nix/store).
# 2. Category Grouping & Columnated Pager:
#    - Clean headings ([directories], [files], [commands], [history], [options]).
#    - Arrow-key menu selection via zsh/complist.
# 3. Arrow-Key Menu Navigation (Fish & zsh-autocomplete parity):
#    - Down-Arrow (↓ / Ctrl-N): Enters active completion menu immediately.
#    - Up-Arrow (↑ / Ctrl-P): Opens interactive multi-line history search menu.
# 4. Fish-Style Autosuggestions ($POSTDISPLAY):
#    - Full line acceptance: Right-Arrow (→), End, Ctrl-F, Ctrl-E.
#    - Word-by-word acceptance: Alt-Right, Alt-F, Ctrl-Right.
# 5. Tab & Shift-Tab:
#    - Tab: Expands or enters completion menu.
#    - Shift-Tab: Cycles backwards (reverse-menu-complete).
# 6. Recent Directories:
#    - Tracks visited directories for fast jump completions.

# Silence completion beeps and prevent typeahead confirmation prompts.
unsetopt list_beep 2>/dev/null
LISTMAX=1000
zstyle ':completion:*' list-prompt ''

# If the completion/history builtin was not compiled into the module, exit cleanly.
if (( ! $+builtins[zsh-pulse-complete] )); then
  return 0
fi

# Exit if both completion and history features are disabled by configuration.
if [[ ${ZSH_PULSE_COMPLETION:-1} == (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) && \
      ${ZSH_PULSE_HISTORY:-1} == (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) ]]; then
  return 0
fi

local has_autocomplete=0
local has_history=0
local has_autosuggest=0

if [[ ${ZSH_PULSE_COMPLETION:-1} != (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) ]]; then
  if zsh-pulse-complete --has-feature=autocomplete 2>/dev/null; then
    has_autocomplete=1
  fi
fi

if [[ ${ZSH_PULSE_HISTORY:-1} != (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) ]]; then
  if zsh-pulse-complete --has-feature=history 2>/dev/null; then
    has_history=1
  fi
fi

if [[ ${ZSH_PULSE_AUTOSUGGEST:-1} != (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) ]]; then
  has_autosuggest=1
fi

# Load menu selection module if either autocomplete or history is active.
if (( has_autocomplete || has_history )); then
  zmodload zsh/complist 2>/dev/null
fi

if (( has_autocomplete )); then
  # Configure completion menu styling and group headings.
  zstyle ':completion:*' menu select
  zstyle ':completion:*' group-name ''
  zstyle ':completion:*:descriptions' format '%B%F{yellow}[%d]%f%b'
  zstyle ':completion:*:messages' format '%F{red}%d%f'
  zstyle ':completion:*:warnings' format '%F{red}No matches for: %d%f'

  # Typeahead list-choices configuration:
  # Strictly prevent menu selection, text insertion, and warning banners during auto-listing.
  zstyle ':completion:list-choices:*' menu no
  zstyle ':completion:list-choices:*' warnings format ''
  zstyle ':completion:list-choices:*' messages format ''
fi

if (( has_autocomplete )); then
  typeset -gi _ZSH_PULSE_MAX_LINES=16
  typeset -gi _ZSH_PULSE_PARTIAL_LIST=0
  typeset -gi _ZSH_PULSE_TYPEAHEAD_VISIBLE=0

  # How many lines the typeahead preview may occupy.
  #
  # zsh-autocomplete's real-time listing is capped by *rendered lines*, not
  # by candidate count. Its default is 16 lines, further limited by the
  # terminal height and the space the current buffer already occupies:
  #
  #     min(16, LINES - BUFFERLINES - 1)
  #
  # Heading lines and the `(MORE)` marker count toward that cap. The
  # optional arguments exist so the formula can be tested without a
  # terminal; the widget calls this with no arguments.
  _zsh_pulse_typeahead_max_lines() {
    local -i max_lines=${ZSH_PULSE_TYPEAHEAD_LINES:-16}
    local -i lines=${1:-${LINES:-24}}
    local -i buffer_lines=${2:-${BUFFERLINES:-1}}

    _ZSH_PULSE_MAX_LINES=$(( lines - buffer_lines - 1 ))
    (( _ZSH_PULSE_MAX_LINES > max_lines )) && _ZSH_PULSE_MAX_LINES=$max_lines
    (( _ZSH_PULSE_MAX_LINES > 0 ))
  }

  # Number of leading arguments of a `compadd` call that are options.
  #
  # Adapted from zsh-autocomplete's `_autocomplete__compadd_opts_len`
  # (MIT; see ATTRIBUTION.md). The wrapper needs it to pass the original
  # option arguments through to the real builtin while replacing the
  # match list with the truncated one.
  _zsh_pulse_compadd_opts_len() {
    local -i len=$(( ${@[(i)(-|--)]} - 1 ))
    (( len < $# )) && return len

    len=${@[(I)-*]}
    [[ $@[len] == -*[PSpsiIdJVXxrRWFMOAD] ]] && (( len++ ))
    return len
  }

  # `_describe` adds a heading line for each group. The wrapper has to
  # reserve room for it before counting the candidates.
  _zsh_pulse_typeahead_describe() {
    local -i _ZSH_PULSE_DESCRIBED_LINES=1
    _zsh_pulse_typeahead_describe_old "$@"
  }

  # Stop the completion system from trying further tags once the preview
  # is full. `comptags` is shadowed only for the duration of the call and
  # restored by the widget's `always` block.
  _zsh_pulse_typeahead_compadd_disable() {
    _ZSH_PULSE_PARTIAL_LIST=1
    comptags() { false }
  }

  # `compadd` wrapper that keeps the preview within the line cap.
  #
  # Adapted from zsh-autocomplete's `.autocomplete:async:compadd`
  # (MIT; see ATTRIBUTION.md). The algorithm is the same: measure how
  # many lines a group would add, trim the match and display arrays when
  # they do not fit, and leave a `(MORE)` marker for the widget to append.
  _zsh_pulse_typeahead_compadd() {
    setopt localoptions multibyte

    local -A _opts_=()
    local -a _xopts_=() _displ_=() _matches_=()
    local _displ_name_= _matches_name_=

    zparseopts -A _opts_ -E -- D: E: O:

    local -i _unused_lines_=$(( _ZSH_PULSE_MAX_LINES - compstate[list_lines] ))

    # If `_grp` is not set, `_describe` is adding completions in a normal
    # way and the group bookkeeping below does not apply.
    if [[ -v _ZSH_PULSE_DESCRIBED_LINES && -n $_grp ]]; then
      # We cannot interfere while `_describe` is actually adding the
      # completions, or the layout breaks.
      if [[ -z $_opts_[-D] ]]; then
        builtin compadd "$@"
        return
      fi

      _displ_name_=$_opts_[-D]
      _matches_name_=$_opts_[-O]

      # The preview is already full: tell `_describe` there is nothing
      # left to add for this group.
      if (( _ZSH_PULSE_PARTIAL_LIST )); then
        set -A $_displ_name_
        [[ -n $_matches_name_ ]] && set -A $_matches_name_
      fi

      builtin compadd "$@"
      local -i _ret_=$?

      local -i _ndisplay_=${(PA)#_displ_name_}
      local -i _lines_left_for_describe_=$(( _unused_lines_ - _ZSH_PULSE_DESCRIBED_LINES ))

      # The number of lines a `_describe` group adds is the number of
      # unique display strings.
      if (( ${#${(u)${(PA)_displ_name_}[@]#*:}} > _lines_left_for_describe_ )); then
        local -i _matches_to_remove=$_ndisplay_
        (( _lines_left_for_describe_ > 0 )) &&
            _matches_to_remove=$(( _ndisplay_ - _lines_left_for_describe_ ))
        if (( _matches_to_remove < _ndisplay_ )); then
          shift -p $_matches_to_remove $_displ_name_ $_matches_name_
        else
          set -A $_displ_name_
          [[ -n $_matches_name_ ]] && set -A $_matches_name_
        fi
        _ndisplay_=${(PA)#_displ_name_}
        _zsh_pulse_typeahead_compadd_disable
      fi
      (( _ZSH_PULSE_DESCRIBED_LINES += _ndisplay_ ))

      return _ret_
    fi

    # The preview is already full: drop this group's matches.
    if (( _ZSH_PULSE_PARTIAL_LIST )); then
      [[ -n $_opts_[-D] ]] && set -A $_opts_[-D]
      [[ -n $_opts_[-O] ]] && set -A $_opts_[-O]
      return 1
    fi

    if [[ -n $_opts_[-D]$_opts_[-O] ]]; then
      builtin compadd "$@"
      return
    fi

    local -i _old_total_lines=$compstate[list_lines]

    # +: keep all occurrences; otherwise only the last one is kept.
    zparseopts -a _xopts_ -D -E -- X+: x+:

    if (( $#_xopts_ )); then
      # Use only the first one: subsequent `-X`/`-x` options are ignored
      # by `compadd` anyway.
      local original="$_xopts_[2]"
      local formatted="${(%g:ce:)original}"

      # Trim the text to one line and omit ANSI sequences: command
      # substitution would otherwise choke on them.
      local ansi_start=${(M)formatted##($'\C-[['[;[:digit:]]#m)#}
      local ansi_end=${(M)formatted%%($'\C-[['[;[:digit:]]#m)#}
      local text=${${formatted#$ansi_start}%$ansi_end}
      _xopts_=(
        "$_xopts_[1]"
        "${(mr:COLUMNS-1:)text%%[[:space:]]#}"
      )
    fi

    local -i _total_new_lines_="$(
      builtin compadd "$_xopts_[@]" "$@"
      print -nr -- $(( $compstate[list_lines] - _old_total_lines ))
    )"

    if (( $#_xopts_ )); then
      _xopts_[2]="${original/$text/$_xopts_[-1]}"
    fi

    # Everything fits.
    if (( _total_new_lines_ + $compstate[list_lines] <= _ZSH_PULSE_MAX_LINES )); then
      builtin compadd "$_xopts_[@]" "$@"
      return
    fi

    local -i _new_completion_lines_="$(
      builtin compadd "$@"
      print -nr -- $(( $compstate[list_lines] - _old_total_lines ))
    )"

    local -a _dopt_=()
    zparseopts -a _dopt_ -D -E -- d: ld:

    _displ_name_=$_dopt_[2]

    # Collect all matching completions and their display strings (if any).
    local -a _Dopt_=()
    [[ -n $_displ_name_ ]] && _Dopt_=( -D $_displ_name_ )
    builtin compadd -O _matches_ $_Dopt_ "$@"

    # Without an array of display strings, create one from the matches.
    if [[ -z $_displ_name_ ]]; then
      _displ_name_=_displ_
      _displ_=( "$_matches_[@]" )
      _dopt_=( -d $_displ_name_ )
    fi

    local -i _nmatches_per_line_=$(( 1.0 * $#_matches_ / _new_completion_lines_ ))

    # If a match needs more than one line, make each match fit exactly one.
    if (( _nmatches_per_line_ < 1 )); then
      # WORKAROUND: zsh treats a display string exactly $COLUMNS wide as
      # not fitting on one line.
      set -A $_displ_name_ ${(@mr:COLUMNS-1:)${(PA)_displ_name_}[@]//$'\n'/\n}

      _dopt_=( -ld $_displ_name_ )
      (( _nmatches_per_line_ = 1 ))
    fi

    local -i _new_heading_lines_=$(( _total_new_lines_ - _new_completion_lines_ ))

    # Round down before subtracting, or the result is effectively rounded up.
    local -i _nmatches_that_fit_=$((
      ( _unused_lines_ - _new_heading_lines_ ) * _nmatches_per_line_
    ))

    local -i _nmatches_to_remove_=$#_matches_
    (( _nmatches_that_fit_ > 0 )) &&
        _nmatches_to_remove_=$(( $#_matches_ - _nmatches_that_fit_ ))

    if (( _nmatches_to_remove_ > 0 )); then
      # Reserve one line for the `(MORE)` marker.
      (( _nmatches_to_remove_++ ))
      (( _nmatches_to_remove_ > $#_matches_ )) &&
          _nmatches_to_remove_=$#_matches_

      _zsh_pulse_typeahead_compadd_disable

      # Always add a heading, even when no matches remain: a lone
      # `(MORE)` without any content makes no sense.
      (( _nmatches_to_remove_ >= $#_matches_ )) && _xopts_[1]=-x

      shift -p $_nmatches_to_remove_ _matches_ $_displ_name_
    fi

    _zsh_pulse_compadd_opts_len "$@"
    builtin compadd "$_xopts_[@]" "$_dopt_[@]" -a "$@[1,?]" _matches_
  }

  # Real-time type-ahead completion widget (lists choices below prompt without inserting).
  #
  # The candidate source is zsh's own completion system: `_main_complete`
  # runs synchronously in the `list-choices` context. That is what makes
  # `git`, `cargo`, flags, subcommands and their descriptions appear exactly
  # as they do on Tab, instead of the Rust engine's path-only guesses.
  _zsh_pulse_typeahead_widget() {
    emulate -L zsh
    setopt extendedglob noglob
    local curcontext="list-choices:::"
    local MENUSELECT MENUMODE

    # Never run typeahead if menu selection is already active or in progress.
    if [[ $KEYMAP == menuselect || $_ZSH_PULSE_MENU_ACTIVE == 1 ]] ||
       { (( $+builtins[zsh-pulse-complete] )) && zsh-pulse-complete --is-menu-active 2>/dev/null; }; then
      return 0
    fi

    (( $+functions[_main_complete] )) || return 0

    _ZSH_PULSE_TYPEAHEAD_VISIBLE=0
    _ZSH_PULSE_PARTIAL_LIST=0
    _zsh_pulse_typeahead_max_lines || return 0

    # Strictly prevent inserting text or modifying buffer during typeahead auto-listing.
    compstate[insert]=''
    compstate[old_list]=''
    compstate[pattern_insert]=''
    compstate[list]='list force packed'
    compstate[list_max]=0

    # Force `_describe` to load so its body can be saved and shadowed.
    autoload -Uz +X _describe 2>/dev/null

    if (( $+functions[compadd] )); then
      functions[_zsh_pulse_typeahead_compadd_old]="$functions[compadd]"
    fi
    functions[compadd]="$functions[_zsh_pulse_typeahead_compadd]"

    if (( $+functions[_describe] )); then
      functions[_zsh_pulse_typeahead_describe_old]="$functions[_describe]"
      functions[_describe]="$functions[_zsh_pulse_typeahead_describe]"
    fi

    {
      _main_complete
    } always {
      unfunction compadd comptags 2>/dev/null
      if (( $+functions[_zsh_pulse_typeahead_compadd_old] )); then
        functions[compadd]="$functions[_zsh_pulse_typeahead_compadd_old]"
        unfunction _zsh_pulse_typeahead_compadd_old
      fi
      if (( $+functions[_zsh_pulse_typeahead_describe_old] )); then
        functions[_describe]="$functions[_zsh_pulse_typeahead_describe_old]"
        unfunction _zsh_pulse_typeahead_describe_old
      fi

      unset MENUSELECT MENUMODE 2>/dev/null
      compstate[insert]=''
      _lastcomp[insert]=''
      compstate[pattern_insert]=''
      _lastcomp[pattern_insert]=''
      compstate[list_max]=0
      MENUSCROLL=0

      if (( _ZSH_PULSE_PARTIAL_LIST )); then
        builtin compadd -J -last- -x '%F{0}%K{12}(MORE)%f%k'
        _lastcomp[list_lines]=$compstate[list_lines]
      fi
    }

    if (( compstate[nmatches] > 0 && compstate[list_lines] > 0 )); then
      _ZSH_PULSE_TYPEAHEAD_VISIBLE=1
    else
      compstate[list]=''
    fi

    return 2
  }

  zle -C zsh-pulse-typeahead list-choices _zsh_pulse_typeahead_widget
fi

# -----------------------------------------------------------------------------
# 1. Hybrid Completer Integration
# -----------------------------------------------------------------------------

if (( has_autocomplete )); then
  _zsh_pulse_completer() {
    emulate -L zsh
    setopt extendedglob noglob

    # If completing an explicit path or a path fragment containing a slash or tilde,
    # use zsh-pulse's bounded filesystem scanner in Rust.
    # This guarantees immune execution on massive directories like /nix/store.
    if [[ "$PREFIX" == /* || "$PREFIX" == ./* || "$PREFIX" == ../* || "$PREFIX" == ~* || "$PREFIX" == */* ]]; then
      zsh-pulse-complete --paths-only --max=32
      if (( ${#_zsh_pulse_matches[@]} > 0 )); then
        local -a dirs files
        local -a dir_descs file_descs
        local i
        for (( i=1; i<=${#_zsh_pulse_matches[@]}; i++ )); do
          if [[ "${_zsh_pulse_kinds[i]}" == "directory" ]]; then
            dirs+=("${_zsh_pulse_matches[i]}")
            dir_descs+=("${_zsh_pulse_descriptions[i]}")
          else
            files+=("${_zsh_pulse_matches[i]}")
            file_descs+=("${_zsh_pulse_descriptions[i]}")
          fi
        done
        # Directories do not append a trailing space (-S ''); files add an auto-removable space (-q -S ' ')
        (( ${#dirs[@]} > 0 )) && compadd -U -S '' -V directories -X '%B%F{blue}[directories]%f%b' -d dir_descs -a dirs
        (( ${#files[@]} > 0 )) && compadd -U -q -S ' ' -V files -X '%B%F{cyan}[files]%f%b' -d file_descs -a files
        return 0
      fi
    fi

    # Fall through to _complete, preserving git, cargo, docker, flags, and all native tools.
    return 1
  }

  # Prepend zsh-pulse completer into Zsh's completer list if autocomplete is enabled.
  zstyle ':completion:*' completer _zsh_pulse_completer _complete
fi

# -----------------------------------------------------------------------------
# 2. History Search & Navigation (Fish Parity)
# -----------------------------------------------------------------------------

if (( has_history )); then
  typeset -g _ZSH_PULSE_SAVED_ACTIVE=0
  typeset -g _ZSH_PULSE_SAVED_BUFFER=""
  typeset -g _ZSH_PULSE_SAVED_CURSOR=0

  # Interactive multi-line history search menu widget (bound to Ctrl-R).
  # Preserves BUFFER and CURSOR so cancelled searches cleanly restore original command line.
  _zsh_pulse_history_widget() {
    emulate -L zsh
    setopt extendedglob noglob
    typeset -g _ZSH_PULSE_MENU_ACTIVE=1
    _ZSH_PULSE_SAVED_BUFFER="$BUFFER"
    _ZSH_PULSE_SAVED_CURSOR=$CURSOR
    _ZSH_PULSE_SAVED_ACTIVE=1
    zsh-pulse-complete --history-only --max=32
    if (( ${#_zsh_pulse_matches[@]} > 0 )); then
      IPREFIX=""
      PREFIX="$BUFFER"
      SUFFIX=""
      ISUFFIX=""
      local -a descs
      local maxw=$(( COLUMNS > 4 ? COLUMNS - 2 : 78 ))
      local d
      for d in "${_zsh_pulse_descriptions[@]}"; do
        if (( ${#d} > maxw )); then
          descs+=( "${d[1,maxw]}" )
        else
          descs+=( "$d" )
        fi
      done
      compadd -Q -U -l -V history -X '%B%F{magenta}[history search]%f%b' -d descs -a _zsh_pulse_matches
    fi
  }

  zle -C zsh-pulse-history-search menu-select _zsh_pulse_history_widget

  # Fish-style prefix search on Up/Down arrows:
  # - Searches backward/forward in history matching the prefix currently in BUFFER.
  # - Seamlessly navigates across multiline history entries without getting trapped.
  # - Restores original typed prefix when navigating back to the start of history.
  # - When actively editing within a multiline command, navigates lines until top/bottom.
  autoload -Uz up-line-or-beginning-search down-line-or-beginning-search 2>/dev/null
  zle -N up-line-or-beginning-search
  zle -N down-line-or-beginning-search

  typeset -g _zsh_pulse_hist_searching=0
  typeset -g _zsh_pulse_hist_orig_buffer=""
  typeset -gi _zsh_pulse_hist_orig_no=0
  typeset -gi _zsh_pulse_hist_cursor=0

  _zsh_pulse_up_line_or_search() {
    if (( _zsh_pulse_hist_searching )); then
      local -i saved_cursor=$CURSOR
      CURSOR=$_zsh_pulse_hist_cursor
      if zle .history-beginning-search-backward; then
        CURSOR=$#BUFFER
      else
        CURSOR=$saved_cursor
      fi
    elif [[ $LBUFFER == *$'\n'* ]]; then
      _zsh_pulse_hist_searching=0
      zle .up-line
    else
      _zsh_pulse_hist_orig_buffer="$BUFFER"
      _zsh_pulse_hist_cursor=$CURSOR
      _zsh_pulse_hist_orig_no=$HISTNO
      _ZSH_PULSE_LAST_TYPEAHEAD=""
      _ZSH_PULSE_TYPEAHEAD_VISIBLE=0
      zle -Rc 2>/dev/null
      if zle .history-beginning-search-backward; then
        _zsh_pulse_hist_searching=1
        CURSOR=$#BUFFER
      else
        zle up-line-or-beginning-search 2>/dev/null || zle .up-line-or-history
      fi
    fi
  }

  zle -N up-line-or-search _zsh_pulse_up_line_or_search

  _zsh_pulse_down_line_or_select() {
    if (( _zsh_pulse_hist_searching )); then
      CURSOR=$_zsh_pulse_hist_cursor
      if zle .history-beginning-search-forward && (( HISTNO < _zsh_pulse_hist_orig_no )); then
        CURSOR=$#BUFFER
      else
        BUFFER="$_zsh_pulse_hist_orig_buffer"
        CURSOR=$_zsh_pulse_hist_cursor
        _zsh_pulse_hist_searching=0
      fi
    elif [[ $RBUFFER == *$'\n'* ]]; then
      _zsh_pulse_hist_searching=0
      zle .down-line
    else
      _zsh_pulse_hist_searching=0
      if (( _ZSH_PULSE_TYPEAHEAD_VISIBLE == 1 )) &&
         [[ -n $_ZSH_PULSE_LAST_TYPEAHEAD && "$BUFFER" == "$_ZSH_PULSE_LAST_TYPEAHEAD" ]]; then
        zle menu-select -w 2>/dev/null || zle down-line-or-beginning-search 2>/dev/null || zle .down-line-or-history
      else
        zle down-line-or-beginning-search 2>/dev/null || zle .down-line-or-history
      fi
    fi
  }

  zle -N down-line-or-select _zsh_pulse_down_line_or_select
fi

# -----------------------------------------------------------------------------
# 4. Fish-Style Autosuggestion Acceptance (Full Line & Word-by-Word)
# -----------------------------------------------------------------------------

if (( has_autosuggest )); then
  # Forward-char / Right-Arrow widget: accepts the entire ghost text at end of line.
  _zsh_pulse_forward_char_or_accept() {
    if [[ -n $POSTDISPLAY && $CURSOR -ge $#BUFFER ]]; then
      BUFFER="$BUFFER$POSTDISPLAY"
      CURSOR=$#BUFFER
      unset POSTDISPLAY
    else
      zle .forward-char
    fi
  }

  zle -N forward-char _zsh_pulse_forward_char_or_accept
  zle -N end-of-line _zsh_pulse_forward_char_or_accept

  # Forward-word / Alt-Right / Alt-F: accepts the next single word of ghost text (Fish feature).
  _zsh_pulse_forward_word_or_accept() {
    if [[ -n $POSTDISPLAY && $CURSOR -ge $#BUFFER ]]; then
      if [[ $POSTDISPLAY =~ '^([[:space:]]*[^[:space:]]+[[:space:]]*)' ]]; then
        local word="$MATCH"
        BUFFER="$BUFFER$word"
        CURSOR=$#BUFFER
        POSTDISPLAY="${POSTDISPLAY#$word}"
        zle -R
        return
      fi
    fi
    zle .forward-word
  }

  zle -N forward-word _zsh_pulse_forward_word_or_accept
fi

# -----------------------------------------------------------------------------
# 5. Tab Behavior: Expand the Full Native Completion List
# -----------------------------------------------------------------------------

if (( has_autocomplete )); then
  # The typeahead preview is only a preview. The first Tab replaces it with
  # the full, uncapped completion list (which may paginate); it does not
  # spend a keystroke hiding the preview first.
  _zsh_pulse_tab() {
    _ZSH_PULSE_LAST_TYPEAHEAD=""
    _ZSH_PULSE_TYPEAHEAD_VISIBLE=0
    # The typeahead widget leaves `_lastcomp` populated. Without clearing
    # it, `expand-or-complete` treats the preview as the current list and
    # jumps straight into menu selection (inserting the first candidate).
    # Clearing it makes the first Tab expand the common prefix and list the
    # full, uncapped completion set instead.
    _lastcomp=()
    zle expand-or-complete
  }

  zle -N zsh-pulse-tab _zsh_pulse_tab
fi

# -----------------------------------------------------------------------------
# 6. Key Bindings (main, emacs, viins, vicmd, menuselect)
# -----------------------------------------------------------------------------

_zsh_pulse_bind_all() {
  local widget="$1"
  shift
  local key
  for key; do
    bindkey -M main "$key" "$widget" 2>/dev/null || bindkey "$key" "$widget" 2>/dev/null
    bindkey -M emacs "$key" "$widget" 2>/dev/null
    bindkey -M viins "$key" "$widget" 2>/dev/null
  done
}

# Up-Arrow / History search & Down-Arrow
if (( has_history )); then
  _zsh_pulse_bind_all up-line-or-search '^[[A' '^[OA' '^P'
  bindkey -M vicmd 'k' up-line-or-search 2>/dev/null
  bindkey -M vicmd '^[[A' up-line-or-search 2>/dev/null
  bindkey -M vicmd '^[OA' up-line-or-search 2>/dev/null

  _zsh_pulse_bind_all down-line-or-select '^[[B' '^[OB' '^N'
  bindkey -M vicmd 'j' down-line-or-select 2>/dev/null
  bindkey -M vicmd '^[[B' down-line-or-select 2>/dev/null
  bindkey -M vicmd '^[OB' down-line-or-select 2>/dev/null

  # Interactive history search (Ctrl-R):
  # If fzf or fzf-history-widget is present:
  # Shell-pulse checks if fzf-history-widget is defined (e.g. sourced via 'fzf --zsh'
  # in ~/.zshrc) or if fzf is available in PATH. When detected, zsh-pulse preserves
  # and invokes the official fzf-history-widget (or non-destructive fzf wrapper),
  # guaranteeing that the user's fzf setup is never clobbered.
  # If fzf is not present (or when ZSH_PULSE_USE_FZF=0), zsh-pulse binds its
  # native sub-microsecond in-memory history search widget (zsh-pulse-history-search).
  local use_fzf=0
  if [[ ${ZSH_PULSE_USE_FZF:-1} != (0|[fF][aA][lL][sS][eE]|[oO][fF][fF]|[nN][oO]) && -z ${ZSH_PULSE_TEST:-} ]]; then
    if (( $+widgets[fzf-history-widget] )); then
      use_fzf=1
    elif (( $+commands[fzf] )); then
      use_fzf=2
    fi
  fi

  if (( use_fzf == 1 )); then
    _zsh_pulse_bind_all fzf-history-widget '^R'
    bindkey -M vicmd '^R' fzf-history-widget 2>/dev/null
  elif (( use_fzf == 2 )); then
    _zsh_pulse_fzf_history_widget() {
      setopt localoptions noglobsubst noposixbuiltins pipefail no_aliases extendedglob 2>/dev/null
      local selected
      selected=$(fc -rl 1 | awk '{ cmd=$0; sub(/^[ \t]*[0-9]+\*?[ \t]+/, "", cmd); if (!seen[cmd]++) print $0 }' |
        fzf --height 40% --tiebreak=index --query="$LBUFFER" +m)
      local ret=$?
      if [[ $ret -eq 0 && -n "$selected" ]]; then
        BUFFER="${selected##[[:space:]]#[0-9]##\*#[[:space:]]##}"
        CURSOR=$#BUFFER
      fi
      zle reset-prompt
      return $ret
    }
    zle -N _zsh_pulse_fzf_history_widget
    _zsh_pulse_bind_all _zsh_pulse_fzf_history_widget '^R'
    bindkey -M vicmd '^R' _zsh_pulse_fzf_history_widget 2>/dev/null
  else
    _zsh_pulse_bind_all zsh-pulse-history-search '^R'
    bindkey -M vicmd '^R' zsh-pulse-history-search 2>/dev/null
  fi
fi

# Autocomplete & tab bindings
if (( has_autocomplete || has_history )); then
  # Safe abort & accept widgets for menu selection to reset _ZSH_PULSE_MENU_ACTIVE
  # and restore original command line buffer on cancellation (Esc / Ctrl-C / Tab).
  _zsh_pulse_menuselect_abort() {
    if (( _ZSH_PULSE_SAVED_ACTIVE )); then
      BUFFER="$_ZSH_PULSE_SAVED_BUFFER"
      CURSOR=$_ZSH_PULSE_SAVED_CURSOR
      _ZSH_PULSE_SAVED_BUFFER=""
      _ZSH_PULSE_SAVED_ACTIVE=0
    fi
    typeset -g _ZSH_PULSE_MENU_ACTIVE=0
    _zsh_pulse_hist_searching=0
    _ZSH_PULSE_TYPEAHEAD_VISIBLE=0
    zle -Rc 2>/dev/null
  }
  zle -N _zsh_pulse_menuselect_abort

  _zsh_pulse_menuselect_accept() {
    typeset -g _ZSH_PULSE_MENU_ACTIVE=0
    _ZSH_PULSE_SAVED_BUFFER=""
    _ZSH_PULSE_SAVED_ACTIVE=0
    _zsh_pulse_hist_searching=0
    _ZSH_PULSE_TYPEAHEAD_VISIBLE=0
    zle .accept-line
  }
  zle -N _zsh_pulse_menuselect_accept

  # Tab (full native list) & Shift-Tab (reverse)
  _zsh_pulse_bind_all zsh-pulse-tab '^I'
  bindkey -M menuselect '^I' _zsh_pulse_menuselect_abort 2>/dev/null
  bindkey -M menuselect '^C' _zsh_pulse_menuselect_abort 2>/dev/null
  bindkey -M menuselect '^G' _zsh_pulse_menuselect_abort 2>/dev/null
  bindkey -M menuselect '\e' _zsh_pulse_menuselect_abort 2>/dev/null
  bindkey -M menuselect '^M' _zsh_pulse_menuselect_accept 2>/dev/null
  bindkey -M menuselect '^J' _zsh_pulse_menuselect_accept 2>/dev/null
  bindkey -M menuselect '\r' _zsh_pulse_menuselect_accept 2>/dev/null

fi

# Ghost text acceptance
if (( has_autosuggest )); then
  _zsh_pulse_bind_all forward-char '^[[C' '^[OC' '^F'
  _zsh_pulse_bind_all end-of-line '^E'
  _zsh_pulse_bind_all forward-word '^[f' '^[^[[C' '^[[1;3C' '^[[1;5C'
fi

# Menu selection navigation: single keystroke per candidate
if (( has_autocomplete || has_history )); then
  bindkey -M menuselect '^[[A' up-line-or-history 2>/dev/null
  bindkey -M menuselect '^[OA' up-line-or-history 2>/dev/null
  bindkey -M menuselect '^P' up-line-or-history 2>/dev/null
  bindkey -M menuselect 'k' up-line-or-history 2>/dev/null

  bindkey -M menuselect '^[[B' down-line-or-history 2>/dev/null
  bindkey -M menuselect '^[OB' down-line-or-history 2>/dev/null
  bindkey -M menuselect '^N' down-line-or-history 2>/dev/null
  bindkey -M menuselect 'j' down-line-or-history 2>/dev/null

  bindkey -M menuselect '^[[C' forward-char 2>/dev/null
  bindkey -M menuselect '^[OC' forward-char 2>/dev/null
  bindkey -M menuselect '^F' forward-char 2>/dev/null
  bindkey -M menuselect 'l' forward-char 2>/dev/null

  bindkey -M menuselect '^[[D' backward-char 2>/dev/null
  bindkey -M menuselect '^[OD' backward-char 2>/dev/null
  bindkey -M menuselect '^B' backward-char 2>/dev/null
  bindkey -M menuselect 'h' backward-char 2>/dev/null

  bindkey -M menuselect '^R' up-line-or-history 2>/dev/null
  _zsh_pulse_bind_all reverse-menu-complete '^[[Z'
  [[ -n "$terminfo[kcbt]" ]] && _zsh_pulse_bind_all reverse-menu-complete "$terminfo[kcbt]"
  bindkey -M menuselect '^[[Z' reverse-menu-complete 2>/dev/null
  [[ -n "$terminfo[kcbt]" ]] && bindkey -M menuselect "$terminfo[kcbt]" reverse-menu-complete 2>/dev/null
fi

unfunction _zsh_pulse_bind_all 2>/dev/null

# -----------------------------------------------------------------------------
# 7. Recent Directories Tracking
# -----------------------------------------------------------------------------

if (( has_autocomplete )); then
  autoload -Uz chpwd_recent_dirs cdr add-zsh-hook 2>/dev/null
  if (( $+functions[chpwd_recent_dirs] )); then
    add-zsh-hook chpwd chpwd_recent_dirs
  fi
fi
