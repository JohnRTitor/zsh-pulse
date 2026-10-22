#!/usr/bin/env zsh
# Interactive latency of the highlighting: keystroke to redraw.
#
# `bench_module_call.zsh` measures the cost of one native call in
# isolation. This harness measures what a user actually waits for: the time
# between a keystroke going in and the line being redrawn with the
# highlighting on it. That path is the one the plugin exists for, and it is
# the only one that involves zsh's line editor, the terminal and the
# pseudo-terminal in between.
#
# How it works:
#
#   * an interactive zsh runs under a `zpty` pseudo-terminal with the
#     module loaded and the adapter sourced, so the real
#     `zle-line-pre-redraw` hook and the real `region_highlight` special
#     parameter are in play — not a stand-in for them;
#   * every keystroke is written on its own, and the harness waits for the
#     pty to answer before writing the next one, so each keystroke is
#     measured on its own instead of in a batch;
#   * the number reported is the average of those measurements.
#
# What the number includes: the pty round trip and the interval between
# "the keystroke was written" and "the answer was noticed". It is
# therefore an upper bound on the latency a user perceives, and it is only
# useful when compared with another run of the same harness on the same
# machine.
#
# Usage:
#   ./bench/bench_zpty.zsh [keystrokes]
#
# `keystrokes` defaults to 200. Needs a built and installed module
# (`./bench/install.sh`) and the `zsh/zpty` module. `hyperfine` is *not*
# needed: the pty's own clock is the clock.
emulate -L zsh
setopt no_unset

N=${1:-200}
REPO=${0:A:h:h}
MP="$REPO/bench/mp"
ADAPTER="$REPO/zsh/init.zsh"
PTY=bench

# ---------------------------------------------------------------------------
# Preconditions. A benchmark that cannot measure the thing it claims to
# measure must fail loudly rather than print a number.
# ---------------------------------------------------------------------------

if ! zmodload zsh/zpty 2>/dev/null; then
  print -u2 "ERROR: the zsh/zpty module is not available; this harness needs it"
  exit 1
fi
if ! zmodload zsh/datetime 2>/dev/null; then
  print -u2 "ERROR: the zsh/datetime module is not available; this harness needs it for its clock"
  exit 1
fi
# `zselect` waits for the pty to become readable instead of polling it.
# Polling would put the poll interval into every measurement; waiting does
# not. It is optional: without it the harness falls back to polling and
# says so, because a coarser measurement is still a measurement.
typeset -gi HAVE_ZSELECT=0
if zmodload zsh/zselect 2>/dev/null; then
  HAVE_ZSELECT=1
else
  print -u2 "NOTE: the zsh/zselect module is not available; the pty will be \
polled instead of waited on, so every measurement includes the poll interval \
and the numbers are an upper bound."
fi
if [[ ! -f $MP/shellpulse/highlight.so ]]; then
  print -u2 "ERROR: the module is not installed at $MP. Run: ./bench/install.sh"
  exit 1
fi
if [[ ! -f $ADAPTER ]]; then
  print -u2 "ERROR: the adapter is missing at $ADAPTER"
  exit 1
fi
# The arm flag comes first, so that the keystroke count is still the first
# positional argument.
typeset -g ARM=default
if [[ $N == "--control" || $N == "--none" ]]; then
  ARM=$N
  shift
  N=${1:-200}
fi
if ! [[ $N == <-> ]] || (( N < 1 )); then
  print -u2 "ERROR: the keystroke count must be a positive integer, got: $N"
  exit 1
fi

# The interactive shell reads its configuration from $ZDOTDIR, so the
# harness decides what it loads and the user's own configuration cannot
# influence the measurement.
ZDOT_TMP="${TMPDIR:-/tmp}/shell-pulse-bench-$$"
if ! mkdir -p $ZDOT_TMP; then
  print -u2 "ERROR: cannot create the temporary directory $ZDOT_TMP"
  exit 1
fi
export ZDOTDIR=$ZDOT_TMP

# Three arms, so that the cost of the plugin existing can be told from the
# cost of the plugin working:
#
#   (default)  the adapter: the module is loaded and the hooks are installed
#   --control  the module is loaded but no hook is installed, so the plugin
#              exists and costs a `makezleparams` per redraw but does no work
#   --none     nothing at all: zsh and the pty on their own
#
# The difference between the default arm and the control is the plugin's own
# work; the difference between the control and `--none` is the cost of the
# plugin merely being installed.
{
  print -r -- "PS1='BENCHPROMPT> '"
  print -r -- "RPROMPT="
  print -r -- "PROMPT_EOL_MARK=''"
  print -r -- "export SHELL_PULSE_MODULE_PATH='$MP'"
  if [[ $ARM == default ]]; then
    print -r -- "source '$ADAPTER'"
  elif [[ $ARM == control ]]; then
    # The module, but no hook: the plugin exists and does nothing.
    print -r -- "zmodload shellpulse/highlight"
  fi
} > $ZDOT_TMP/.zshrc

typeset -g CAP=''
typeset -gi CYAN=0

# The pty must not outlive this script.
cleanup() {
  zpty -d $PTY 2>/dev/null
  rm -f $ZDOT_TMP/.zshrc
  rmdir $ZDOT_TMP 2>/dev/null
}
trap cleanup EXIT INT TERM

# Count the occurrences of $2 in $1.
#
# The needle is compared literally: the bytes we look for contain `[`,
# which would otherwise be read as the start of a character class.
count_occurrences() {
  local hay=$1 needle=$2 n=0
  while [[ $hay == *"$needle"* ]]; do
    (( n++ ))
    hay=${hay#*"$needle"}
  done
  print -r -- $n
}

# Read everything the pty has to offer right now.
drain() {
  local chunk=''
  while zpty -rt $PTY chunk 2>/dev/null; do
    CAP+=$chunk
    chunk=''
  done
  return 0
}

# Wait for the pty to say something, up to $1 seconds.
wait_for_output() {
  local limit=$1 chunk=''
  if (( HAVE_ZSELECT )); then
    # `zselect` returns as soon as the pty has something to say, so the
    # measurement is not rounded up to a poll interval.
    zselect -t $limit $PTY_FD
    local rc=$?
    if (( rc == 2 )); then
      print -u2 "ERROR: zselect failed on the pty"
      exit 1
    fi
    (( rc == 0 )) || return 1
  else
    local deadline=$(( EPOCHREALTIME + limit ))
    while (( EPOCHREALTIME < deadline )); do
      if zpty -rt $PTY chunk 2>/dev/null; then
        CAP+=$chunk
        return 0
      fi
      sleep 0.001
    done
    return 1
  fi
  zpty -rt $PTY chunk 2>/dev/null || return 1
  CAP+=$chunk
  return 0
}

# ---------------------------------------------------------------------------
# The pseudo-terminal
# ---------------------------------------------------------------------------

if ! zpty -b $PTY zsh -i; then
  print -u2 "ERROR: this environment cannot provide a pseudo-terminal"
  exit 1
fi
# `zpty` leaves the master side's file descriptor in `$REPLY`.
typeset -i PTY_FD=${REPLY-0}
if (( PTY_FD <= 0 )); then
  print -u2 "ERROR: the pseudo-terminal has no file descriptor"
  exit 1
fi

# Wait for the prompt: it means the terminal is in raw mode and the line
# editor is running. Typing before that would be swallowed by the tty
# driver's canonical-mode line discipline.
typeset -i deadline=$(( EPOCHREALTIME + 10 ))
typeset -i ready=0
while (( EPOCHREALTIME < deadline )); do
  typeset chunk=''
  if zpty -rt $PTY chunk 2>/dev/null; then
    CAP+=$chunk
  fi
  if [[ $CAP == *BENCHPROMPT* ]]; then
    ready=1
    break
  fi
  sleep 0.02
done
if (( ! ready )); then
  print -u2 "ERROR: the interactive shell under the pty never drew its prompt."
  print -u2 "captured so far: ${(V)CAP}"
  exit 1
fi

# ---------------------------------------------------------------------------
# The measurement
# ---------------------------------------------------------------------------

# A realistic command line, repeated until there are enough characters to
# type. Every character is one keystroke.
typeset text=''
typeset pattern='git commit --amend --no-edit '
while (( ${#text} < N )); do
  text+=$pattern
done
text=${text[1,N]}

typeset -F total=0
typeset -F fastest=0
typeset -F slowest=0
typeset -i typed=0

for (( k = 1; k <= N; k++ )); do
  # Drain the pty first, so the answer to this keystroke cannot be
  # confused with the tail of the previous one.
  drain

  typeset -F start=$EPOCHREALTIME
  if ! zpty -wn $PTY "$text[k]"; then
    print -u2 "ERROR: could not type keystroke $k into the pty"
    exit 1
  fi
  if ! wait_for_output 5; then
    print -u2 "ERROR: the pty did not answer keystroke $k within 5 s"
    exit 1
  fi
  typeset -F stop=$EPOCHREALTIME
  typeset -F delta=$(( stop - start ))

  (( total += delta ))
  (( typed++ ))
  if (( typed == 1 )); then
    fastest=$delta
    slowest=$delta
  else
    (( delta < fastest )) && fastest=$delta
    (( delta > slowest )) && slowest=$delta
  fi
done

# ---------------------------------------------------------------------------
# The report
# ---------------------------------------------------------------------------

# The highlighting has to have happened, or this harness measured a shell
# without the plugin and the number means nothing.
CYAN=$(count_occurrences $CAP $'\e[36m')
if (( CYAN == 0 )) && [[ $ARM == default ]]; then
  print -u2 "ERROR: no highlighting was seen in the terminal output."
  print -u2 "       This harness would be measuring an unhighlighted shell."
  print -u2 "       captured: ${(V)CAP}"
  exit 1
fi

typeset -F per=$(( total / typed ))
printf 'keystrokes:       %d\n' $typed
printf 'total time:       %.6f s\n' $total
printf 'per keystroke:    %.6f s  (%.1f us)\n' $per $(( per * 1000000 ))
printf 'fastest:          %.1f us\n' $(( fastest * 1000000 ))
printf 'slowest:          %.1f us\n' $(( slowest * 1000000 ))
printf 'highlighted runs: %d\n' $CYAN
printf 'pty wait:         %s\n' $( (( HAVE_ZSELECT )) && print -r -- 'zselect' || print -r -- 'polling' )
printf 'TERM:             %s\n' ${TERM:-unset}
printf 'arm:              %s\n' $ARM
printf 'line typed:       %s\n' "$text"
