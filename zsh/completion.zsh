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
  # Real-time type-ahead completion widget (lists choices below prompt without inserting).
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

    # Strictly prevent inserting text or modifying buffer during typeahead auto-listing.
    compstate[insert]=''
    compstate[old_list]=''
    compstate[pattern_insert]=''
    unset MENUSELECT MENUMODE 2>/dev/null

    # Query zsh-pulse's native Rust completion engine (<10us latency).
    # Never invokes Zsh compsys shell scripts or spelling correctors.
    zsh-pulse-complete --max=16

    if (( ${#_zsh_pulse_matches[@]} > 0 )); then
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
      compadd -Q -U -d descs -a _zsh_pulse_matches
      compstate[list]='list force'
    else
      compstate[list]=''
    fi

    unset MENUSELECT MENUMODE 2>/dev/null
    compstate[insert]=''
    _lastcomp[insert]=''
    compstate[pattern_insert]=''
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
      _zsh_pulse_matches=()
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
      if (( _ZSH_PULSE_MENU_HIDDEN == 0 && ${#_zsh_pulse_matches[@]} > 0 )) &&
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
# 5. Tab Behavior: Toggle / Hide Completion Menu
# -----------------------------------------------------------------------------

if (( has_autocomplete )); then
  typeset -g _ZSH_PULSE_MENU_HIDDEN=0

  # Pressing Tab when the completion menu is visible hides/clears the menu.
  # Pressing Tab when the menu is hidden or empty expands/shows completion.
  _zsh_pulse_tab() {
    if (( _ZSH_PULSE_MENU_HIDDEN == 0 )) && [[ -n $_ZSH_PULSE_LAST_TYPEAHEAD ]]; then
      _ZSH_PULSE_MENU_HIDDEN=1
      _ZSH_PULSE_LAST_TYPEAHEAD=""
      zle -Rc 2>/dev/null
    else
      _ZSH_PULSE_MENU_HIDDEN=0
      _ZSH_PULSE_LAST_TYPEAHEAD=""
      zle expand-or-complete
    fi
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
  # in ~/.zshrc) or if fzf is available in PATH. When detected, shell-pulse preserves
  # and invokes the official fzf-history-widget (or non-destructive fzf wrapper),
  # guaranteeing that the user's fzf setup is never clobbered.
  # If fzf is not present (or when ZSH_PULSE_USE_FZF=0), shell-pulse binds its
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
    zle -Rc 2>/dev/null
  }
  zle -N _zsh_pulse_menuselect_abort

  _zsh_pulse_menuselect_accept() {
    typeset -g _ZSH_PULSE_MENU_ACTIVE=0
    _ZSH_PULSE_SAVED_BUFFER=""
    _ZSH_PULSE_SAVED_ACTIVE=0
    _zsh_pulse_hist_searching=0
    zle .accept-line
  }
  zle -N _zsh_pulse_menuselect_accept

  # Tab (toggle/hide menu) & Shift-Tab (reverse)
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
