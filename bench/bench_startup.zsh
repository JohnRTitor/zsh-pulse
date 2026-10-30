#!/usr/bin/env zsh
#
# Startup cost and syscall behaviour of the zsh-pulse zsh module.
#
# What this measures
# -------------------
# 1. Load cost -- how much `zmodload zshpulse/core` adds to the startup
#    of a fresh `zsh -f`.  Four arms are timed with zsh's own clock
#    (`zmodload zsh/datetime` gives `EPOCHREALTIME`, microsecond
#    resolution); no `hyperfine` and no external tool are needed:
#
#      baseline : `zsh -f -c :`
#      path     : the same, plus the `module_path=(...)` assignment
#      loaded   : the same, plus `zmodload zshpulse/core`
#      plugin   : the same, plus `source zsh/init.zsh` (the real user path)
#
#    `loaded - path` is what the module itself costs; `loaded - baseline` is
#    the whole price of the plugin.  The arms are interleaved (one sample of
#    each, round and round) so that a busy machine perturbs all of them
#    equally instead of only the last one.
#
# 2. Syscall counts -- `strace -c -f` totals for a `zsh -f` with and without
#    the module, and the difference.
#
# 3. Hot path -- the number of `execve`/`execveat`/`clone`/`clone3`/`fork`/
#    `vfork` calls that happen *while the highlighting builtin runs*.  The
#    expectation is zero: nothing may be spawned on the keystroke path.
#    The calls are bracketed by markers written to stderr, and the window is
#    read out of the trace, so a spawn at load time (the worker thread) is
#    not mistaken for a spawn on the keystroke path.
#
# Deliberate-failure checks
# --------------------------
# A harness that measures the wrong thing must fail, not print a meaningless
# number.  This one refuses to report numbers unless:
#
#   * the module really loads and registers a builtin (`whence -w` says
#     "builtin"), checked *before* anything is measured;
#   * the line under test really reaches the builtin -- the module's own
#     `ZSH_PULSE_DEBUG` diagnostics are counted, and the count must equal
#     the number of calls made;
#   * the syscall counter really can see a process being spawned -- a
#     control run spawns one on purpose, inside the measurement window, and
#     the counter must find it.  Without this, "zero spawns" would only
#     mean "the counter is broken".
#
# Usage
# -----
#   ./bench/bench_startup.zsh
#
# Environment
# -----------
#   ZSH_PULSE_MODULE_PATH    directory holding `zshpulse/core.so`
#                               (default: <repo>/bench/mp)
#   ZSH_PULSE_BENCH_SAMPLES  timing samples per arm (default: 30)
#   ZSH_PULSE_BENCH_CALLS    builtin calls in the hot-path run (default: 50)
#
# Exits non-zero if any check fails.
#
# Known limitation
# ----------------
# The hot-path run uses `zsh -f -c`, so there is no ZLE widget and no
# `region_highlight` special parameter: the module reads the line and lexes
# it, but the final `setaparam` of the regions is not exercised (that path
# needs a real widget -- see the zpty harness, plan item E2).  The missing
# step is a call into zsh's own memory, which cannot spawn anything, so it
# does not weaken the "no spawns" conclusion; it does mean the hot path
# measured here is the *lower* bound of the real one.

emulate -L zsh
setopt no_unset err_return pipe_fail

# --------------------------------------------------------------------------
# Configuration
# --------------------------------------------------------------------------

REPO=${0:A:h:h}

# Where the module lives.  `zsh/init.zsh` uses the same variable, so the two
# can be pointed at the same build.
MP=${ZSH_PULSE_MODULE_PATH:-$REPO/bench/mp}

# Exported so that the plugin arm (`source zsh/init.zsh`) loads the very
# same module this harness measures.
export ZSH_PULSE_MODULE_PATH=$MP

# How many samples per timing arm, and how many times the builtin is called
# in the hot-path run.
SAMPLES=${ZSH_PULSE_BENCH_SAMPLES:-30}
CALLS=${ZSH_PULSE_BENCH_CALLS:-50}

[[ $SAMPLES == <-> ]] && (( SAMPLES >= 2 )) || {
  print -u2 -- "ERROR: ZSH_PULSE_BENCH_SAMPLES must be a number of at least 2."
  exit 1
}
[[ $CALLS == <-> ]] && (( CALLS >= 1 )) || {
  print -u2 -- "ERROR: ZSH_PULSE_BENCH_CALLS must be a number of at least 1."
  exit 1
}

# The module is loaded as `zshpulse/core`, never as a path: zsh's
# `modname_ok()` accepts only slash-separated identifiers, so absolute paths
# and `.so` suffixes are rejected.  `${(qq)}` quotes the path for the inner
# shell correctly even if it contains a single quote.
MP_Q=${(qq)MP}

# The inner scripts.  `$module_path` is escaped so the *inner* zsh expands
# it, not this one.
LOAD_SNIPPET="module_path=($MP_Q \$module_path); zmodload zshpulse/core"

ARM_BASELINE=":"
ARM_PATH="module_path=($MP_Q \$module_path)"
ARM_LOADED="$LOAD_SNIPPET"

# The plugin's real startup path, if the adapter is present.
ARM_PLUGIN=""
INIT_ZSH=$REPO/zsh/init.zsh
if [[ -f $INIT_ZSH ]]; then
  ARM_PLUGIN="source ${(qq)INIT_ZSH}"
fi

# --------------------------------------------------------------------------
# Small helpers
# --------------------------------------------------------------------------

# Every check the harness performs, and its outcome.
typeset -a CHECK_NAME CHECK_STATE CHECK_DETAIL
FAILURES=0

# record_check <name> <ok|fail|skip> <detail>
record_check() {
  CHECK_NAME+=( "$1" )
  CHECK_STATE+=( "$2" )
  CHECK_DETAIL+=( "$3" )
  if [[ $2 == fail ]]; then
    FAILURES=$(( FAILURES + 1 ))
  fi
}

# fail <message> [detail...] -- a fatal problem: say what went wrong, then
# stop here with a non-zero exit.
fail() {
  local message=$1
  shift
  print -u2 -- "ERROR: $message"
  local detail
  for detail in "$@"; do
    print -u2 -- "       $detail"
  done
  exit 1
}

# report <label> <value> <unit> [note] -- one number per line, with its unit.
report() {
  local label=$1 value=$2 unit=$3 note=${4:-}
  local text=$value
  if [[ -n $unit ]]; then
    text="$value $unit"
  fi
  if [[ -n $note ]]; then
    text="$text  ($note)"
  fi
  printf '  %-46s : %s\n' "$label" "$text"
}

# fmt_ms <float> -- milliseconds with three decimals.
fmt_ms() {
  printf '%.3f' "$1"
}

# --------------------------------------------------------------------------
# Preflight: the module must load, and it must register a builtin
# --------------------------------------------------------------------------

if ! zmodload zsh/datetime; then
  fail "this zsh has no zsh/datetime module, so nothing can be timed."
fi

if [[ ! -d $MP ]]; then
  fail "module path does not exist: $MP" \
    "set ZSH_PULSE_MODULE_PATH to the directory that holds zshpulse/core.so"
fi

if [[ ! -f $MP/zshpulse/core.so ]]; then
  fail "module not found: $MP/zshpulse/core.so" \
    "build and install it with: ./bench/install.sh release"
fi

# The load itself, in a fresh `zsh -f`.  `whence -w` must report a builtin:
# that is the proof the module loaded and registered its entry point.
#
# `no_err_return` is local to the function so that a *failed* load is
# reported in words rather than aborting the script silently: a module that
# does not load must fail loudly, not quietly.
LOAD_OUTPUT=""
LOAD_STATUS=0
load_check() {
  setopt localoptions no_err_return
  LOAD_OUTPUT=$(zsh -f -c "$1" 2>&1)
  LOAD_STATUS=$?
}
load_check "$LOAD_SNIPPET; whence -w zsh-pulse-highlight"
if [[ $LOAD_OUTPUT != *builtin* ]]; then
  fail "the module did not load, or did not register a builtin." \
    "zsh said: ${LOAD_OUTPUT:-<nothing>}" \
    "zsh exit status: $LOAD_STATUS" \
    "module path: $MP"
fi
record_check "module loads and registers a builtin" ok "$LOAD_OUTPUT"

# --------------------------------------------------------------------------
# Timing
# --------------------------------------------------------------------------

# ELAPSED_MS -- the wall-clock time of the last `time_once` call, in ms.
ELAPSED_MS=0

# time_once <inner zsh script> -- run it and time it.
time_once() {
  local s e
  s=$EPOCHREALTIME
  zsh -f -c "$1"
  e=$EPOCHREALTIME
  ELAPSED_MS=$(( (e - s) * 1000 ))
}

# Numeric sort.  zsh's ${(o)} sorts strings, not numbers, and the samples
# are floats, so they are sorted here.
# SORTED -- the sorted copy of the arguments.
typeset -a SORTED
sort_nums() {
  local -a a
  a=( "${(@)argv}" )
  local i j key
  for (( i = 2; i <= ${#a}; i++ )); do
    key=$a[i]
    j=$(( i - 1 ))
    while (( j >= 1 )); do
      if (( a[j] <= key )); then
        break
      fi
      a[j+1]=$a[j]
      j=$(( j - 1 ))
    done
    a[j+1]=$key
  done
  SORTED=( "${a[@]}" )
}

# STAT_MIN STAT_MED STAT_MAX -- statistics of the last sorted set.
STAT_MIN=0 STAT_MED=0 STAT_MAX=0
stats_of() {
  local n=${#SORTED}
  if (( n == 0 )); then
    return 1
  fi
  STAT_MIN=$SORTED[1]
  STAT_MAX=$SORTED[-1]
  STAT_MED=$SORTED[(( (n + 1) / 2 ))]
}

typeset -a T_BASE T_PATH T_LOAD T_PLUGIN
typeset -a D_PATH D_MODULE D_LOAD D_PLUGIN

# Warm the page cache and the dynamic loader, so that the first sample is
# not the one that pays for reading the .so from disk.
for w in {1..3}; do
  zsh -f -c "$ARM_BASELINE"
  zsh -f -c "$ARM_PATH"
  zsh -f -c "$ARM_LOADED"
  if [[ -n $ARM_PLUGIN ]]; then
    zsh -f -c "$ARM_PLUGIN"
  fi
done

# The samples, interleaved: one of each arm, then the next round.
#
# The per-sample *differences* are kept as well.  Because the arms are
# interleaved, sample i of every arm was taken within a few milliseconds of
# the others, so subtracting them cancels the slow drift of a busy machine
# that would otherwise swamp a difference of a few hundred microseconds.
for (( i = 1; i <= SAMPLES; i++ )); do
  time_once "$ARM_BASELINE"; T_BASE+=( $ELAPSED_MS )
  time_once "$ARM_PATH";     T_PATH+=( $ELAPSED_MS )
  time_once "$ARM_LOADED";   T_LOAD+=( $ELAPSED_MS )
  if [[ -n $ARM_PLUGIN ]]; then
    time_once "$ARM_PLUGIN"; T_PLUGIN+=( $ELAPSED_MS )
  fi
  D_PATH+=( $(( T_PATH[i] - T_BASE[i] )) )
  D_MODULE+=( $(( T_LOAD[i] - T_PATH[i] )) )
  D_LOAD+=( $(( T_LOAD[i] - T_BASE[i] )) )
  if [[ -n $ARM_PLUGIN ]]; then
    D_PLUGIN+=( $(( T_PLUGIN[i] - T_BASE[i] )) )
  fi
done

# --------------------------------------------------------------------------
# strace
# --------------------------------------------------------------------------

# The trace is read for two things: the spawn syscalls inside a marked
# window, and the module's own debug output.
#
# A trace line looks like:
#     3894769 write(2, "read BUFFER=\"echo hi\" (7 bytes) "...  = 46
#     3894769 clone3({flags=CLONE_VM|...}, 88) = 3895458
# The spawn syscalls are recognised by name at the start of the call.
#
# `clone3` matters: glibc's `pthread_create` uses it when the kernel offers
# it, and `strace -e trace=clone` alone does *not* match it.  A harness that
# traced only `clone` would happily report "no thread was created".
#
# The names are matched by hand, not with `=~`: zsh's `=~` applies *pattern*
# syntax, where a capture group does not mean what it means in a regular
# expression, and `<->` collides with the `<` operator of `[[ ... ]]`.
#
# SPAWN -- spawn counts, by phase (before/inside/after the window) and by
# kind (exec/clone/fork/total).  Every key is pre-set, so `no_unset` never
# trips over a missing one.
typeset -A SPAWN
for phase in before inside after; do
  for kind in exec clone fork total; do
    SPAWN["${phase}:${kind}"]=0
  done
done

# SPAWN_KIND -- the kind of the last line passed to spawn_kind.
SPAWN_KIND=""

# spawn_kind <trace line> -- sets SPAWN_KIND to exec|clone|fork, or to the
# empty string when the line is not a spawn.
#
# strace writes "PID name(args) = result", so the name is what is left
# between the PID and the opening parenthesis.
spawn_kind() {
  local line=$1 name
  name=${line#* }        # drop the PID
  name=${name%%"("*}      # drop the arguments
  name=${name%% *}        # drop anything after the name
  if [[ $name == "<..."* ]]; then
    # A resumed syscall: "<... clone3 resumed>".
    name=${name#"<... "}
    name=${name%% *}
  fi
  SPAWN_KIND=""
  case $name in
    execve|execveat) SPAWN_KIND=exec ;;
    clone|clone3)    SPAWN_KIND=clone ;;
    fork|vfork)      SPAWN_KIND=fork ;;
  esac
}

# count_spawns <trace file> <begin marker> <end marker>
#
# Fills SPAWN with the number of spawn syscalls before the window, inside
# it, and after it.  Returns 1 if a marker is missing, because a missing
# marker means the window does not exist and every count derived from it
# would be meaningless.
count_spawns() {
  local trace=$1 begin=$2 stop=$3
  local line state=before kind key
  local saw_begin=0 saw_stop=0
  for phase in before inside after; do
    for kind in exec clone fork total; do
      key="${phase}:${kind}"
      SPAWN[$key]=0
    done
  done
  while IFS= read -r line; do
    if (( ! saw_begin )) && [[ $line == *$begin* ]]; then
      saw_begin=1
      state=inside
      continue
    fi
    if (( ! saw_stop )) && [[ $line == *$stop* ]]; then
      saw_stop=1
      state=after
      continue
    fi
    spawn_kind "$line"
    kind=$SPAWN_KIND
    if [[ -n $kind ]]; then
      key="${state}:total"
      SPAWN[$key]=$(( SPAWN[$key] + 1 ))
      key="${state}:${kind}"
      SPAWN[$key]=$(( SPAWN[$key] + 1 ))
    fi
  done < "$trace"
  if (( ! saw_begin || ! saw_stop )); then
    return 1
  fi
  return 0
}

# count_marker <trace file> <marker> -- how many lines carry the marker.
count_marker() {
  local trace=$1 marker=$2
  local line n=0
  while IFS= read -r line; do
    if [[ $line == *$marker* ]]; then
      n=$(( n + 1 ))
    fi
  done < "$trace"
  print -r -- $n
}

# parse_syscall_total <strace -c output file>
#
# The summary's last line is
#     100.00    0.003895           7       503        51 total
# that is: percent, seconds, usecs/call, calls, errors, name.  The `errors`
# column is blank when it is zero, so the call count is found by counting
# the integer columns from the right: three of them means
# [usecs, calls, errors], two of them means [usecs, calls].
# Sets SYSCALL_TOTAL; returns 1 if the line cannot be understood.
SYSCALL_TOTAL=0
parse_syscall_total() {
  local file=$1
  local line last=""
  local -a f
  while IFS= read -r line; do
    if [[ $line == *total* ]]; then
      last=$line
    fi
  done < "$file"
  if [[ -z $last ]]; then
    return 1
  fi
  f=( ${=last} )
  if [[ $f[-1] != total ]]; then
    return 1
  fi
  # Drop the name, then look at what is left.
  f=( ${f[1,-2]} )
  local -a ints
  local v
  for v in "${(@)f}"; do
    if [[ $v == <-> ]]; then
      ints+=( "$v" )
    fi
  done
  case ${#ints} in
    2|3) SYSCALL_TOTAL=$ints[2] ;;
    *) return 1 ;;
  esac
  if [[ $SYSCALL_TOTAL != <-> ]]; then
    return 1
  fi
  return 0
}

# --------------------------------------------------------------------------
# Temporary files
# --------------------------------------------------------------------------

WORK=${TMPDIR:-/tmp}/zsh-pulse-bench-startup.$$
mkdir -p "$WORK" || fail "cannot create the temporary directory $WORK"

# The temporary directory goes away however this script ends -- including
# when it is killed, or when its output is piped into something that exits
# early (`| head`).
cleanup() {
  rm -rf -- "$WORK"
}
trap cleanup EXIT
trap 'cleanup; exit 141' PIPE
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

# --------------------------------------------------------------------------
# Report header
# --------------------------------------------------------------------------

print -r -- "zsh-pulse -- startup cost and syscall behaviour"
print -r -- "=================================================="
print -r -- ""
report "zsh version" "$(zsh --version)" ""
report "module path" "$MP" ""
report "module file" "$MP/zshpulse/core.so" ""
report "module size" "$(stat -c %s -- "$MP/zshpulse/core.so" 2>/dev/null || print '?')" "bytes"
report "timing samples per arm" "$SAMPLES" "samples"
report "hot-path builtin calls" "$CALLS" "calls"

# --------------------------------------------------------------------------
# 1. Load cost
# --------------------------------------------------------------------------

print -r -- ""
print -r -- "[1] load cost -- wall clock of a fresh zsh -f, per arm"
print -r -- "    (median of $SAMPLES interleaved samples, timed with zsh's own clock)"

sort_nums "${(@)T_BASE}"; stats_of
BASE_MED=$STAT_MED BASE_MIN=$STAT_MIN BASE_MAX=$STAT_MAX
sort_nums "${(@)T_PATH}"; stats_of
PATH_MED=$STAT_MED
sort_nums "${(@)T_LOAD}"; stats_of
LOAD_MED=$STAT_MED LOAD_MIN=$STAT_MIN LOAD_MAX=$STAT_MAX
if [[ -n $ARM_PLUGIN ]]; then
  sort_nums "${(@)T_PLUGIN}"; stats_of
  PLUGIN_MED=$STAT_MED
else
  PLUGIN_MED=""
fi

report "zsh -f (baseline)" "$(fmt_ms $BASE_MED)" "ms" \
  "min $(fmt_ms $BASE_MIN) ms, max $(fmt_ms $BASE_MAX) ms"
report "zsh -f + module_path assignment" "$(fmt_ms $PATH_MED)" "ms"
report "zsh -f + zmodload zshpulse/core" "$(fmt_ms $LOAD_MED)" "ms" \
  "min $(fmt_ms $LOAD_MIN) ms, max $(fmt_ms $LOAD_MAX) ms"
if [[ -n $ARM_PLUGIN ]]; then
  report "zsh -f + source zsh/init.zsh" "$(fmt_ms $PLUGIN_MED)" "ms"
fi

# The differences, taken per sample pair and then summarised.  The arms were
# interleaved, so this cancels the drift of a machine that gets busy halfway
# through the run -- which is larger than the effect being measured.
print -r -- ""
print -r -- "    (the differences below are per-sample, then the median of those)"
sort_nums "${(@)D_MODULE}"; stats_of
MODULE_COST=$STAT_MED
sort_nums "${(@)D_PATH}"; stats_of
PATH_COST=$STAT_MED
sort_nums "${(@)D_LOAD}"; stats_of
LOAD_COST=$STAT_MED
if [[ -n $ARM_PLUGIN ]]; then
  sort_nums "${(@)D_PLUGIN}"; stats_of
  PLUGIN_COST=$STAT_MED
else
  PLUGIN_COST=""
fi

# The noise floor: the median absolute deviation of the paired differences.
# A difference smaller than this is not resolved by the measurement, and is
# reported as such rather than as a number with a sign.
typeset -a D_ABS
D_ABS=()
for v in "${(@)D_MODULE}"; do
  d=$(( v - MODULE_COST ))
  if (( d < 0 )); then
    d=$(( -d ))
  fi
  D_ABS+=( $d )
done
sort_nums "${(@)D_ABS}"; stats_of
NOISE=$STAT_MED
if (( NOISE <= 0 )); then
  NOISE=0.001
fi

PATH_NOTE=""
if (( PATH_COST < 0 && PATH_COST > -NOISE )); then
  PATH_NOTE="within the noise: no measurable cost"
fi

report "module load cost (loaded - path)" "$(fmt_ms $MODULE_COST)" "ms"
report "module_path assignment cost (path - baseline)" "$(fmt_ms $PATH_COST)" "ms" "$PATH_NOTE"
if [[ -n $ARM_PLUGIN ]]; then
  report "whole plugin cost (plugin - baseline)" "$(fmt_ms $PLUGIN_COST)" "ms"
fi
report "whole plugin cost (loaded - baseline)" "$(fmt_ms $LOAD_COST)" "ms"
report "measurement noise (median absolute deviation)" "$(fmt_ms $NOISE)" "ms"

# A measurement that is not a measurement: a difference that is wildly
# negative means the clock or the arms are broken.
if (( LOAD_MED < BASE_MED - 5 )); then
  record_check "load cost is plausible" fail \
    "loading the module measured faster than not loading it ($LOAD_MED ms vs $BASE_MED ms)"
else
  record_check "load cost is plausible" ok \
    "module load costs $(fmt_ms $MODULE_COST) ms, whole plugin $(fmt_ms $LOAD_COST) ms"
fi

# --------------------------------------------------------------------------
# 2. Syscall counts
# --------------------------------------------------------------------------

print -r -- ""
print -r -- "[2] syscall counts -- strace -c -f"

# Is strace usable at all?  It may be missing, or present but unable to
# ptrace (a sandbox without CAP_SYS_PTRACE).  Either way the rest of the
# harness still runs.
STRACE_USABLE=0
if (( ! $+commands[strace] )); then
  report "strace syscall counts" "skipped: strace is not installed" ""
  record_check "syscall counts" skip "strace is not installed"
else
  STRACE=$commands[strace]
  if $STRACE -c -f -o "$WORK/probe.txt" /bin/true >/dev/null 2>"$WORK/probe.err"; then
    STRACE_USABLE=1
    STRACE_VERSION=$($STRACE --version 2>/dev/null | head -1) || STRACE_VERSION="unknown version"
  else
    report "strace syscall counts" "skipped: strace cannot run here" ""
    print -u2 -- "       $(head -1 -- "$WORK/probe.err" 2>/dev/null || print 'strace failed without a message')"
    record_check "syscall counts" skip "strace cannot run in this environment"
  fi
fi

if (( STRACE_USABLE )); then
  $STRACE -c -f -o "$WORK/sum_base.txt" zsh -f -c "$ARM_BASELINE" >/dev/null 2>&1 \
    || fail "strace failed on the baseline arm."
  $STRACE -c -f -o "$WORK/sum_load.txt" zsh -f -c "$ARM_LOADED" >/dev/null 2>&1 \
    || fail "strace failed on the module-loaded arm."

  if ! parse_syscall_total "$WORK/sum_base.txt"; then
    fail "cannot read the syscall summary strace produced for the baseline arm."
  fi
  BASE_SYSCALLS=$SYSCALL_TOTAL
  if ! parse_syscall_total "$WORK/sum_load.txt"; then
    fail "cannot read the syscall summary strace produced for the loaded arm."
  fi
  LOAD_SYSCALLS=$SYSCALL_TOTAL

  report "zsh -f (baseline) syscalls" "$BASE_SYSCALLS" "syscalls"
  report "zsh -f + module loaded syscalls" "$LOAD_SYSCALLS" "syscalls"
  report "difference (loaded - baseline)" "$(( LOAD_SYSCALLS - BASE_SYSCALLS ))" "syscalls"
  report "strace" "$STRACE_VERSION" ""

  if (( BASE_SYSCALLS <= 0 )); then
    record_check "syscall counts" fail "the baseline arm reported $BASE_SYSCALLS syscalls"
  else
    record_check "syscall counts" ok "baseline $BASE_SYSCALLS, loaded $LOAD_SYSCALLS"
  fi
fi

# --------------------------------------------------------------------------
# 3. Hot path: no process may be spawned
# --------------------------------------------------------------------------

print -r -- ""
print -r -- "[3] hot path -- spawns while the builtin runs"

# The inner script.  The builtin is called with a *changing* buffer, so the
# module really re-lexes the line on every call instead of taking the
# "nothing changed" path.
#
# `whence -w` is checked inside the inner shell, so a run whose module did
# not load fails loudly instead of measuring nothing.  The output is read
# through a redirection rather than a command substitution, because a
# command substitution forks -- and a fork would show up in this very
# measurement.
BUF_A='git commit --amend --no-edit'
BUF_B='git commit --amend --no-edix'
BUF_A_Q=${(qq)BUF_A}
BUF_B_Q=${(qq)BUF_B}

WHENCE_FILE=$WORK/whence.txt
WHENCE_Q=${(qq)WHENCE_FILE}

HOT_SCRIPT="module_path=($MP_Q \$module_path)
zmodload zshpulse/core
whence -w zsh-pulse-highlight >$WHENCE_Q 2>/dev/null
read -r whence_line <$WHENCE_Q
[[ \$whence_line == *builtin* ]] || { print -u2 'ZSH_PULSE_FATAL: the builtin is missing'; exit 1 }
print -u2 -- 'ZSH_PULSE_HOT_BEGIN'
for i in {1..$CALLS}; do
  if (( i % 2 )); then BUFFER=$BUF_A_Q; else BUFFER=$BUF_B_Q; fi
  CURSOR=8
  zsh-pulse-highlight
done
print -u2 -- 'ZSH_PULSE_HOT_END'"

if (( ! STRACE_USABLE )); then
  report "hot-path spawn count" "skipped: strace is not usable" ""
  record_check "hot path spawns no process" skip "strace is not usable"
else
  # 3a. The run with the module's own diagnostics on.  `ZSH_PULSE_DEBUG`
  #     makes the module print what it read, so the trace shows how many
  #     calls really reached the native code.
  HOT_STATUS=0
  ZSH_PULSE_DEBUG=1 \
    $STRACE -f -s 200 \
      -e trace=execve,execveat,clone,clone3,fork,vfork,write \
      -o "$WORK/hot_debug.txt" \
      -- zsh -f -c "$HOT_SCRIPT" >/dev/null 2>&1 || HOT_STATUS=$?
  if (( HOT_STATUS != 0 )); then
    fail "the hot-path run failed (exit status $HOT_STATUS)." \
      "the inner shell refuses to run without the builtin"
  fi

  if ! count_spawns "$WORK/hot_debug.txt" ZSH_PULSE_HOT_BEGIN ZSH_PULSE_HOT_END; then
    fail "the hot-path trace has no begin/end markers, so the spawn window is undefined."
  fi

  # Every call must have reached the builtin: the module prints one
  # "read BUFFER=" line per call.
  REACHED=$(count_marker "$WORK/hot_debug.txt" 'read BUFFER=')

  report "calls made" "$CALLS" "calls"
  report "calls that reached the builtin" "$REACHED" "calls"
  report "spawns inside the window" "${SPAWN[inside:total]}" "spawns"
  report "  execve / execveat" "${SPAWN[inside:exec]}" "spawns"
  report "  clone / clone3" "${SPAWN[inside:clone]}" "spawns"
  report "  fork / vfork" "${SPAWN[inside:fork]}" "spawns"
  report "spawns per builtin call" \
    "$(printf '%.3f' $(( SPAWN[inside:total] * 1.0 / CALLS )))" "spawns/call"
  report "spawns before the window (load time)" "${SPAWN[before:total]}" "spawns"
  report "  execve / execveat" "${SPAWN[before:exec]}" "spawns" "the shell itself"
  report "  clone / clone3" "${SPAWN[before:clone]}" "spawns" "the engine worker thread"
  report "  fork / vfork" "${SPAWN[before:fork]}" "spawns"
  report "spawns after the window (exit)" "${SPAWN[after:total]}" "spawns"

  if (( REACHED != CALLS )); then
    record_check "every call reached the builtin" fail \
      "$REACHED of $CALLS calls produced the module's own diagnostic"
  else
    record_check "every call reached the builtin" ok \
      "$REACHED of $CALLS calls produced the module's own diagnostic"
  fi

  if (( SPAWN[before:total] < 1 )); then
    record_check "the trace can see a spawn" fail \
      "no execve before the window: the trace is not counting spawns at all"
  else
    record_check "the trace can see a spawn" ok \
      "${SPAWN[before:total]} spawn(s) before the window, including the shell's own execve"
  fi

  if (( SPAWN[inside:total] != 0 )); then
    record_check "hot path spawns no process" fail \
      "${SPAWN[inside:total]} spawn syscall(s) inside the hot-path window"
  else
    record_check "hot path spawns no process" ok "0 spawn syscalls inside the window"
  fi

  # 3b. The same run without the diagnostics, i.e. the production path: the
  #     module does no formatting and no writing at all.
  HOT_STATUS=0
  $STRACE -f -s 200 \
    -e trace=execve,execveat,clone,clone3,fork,vfork,write \
    -o "$WORK/hot_plain.txt" \
    -- zsh -f -c "$HOT_SCRIPT" >/dev/null 2>&1 || HOT_STATUS=$?
  if (( HOT_STATUS != 0 )); then
    fail "the hot-path run without diagnostics failed (exit status $HOT_STATUS)."
  fi

  if ! count_spawns "$WORK/hot_plain.txt" ZSH_PULSE_HOT_BEGIN ZSH_PULSE_HOT_END; then
    fail "the hot-path trace has no begin/end markers, so the spawn window is undefined."
  fi
  report "spawns inside the window (no diagnostics)" "${SPAWN[inside:total]}" "spawns"
  report "  execve / execveat" "${SPAWN[inside:exec]}" "spawns"
  report "  clone / clone3" "${SPAWN[inside:clone]}" "spawns"
  report "  fork / vfork" "${SPAWN[inside:fork]}" "spawns"

  if (( SPAWN[inside:total] != 0 )); then
    record_check "hot path spawns no process (production path)" fail \
      "${SPAWN[inside:total]} spawn syscall(s) inside the hot-path window"
  else
    record_check "hot path spawns no process (production path)" ok "0 spawn syscalls inside the window"
  fi

  # 3c. The control.  The same counter that just reported "zero" must find
  #     a process that is spawned on purpose inside the window.  If it does
  #     not, the "zero" above is a broken counter, not a measurement.
  CONTROL_SCRIPT="print -u2 -- 'ZSH_PULSE_HOT_BEGIN'; zsh -f -c ':'; print -u2 -- 'ZSH_PULSE_HOT_END'"
  CONTROL_STATUS=0
  $STRACE -f -s 200 \
    -e trace=execve,execveat,clone,clone3,fork,vfork,write \
    -o "$WORK/control.txt" \
    -- zsh -f -c "$CONTROL_SCRIPT" >/dev/null 2>&1 || CONTROL_STATUS=$?
  if (( CONTROL_STATUS != 0 )); then
    fail "the control run failed (exit status $CONTROL_STATUS)."
  fi

  if ! count_spawns "$WORK/control.txt" ZSH_PULSE_HOT_BEGIN ZSH_PULSE_HOT_END; then
    fail "the control trace has no begin/end markers, so the spawn window is undefined."
  fi
  report "control: spawns inside the window" "${SPAWN[inside:total]}" "spawns" "a child spawned on purpose"
  if (( SPAWN[inside:total] < 1 )); then
    record_check "the counter detects a real spawn" fail \
      "a child process spawned inside the window was not detected"
  else
    record_check "the counter detects a real spawn" ok \
      "${SPAWN[inside:total]} spawn syscall(s) detected for a child spawned on purpose"
  fi
fi

# --------------------------------------------------------------------------
# 4. The checks
# --------------------------------------------------------------------------

print -r -- ""
print -r -- "[4] checks"
for (( i = 1; i <= ${#CHECK_NAME}; i++ )); do
  case $CHECK_STATE[i] in
    ok)   mark="ok  " ;;
    fail) mark="FAIL" ;;
    skip) mark="skip" ;;
    *)    mark="??" ;;
  esac
  printf '  %-4s %-46s : %s\n' "$mark" "$CHECK_NAME[i]" "$CHECK_DETAIL[i]"
done

print -r -- ""
if (( FAILURES > 0 )); then
  print -u2 -- "FAILED: $FAILURES check(s) did not pass."
  exit 1
fi
print -r -- "All checks passed."
exit 0
