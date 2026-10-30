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
#   ./bench/bench_zpty.zsh [options] [keystrokes]
#
# Options:
#   --control      the module is loaded, but no hook is installed
#   --none         nothing is loaded at all: zsh and the pty on their own
#   --regions N    type a line with N highlighted regions (see below)
#   -h, --help     this text
#
# `keystrokes` defaults to 200. Needs a built and installed module
# (`./bench/install.sh`) and the `zsh/zpty` module. `hyperfine` is *not*
# needed: the pty's own clock is the clock.
#
# The --regions mode
# ------------------
#
# The *number* of highlighted regions is what zsh pays for: it parses every
# `region_highlight` entry on every redraw
# (`zsh-src/Src/Zle/zle_refresh.c:488-587`), so a line with many regions
# costs more than a long line with few. The ordinary line of this harness
# (`git commit --amend --no-edit`) has exactly one region: the command
# word. That is not enough to tell what one region costs, so the harness
# can type a line with *N* regions instead:
#
#   ./bench/bench_zpty.zsh --regions 1 60
#   ./bench/bench_zpty.zsh --regions 40 60
#
# The line is `echo` followed by N quoted words:
#
#   echo 'a' 'a' 'a'
#
# Every quoted word is one `SingleQuoted` region — and the command word is
# a region of its own, so **N quoted words make N + 1 regions**. The
# harness says so in its report, and it checks the number against the one
# zsh actually holds.
#
# How the measurement is kept clean
# ---------------------------------
#
# The region line is written in one go first (the *setup* phase, which is
# not measured), and the measured keystrokes are typed afterwards, at the
# end of that line. Each of them is an ordinary argument character, so:
#
#   * the number of regions is the same for every measured keystroke — the
#     slope is not diluted by the line growing while it is typed;
#   * the line does change, so the module republishes every region on
#     every keystroke, and it is zsh's parsing of those regions — the cost
#     this mode is after — that is measured.
#
# The region count is verified, not assumed. `region_highlight` is a ZLE
# special parameter, so it can be read from inside a widget, and the
# harness binds a key to a widget that reports how many regions zsh holds.
# The widget is the harness's own scaffolding, it is not part of the
# plugin, and it only ever runs outside the measured keystrokes, so it
# cannot perturb the numbers.
#
# Skips
# -----
#
# Every condition that stops this harness from *running* here — a missing
# test dependency, a machine without a pseudo-terminal — prints
# `SKIP-PTY: <reason>` on stderr, next to the human-readable message. A CI
# job that *requires* the pty harness must grep the output for `SKIP-PTY`
# and fail when it finds one: a skipped check must never be
# indistinguishable from a passing one.
#
# A measurement that *fails* is not a skip: it says ERROR and exits
# non-zero, as it always did.
emulate -L zsh
setopt no_unset

REPO=${0:A:h:h}
MP="$REPO/bench/mp"
ADAPTER="$REPO/zsh/init.zsh"
PTY=bench

# The name of this script, taken before any function can change `$0`.
typeset -g PROG=${0:t}

# The marker a CI job greps for. See "Skips" in the header.
typeset -g SKIP_MARKER='SKIP-PTY'

# The marker the widget under the pty prints when the harness asks the
# shell how many regions it holds.
typeset -g REGION_MARKER='ZSH-PULSE-REGIONS'

# The key that runs that widget: Ctrl-X.
typeset -g REGION_KEY=$'\x18'

usage() {
  print -r -- "Usage: $PROG [options] [keystrokes]"
  print -r -- ''
  print -r -- '  --control      the module is loaded, but no hook is installed'
  print -r -- '  --none         nothing is loaded at all'
  print -r -- '  --regions N    type a line with N highlighted regions'
  print -r -- '  -h, --help     this text'
  print -r -- ''
  print -r -- 'keystrokes defaults to 200.'
}

# Say that the harness cannot run here.
#
# The marker is the machine-readable half of the message: the caller
# prints the human-readable half next to it. A job that *requires* this
# harness must fail when it finds the marker.
skip_pty() {
  print -u2 "$SKIP_MARKER: $1"
}

# ---------------------------------------------------------------------------
# The arguments
# ---------------------------------------------------------------------------

# The three arms, so that the cost of the plugin existing can be told from
# the cost of the plugin working:
#
#   (default)  the adapter: the module is loaded and the hooks are installed
#   --control  the module is loaded but no hook is installed, so the plugin
#              exists and costs a `makezleparams` per redraw but does no work
#   --none     nothing at all: zsh and the pty on their own
#
# The difference between the default arm and the control is the plugin's own
# work; the difference between the control and `--none` is the cost of the
# plugin merely being installed.
typeset -g ARM=default

# How many regions the typed line should have. 0 means "the ordinary line".
typeset -gi REGIONS=0

# How many keystrokes to type.
typeset -gi KEYSTROKES=200

typeset -ga POSITIONAL=()
while (( $# > 0 )); do
  case $1 in
    --control)
      ARM=control
      shift
      ;;
    --none)
      ARM=none
      shift
      ;;
    --regions)
      shift
      if (( $# == 0 )) || ! [[ ${1-} == <-> ]] || (( ${1-0} < 1 )); then
        print -u2 "ERROR: --regions needs a positive integer, got: ${1-<nothing>}"
        exit 1
      fi
      REGIONS=$1
      shift
      ;;
    --regions=*)
      REGIONS=${1#--regions=}
      if ! [[ $REGIONS == <-> ]] || (( REGIONS < 1 )); then
        print -u2 "ERROR: --regions needs a positive integer, got: $REGIONS"
        exit 1
      fi
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      POSITIONAL+=("$@")
      break
      ;;
    <->)
      POSITIONAL+=($1)
      shift
      ;;
    *)
      print -u2 "ERROR: unknown argument: $1"
      usage >&2
      exit 1
      ;;
  esac
done

if (( ${#POSITIONAL} > 1 )); then
  print -u2 "ERROR: only one keystroke count may be given, got: ${(j:, :)POSITIONAL}"
  exit 1
fi
if (( ${#POSITIONAL} == 1 )); then
  KEYSTROKES=$POSITIONAL[1]
fi
if ! [[ $KEYSTROKES == <-> ]] || (( KEYSTROKES < 1 )); then
  print -u2 "ERROR: the keystroke count must be a positive integer, got: $KEYSTROKES"
  exit 1
fi

# ---------------------------------------------------------------------------
# Preconditions. A benchmark that cannot measure the thing it claims to
# measure must fail loudly rather than print a number.
# ---------------------------------------------------------------------------

if ! zmodload zsh/zpty 2>/dev/null; then
  skip_pty "the zsh/zpty module is not available in this zsh"
  print -u2 "ERROR: the zsh/zpty module is not available; this harness needs it"
  exit 1
fi
if ! zmodload zsh/datetime 2>/dev/null; then
  skip_pty "the zsh/datetime module is not available in this zsh"
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
if [[ ! -f $MP/zshpulse/core.so ]]; then
  skip_pty "the module is not installed at $MP"
  print -u2 "ERROR: the module is not installed at $MP. Run: ./bench/install.sh"
  exit 1
fi
if [[ ! -f $ADAPTER ]]; then
  skip_pty "the adapter is missing at $ADAPTER"
  print -u2 "ERROR: the adapter is missing at $ADAPTER"
  exit 1
fi

# The interactive shell reads its configuration from $ZDOTDIR, so the
# harness decides what it loads and the user's own configuration cannot
# influence the measurement.
ZDOT_TMP="${TMPDIR:-/tmp}/zsh-pulse-bench-$$"
if ! mkdir -p $ZDOT_TMP; then
  print -u2 "ERROR: cannot create the temporary directory $ZDOT_TMP"
  exit 1
fi
export ZDOTDIR=$ZDOT_TMP

{
  print -r -- "PS1='BENCHPROMPT> '"
  print -r -- "RPROMPT="
  print -r -- "PROMPT_EOL_MARK=''"
  print -r -- "export ZSH_PULSE_MODULE_PATH='$MP'"
  if [[ $ARM == default ]]; then
    print -r -- "source '$ADAPTER'"
  elif [[ $ARM == control ]]; then
    # The module, but no hook: the plugin exists and does nothing.
    print -r -- "zmodload zshpulse/core"
  fi
  # The widget that reports how many regions zsh holds for the line.
  #
  # `region_highlight` is a ZLE special parameter: it exists inside every
  # widget, and its value is the list of regions zsh currently has. The
  # widget is scaffolding of this harness, not part of the plugin, and it
  # is only ever run after the measurement.
  print -r -- "zsh-pulse-bench-regions() { print -r -- '$REGION_MARKER: '\"\${#region_highlight}\" }"
  print -r -- 'zle -N zsh-pulse-bench-regions'
  print -r -- "bindkey '^X' zsh-pulse-bench-regions"
} > $ZDOT_TMP/.zshrc

typeset -g CAP=''
typeset -gi CYAN=0

# The answer of the shell under the pty to "how many regions do you hold?".
typeset -gi ANSWERED_REGIONS=0

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

# Ask the shell under the pty how many regions it holds.
#
# The answer is put into `ANSWERED_REGIONS` and *not* printed: a command
# substitution would run in a subshell, and that subshell would be the one
# reading the pty — the bytes it consumed would never reach the harness's
# own capture, and the capture is what the harness checks for the
# highlighting.
#
# Only the bytes that arrive *after* the question are looked at: an answer
# from an earlier question must not be mistaken for this one.
ask_for_regions() {
  local limit=$1 chunk='' answer=''
  local start=${#CAP}
  local deadline=$(( EPOCHREALTIME + limit ))
  ANSWERED_REGIONS=0
  if ! zpty -wn $PTY $REGION_KEY; then
    print -u2 "ERROR: could not ask the shell under the pty for its region count"
    exit 1
  fi
  while (( EPOCHREALTIME < deadline )); do
    if zpty -rt $PTY chunk 2>/dev/null; then
      CAP+=$chunk
      answer=${CAP[$((start + 1)),-1]}
      if [[ $answer == *"${REGION_MARKER}:"* ]]; then
        # The number is what follows the marker.
        local after=${answer##*${REGION_MARKER}: }
        if [[ $after =~ ^[0-9]+ ]]; then
          ANSWERED_REGIONS=$MATCH
          return 0
        fi
      fi
    else
      sleep 0.02
    fi
  done
  return 1
}

# ---------------------------------------------------------------------------
# The pseudo-terminal
# ---------------------------------------------------------------------------

if ! zpty -b $PTY zsh -i; then
  skip_pty "this environment cannot provide a pseudo-terminal"
  print -u2 "ERROR: this environment cannot provide a pseudo-terminal"
  exit 1
fi
# `zpty` leaves the master side's file descriptor in `$REPLY`.
typeset -i PTY_FD=${REPLY-0}
if (( PTY_FD <= 0 )); then
  skip_pty "the pseudo-terminal has no file descriptor"
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
  skip_pty "the interactive shell under the pty never drew its prompt"
  print -u2 "ERROR: the interactive shell under the pty never drew its prompt."
  print -u2 "captured so far: ${(V)CAP}"
  exit 1
fi

# ---------------------------------------------------------------------------
# The line to type
# ---------------------------------------------------------------------------

typeset text=''
typeset region_line=''
typeset -i expected_regions=0
typeset -i tail_length=0

# The characters that are typed, one per keystroke, and where in `text`
# they start.
#
# In the --regions mode the region line is written in the setup phase, so
# the keystrokes that are measured are the ones *after* it; in the ordinary
# mode the whole line is typed, keystroke by keystroke.
typeset type_text=''
typeset -i type_offset=0

if (( REGIONS > 0 )); then
  # `echo` is the command word: a region of its own. Every quoted word is
  # one more region, so N quoted words make N + 1 regions.
  region_line='echo'
  typeset -i i
  for (( i = 1; i <= REGIONS; i++ )); do
    region_line+=" 'a'"
  done
  # The trailing space keeps the measured keystrokes a separate word: the
  # line ends with an unstyled argument, not with a quoted one.
  region_line+=' '
  # The control and the --none arm install no hook, so they hold no
  # regions at all: they are the baseline the default arm is compared
  # with, not a measurement of the plugin's work.
  if [[ $ARM == default ]]; then
    expected_regions=$(( REGIONS + 1 ))
  else
    expected_regions=0
  fi

  # The measured keystrokes are typed after the region line is complete.
  # Each of them is an ordinary argument character: the number of regions
  # does not change, but the line does — which is what makes the module
  # republish every region on every keystroke.
  text=$region_line
  for (( i = 1; i <= KEYSTROKES; i++ )); do
    text+='x'
  done
  tail_length=$KEYSTROKES
  type_text=$text
  type_offset=${#region_line}
else
  # A realistic command line, repeated until there are enough characters to
  # type. Every character is one keystroke.
  typeset pattern='git commit --amend --no-edit '
  while (( ${#text} < KEYSTROKES )); do
    text+=$pattern
  done
  text=${text[1,KEYSTROKES]}
  # The command word is the only region on this line: the arguments are
  # not styled. The control and the --none arm install no hook, so they
  # hold no regions at all.
  if [[ $ARM == default ]]; then
    expected_regions=1
  else
    expected_regions=0
  fi
  type_text=$text
fi

# ---------------------------------------------------------------------------
# The measurement
# ---------------------------------------------------------------------------

typeset -F total=0
typeset -F fastest=0
typeset -F slowest=0
typeset -i typed=0

# How many regions the line had when the measurement started.
typeset -i setup_regions=0

if (( REGIONS > 0 )); then
  # The setup phase: the region line is written in one go, so that every
  # measured keystroke happens with the same number of regions on the
  # line. It is not measured — the harness is setting the line up, not
  # timing it.
  drain
  if ! zpty -wn $PTY $region_line; then
    print -u2 "ERROR: could not write the region line into the pty"
    exit 1
  fi
  # The question is also a barrier: the shell processes the key press
  # after everything that was written before it, so its answer proves
  # that the whole region line has been processed.
  if ! ask_for_regions 30; then
    print -u2 "ERROR: the shell under the pty did not answer the region count."
    print -u2 "       The region line was not processed, so there is nothing to measure."
    exit 1
  fi
  setup_regions=$ANSWERED_REGIONS
  if (( setup_regions == 0 && expected_regions > 0 )); then
    print -u2 "ERROR: the module published no regions for the line the harness set up."
    print -u2 "       The measurement would be of an unhighlighted shell."
    exit 1
  fi
  if (( setup_regions > expected_regions )); then
    print -u2 "ERROR: the line the harness set up has $setup_regions regions, more than the $expected_regions it should have."
    print -u2 "       The measurement would be of a different line than the one claimed."
    exit 1
  fi
  if (( setup_regions < expected_regions )); then
    print -u2 "WARNING: the line has $expected_regions regions, but the module published only $setup_regions."
    print -u2 "         Its region list is capped (region::MAX_ENTRIES), so the"
    print -u2 "         measurement below is for $setup_regions regions."
  fi
  drain
fi
for (( k = 1; k <= KEYSTROKES; k++ )); do
  # Drain the pty first, so the answer to this keystroke cannot be
  # confused with the tail of the previous one.
  drain

  typeset -F start=$EPOCHREALTIME
  if ! zpty -wn $PTY "$type_text[k + type_offset]"; then
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

# The region count is the harness's central claim in the --regions mode: if
# it cannot be established, the number below means nothing.
if ! ask_for_regions 10; then
  print -u2 "ERROR: the shell under the pty did not report its region count."
  print -u2 "       The harness cannot say how many regions it measured."
  exit 1
fi
typeset -i reported=$ANSWERED_REGIONS

if [[ $ARM == default ]]; then
  if (( reported == 0 )); then
    print -u2 "ERROR: the plugin published no regions for the line the harness typed."
    print -u2 "       The measurement would be of an unhighlighted shell."
    exit 1
  fi
  if (( REGIONS > 0 )); then
    # The measurement was about the number of regions the line had when it
    # started; if the count changed, the figures below would be about two
    # different lines.
    if (( reported != setup_regions )); then
      print -u2 "ERROR: the line held $setup_regions regions when the measurement started and $reported when it ended."
      print -u2 "       The figures below would be about two different lines."
      exit 1
    fi
  elif (( reported != expected_regions )); then
    print -u2 "WARNING: the line has $expected_regions regions, but zsh holds $reported."
    print -u2 "         The figures below are for $reported regions."
  fi
elif (( reported != 0 )); then
  # The control and the --none arm install no hook, so there are no
  # regions by design: they are the baseline the default arm is compared
  # with.
  print -u2 "WARNING: the $ARM arm holds $reported regions; it should hold none."
fi

typeset -F per=$(( total / typed ))
printf 'keystrokes:       %d\n' $typed
printf 'total time:       %.6f s\n' $total
printf 'per keystroke:    %.6f s  (%.1f us)\n' $per $(( per * 1000000 ))
printf 'fastest:          %.1f us\n' $(( fastest * 1000000 ))
printf 'slowest:          %.1f us\n' $(( slowest * 1000000 ))
printf 'regions:          %d\n' $reported
if [[ $ARM == control ]]; then
  printf 'regions expected: %d  (the control arm installs no hook)\n' $expected_regions
elif [[ $ARM == none ]]; then
  printf 'regions expected: %d  (the none arm loads nothing at all)\n' $expected_regions
elif (( REGIONS > 0 )); then
  printf 'regions expected: %d  (%d quoted word%s + the command word)\n' \
    $expected_regions $REGIONS "$( (( REGIONS == 1 )) && print -r -- '' || print -r -- 's' )"
  printf 'region line:      %s (%d characters)\n' \
    "${region_line[1,60]}$( (( ${#region_line} > 60 )) && print -r -- '…' )" \
    ${#region_line}
  printf 'measured tail:    %d argument characters, one keystroke each\n' $tail_length
else
  printf 'regions expected: %d  (the command word)\n' $expected_regions
fi
printf 'highlighted runs: %d\n' $CYAN
printf 'pty wait:         %s\n' $( (( HAVE_ZSELECT )) && print -r -- 'zselect' || print -r -- 'polling' )
printf 'TERM:             %s\n' ${TERM:-unset}
printf 'arm:              %s\n' $ARM
if (( REGIONS > 0 )); then
  printf 'line typed:       %s… (%d characters)\n' "${text[1,80]}" ${#text}
else
  printf 'line typed:       %s\n' "$text"
fi
