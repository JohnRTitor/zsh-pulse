#!/usr/bin/env zsh
# Per-keystroke highlight cost, measured by calling each highlighter's entry
# point directly N times on a realistic buffer. Isolation of the *work*, not the
# ZLE plumbing.
setopt extendedglob no_nomatch
N=${1:-2000}; SHAPE=${2:-A}; MOD=${3:-none}
case $SHAPE in
 A) LINE='git commit --amend --no-edit';;
 B) LINE='cargo build --release 2>&1 | tee /tmp/out.log && kubectl apply -f deploy.yaml --namespace prod';;
 C) LINE='echo "$(date +%s)" ${HOME}/.cache/foo/*.log `hostname` | grep -E "^ok$" > /dev/null';;
 L200) LINE=$(printf 'cmd%d --flag=value /tmp/path/to/file.txt; ' {1..20});;
 L400) LINE=$(printf 'cmd%d --flag=value /tmp/path/to/file.txt; ' {1..40});;
 L800) LINE=$(printf 'cmd%d --flag=value /tmp/path/to/file.txt; ' {1..80});;
esac
zmodload zsh/datetime 2>/dev/null; zmodload zsh/zleparameter 2>/dev/null
if [[ $MOD == fsh ]]; then
  fpath=( $PWD/fsh $fpath ); source fsh/fast-syntax-highlighting.plugin.zsh 2>/dev/null
  FN=_zsh_highlight
elif [[ $MOD == zsyh ]]; then
  fpath=( $PWD/zsh-syntax-highlighting $fpath )
  ZSH_HIGHLIGHT_HIGHLIGHTERS=( main-highlighter )
  source zsh-syntax-highlighting/zsh-syntax-highlighting.zsh 2>/dev/null
  ZSH_HIGHLIGHT_MAXLENGTH=0; FN=_zsh_highlight
else
  FN=hl_noop; hl_noop() { : }
fi
BUFFER=$LINE; CURSOR=${#LINE}; WIDGET=zle-line-init; PENDING=0; region_highlight=()
for ((i=0;i<50;i++)); do $FN 2>/dev/null; done
t0=$(print -P %D{%s.%N})
for ((i=0;i<N;i++)); do BUFFER="${LINE} "; BUFFER=$LINE; $FN 2>/dev/null; done
t1=$(print -P %D{%s.%N})
print -r -- "$MOD $SHAPE spans=${#region_highlight} $N $t0 $t1"
