#!/usr/bin/env bash
# Build the cdylib and install it where the benchmarks and the zsh adapter
# expect it: `<repo>/bench/mp/shellpulse/highlight.so`.
#
# The nested `shellpulse/` directory and the dotless name are mandatory:
# zsh's `modname_ok()` (zsh-src/Src/module.c:2177-2186) accepts only
# slash-separated identifiers, so the module is loaded as
# `zmodload shellpulse/highlight`. Absolute paths and `.so` suffixes are
# rejected.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
profile="${1:-release}"

echo "building zsh-module ($profile)…"
cargo build --profile "$profile" -p zsh-module

dest="$repo/bench/mp/shellpulse"
mkdir -p "$dest"
cp "$repo/target/$profile/libshellpulse_highlight.so" "$dest/highlight.so"
echo "installed -> $dest/highlight.so"

# Fail loudly here rather than mid-benchmark.
if ! zsh -f -c "module_path=('$repo/bench/mp' \$module_path); zmodload shellpulse/highlight; whence -w shell-pulse-highlight"; then
  echo "ERROR: module built but zsh refused to load it" >&2
  exit 1
fi