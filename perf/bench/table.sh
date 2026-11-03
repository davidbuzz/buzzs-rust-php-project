#!/bin/bash
# Usage: perf/bench/table.sh <scale> <php-binary>...
# Runs every perf/bench/*.php benchmark (iterations = default * scale) three
# times per binary and prints the best in-script hrtime ns/op as a table.
# Every binary gets -d memory_limit=1G -d opcache.enable_cli=0 (CLAUDE.md).
dir=$(cd "$(dirname "$0")" && pwd)
scale=$1; shift
printf '%-16s' bench; for b in "$@"; do printf ' %14s' "$(basename "$b" | cut -c1-14)"; done; echo
for f in "$dir"/*.php; do
    name=$(basename "$f" .php)
    [ "$name" = all ] && continue
    def=$(grep -o '?? [0-9]*' "$f" | head -1 | awk '{print $2}')
    n=$(awk -v d="$def" -v s="$scale" 'BEGIN{n=int(d*s); if(n<1000)n=1000; print n}')
    printf '%-16s' "$name"
    for b in "$@"; do
        best=
        for _ in 1 2 3; do
            v=$("$b" -d memory_limit=1G -d opcache.enable_cli=0 "$f" "$n" | awk '{print $2}')
            if [ -z "$best" ] || awk -v a="$v" -v b="$best" 'BEGIN{exit !(a<b)}'; then best=$v; fi
        done
        printf ' %14s' "$best"
    done
    echo
done
