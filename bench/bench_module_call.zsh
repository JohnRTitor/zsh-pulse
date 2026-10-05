#!/usr/bin/env zsh
# Per-native-call cost of the loaded zsh module.
#
# Isolates the FFI boundary: loads shellpulse/highlight, then invokes the
# builtin N times on a fixed buffer. The empty-loop arm subtracts zsh's own
# interpretation cost, so what is left is the marginal cost of one native call.
#
# Usage:  ./bench_module_call.zsh [iterations] [buffer-length]
# Needs:  hyperfine (nix shell nixpkgs#hyperfine) and a built cdylib.
emulate -L zsh
setopt no_unset

N=${1:-200000}
LEN=${2:-28}
REPO=${0:A:h:h}
MP="$REPO/bench/mp"

if ! (( $+commands[hyperfine] )); then
  print -u2 "hyperfine not found. Run under: nix shell nixpkgs#hyperfine --command $0"
  exit 1
fi
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
  while (( $#parts[1] + 1 < LEN / 20 )); do
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
#   - `${(qqq)}` emits double quotes, which terminate the shell command
#     hyperfine passes to `sh -c`, so zsh exits immediately and the run looks
#     impossibly fast.
#
# `${(qq)}` emits single quotes, which survive that wrapping.
if [[ $LINE == *"'"* ]]; then
  print -u2 "ERROR: benchmark buffer must not contain a single quote" >&2
  exit 1
fi
QUOTED_LINE=${(qq)LINE}
SETUP="$LOAD; BUFFER=$QUOTED_LINE; CURSOR=24"

print -u2 "buffer: ${#LINE} bytes, $N iterations"
if [[ $QUOTED_LINE == '${(qq)LINE}' ]]; then
  print -u2 "ERROR: buffer quoting did not expand (got: $QUOTED_LINE)" >&2
  exit 1
fi

# `-N` disables hyperfine's own shell so it measures zsh, not the wrapper.
hyperfine -N --warmup 3 -r 15 \
  "zsh -f -c \"$SETUP; for i in {1..$N}; do $BUILTIN; done\"" \
  "zsh -f -c \"for i in {1..$N}; do :; done\""

print -u2 ""
print -u2 "Subtract the two arms and divide by $N to get the per-call cost."