#!/usr/bin/env zsh
# Per-native-call cost of the loaded zsh module.
#
# Isolates the FFI boundary: loads shellpulse/highlight, then invokes the
# builtin N times on a *changing* buffer. The empty-loop arm subtracts zsh's
# own interpretation cost, so what is left is the marginal cost of one
# keystroke's worth of highlighting.
#
# The measurement exercises the real path:
#
#   * a `region_highlight` parameter exists, so the module actually publishes
#     the highlights (without one it returns early, and the number measured
#     would be the cost of doing nothing), and
#   * the line *changes* on every iteration, so the module really re-lexes it.
#     A fixed buffer would take the "nothing changed" path every time.
#
# Usage:  ./bench_module_call.zsh [iterations] [buffer-length]
# Needs:  hyperfine (nix shell nixpkgs#hyperfine) and a built cdylib.
#         Without hyperfine a less accurate, zsh-clock based measurement is
#         used instead, so the harness still runs anywhere.
emulate -L zsh
setopt no_unset

N=${1:-200000}
LEN=${2:-28}
REPO=${0:A:h:h}
MP="$REPO/bench/mp"

if [[ ! -f $MP/shellpulse/highlight.so ]]; then
  print -u2 "module not installed. Run: ./bench/install.sh"
  exit 1
fi

# Build a buffer of roughly LEN bytes from a realistic command shape.
if (( LEN <= 28 )); then
  LINE='git commit --amend --no-edit'
else
  typeset -a parts
  parts=(git commit --amend --no-edit)
  # `${#parts}` is the number of elements; `$#parts[1]` is the length of
  # the first one, which would make this loop never end.
  while (( ${#parts} * 20 < LEN )); do
    parts+=(--flag=value /tmp/path/to/file.txt)
  done
  LINE="${(j: :)parts}"
fi

BUILTIN='shell-pulse-highlight'
LOAD="module_path=($MP \$module_path); zmodload shellpulse/highlight"

# Quote the buffer *here*, in the outer shell.
#
# Two traps this guards against, both of which silently produce a meaningless
# number rather than an error:
#   - leaving `${(qqq)LINE}` for the inner zsh to expand benchmarks an EMPTY
#     buffer;
#   - `${(qqq)}`'s double quotes break the command entirely, so zsh exits
#     immediately and the run looks impossibly fast.
#
# `${(qq)}` emits single quotes, which survive that wrapping.
if [[ $LINE == *"'"* ]]; then
  print -u2 "ERROR: benchmark buffer must not contain a single quote" >&2
  exit 1
fi
QUOTED_LINE=${(qq)LINE}
# The second version of the line, quoted the same way. It is built from the
# *raw* line and then quoted, so that the quoting cannot be cut in half.
# The command word is one character longer, so the styled region moves.
LINE_B_RAW="x$LINE"
QUOTED_LINE_B=${(qq)LINE_B_RAW}
# `WIDGET` is what tells the module it is inside a line editor: the ZLE
# special parameters only exist there (`zsh-src/Src/Zle/zle_params.c:141`).
# This harness is not a widget, so it stands in for one. The *real* path —
# the ZLE special parameter — is measured by `bench_zpty.zsh`.
SETUP="$LOAD; typeset -ga region_highlight; region_highlight=(); WIDGET=test-widget; CURSOR=8"

# Two versions of the line. The loop alternates between them, so *every*
# iteration is a real edit.
#
# They must differ in their *styling*, not only in a byte inside a word that
# is styled the same either way: the module skips the write when the spans
# are unchanged, and a harness that measured that skip would report the cost
# of doing nothing.
LINE_A=$QUOTED_LINE
LINE_B=$QUOTED_LINE_B

# The deliberate-failure check: the module must actually publish, or the
# measurement below is the cost of doing nothing.
# The `\${...}` is escaped so that the *inner* shell expands it.
published=$(zsh -f -c "$SETUP; typeset -ga region_highlight; region_highlight=(); BUFFER=$LINE_A; CURSOR=8; $BUILTIN --region-highlight; print -r -- \"\${#region_highlight}\"")
if [[ $published != 1 ]]; then
  print -u2 "ERROR: the module published $published region(s), expected 1;" >&2
  print -u2 "       this measurement would be the cost of doing nothing." >&2
  exit 1
fi

print -u2 "buffer: ${#LINE} bytes, $N iterations"
if [[ $QUOTED_LINE == '${(qq)LINE}' ]]; then
  print -u2 "ERROR: buffer quoting did not expand (got: $QUOTED_LINE)" >&2
  exit 1
fi

# The two arms. They differ in exactly one thing: whether the builtin is
# called. The difference is therefore the cost of one keystroke.
ARM_WITH="for i in {1..$N}; do if (( i % 2 )); then BUFFER=$LINE_A; else BUFFER=$LINE_B; fi; $BUILTIN --region-highlight; done"
ARM_WITHOUT="for i in {1..$N}; do if (( i % 2 )); then BUFFER=$LINE_A; else BUFFER=$LINE_B; fi; :; done"

if (( $+commands[hyperfine] )); then
  # `-N` disables hyperfine's own shell so it measures zsh, not the wrapper.
  hyperfine -N --warmup 3 -r 15 \
    "zsh -f -c \"$SETUP; $ARM_WITH\"" \
    "zsh -f -c \"$SETUP; $ARM_WITHOUT\""
else
  # Fallback: zsh's own clock. Less accurate than hyperfine, but the harness
  # still measures the same thing and still runs.
  zmodload zsh/datetime
  print -u2 "hyperfine not found; using zsh's clock instead."
  typeset -F SECONDS
  for arm in with without; do
    if [[ $arm == with ]]; then
      script="$ARM_WITH"
    else
      script="$ARM_WITHOUT"
    fi
    start=$EPOCHREALTIME
    zsh -f -c "$SETUP; $script"
    end=$EPOCHREALTIME
    print -r -- "arm $arm: $(( end - start )) s"
  done
fi

print -u2 ""
print -u2 "Subtract the two arms and divide by $N to get the per-call cost."
print -u2 ""
print -u2 "The second arm does everything the first does except call the"
print -u2 "builtin, so the difference is the cost of one keystroke's worth of"
print -u2 "highlighting: read the line, re-lex it, build the regions and"
print -u2 "hand them to zsh."
