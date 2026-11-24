#!/bin/bash
# Usage: perf/bench/startup.sh <php-binary>...
# Process startup cost of `php -r 'echo 1;'`: user-space instructions and
# CPU time (task-clock, ms) averaged over 5 runs with `perf stat`, plus the
# best-of-5 wall time. CPU numbers are what matter on a loaded box; wall time
# there mostly measures run-queue waiting. Needs `sudo -n perf` (falls back
# to /usr/bin/time when perf is unavailable).
for b in "$@"; do
    extra=
    case "$b" in */usr/bin/php8.5|php) extra="-d memory_limit=1G -d opcache.enable_cli=0" ;; esac
    best=
    for _ in 1 2 3 4 5; do
        s=$(date +%s%N); "$b" $extra -r 'echo 1;' >/dev/null 2>&1; e=$(date +%s%N)
        w=$(( (e - s) / 1000000 ))
        if [ -z "$best" ] || [ "$w" -lt "$best" ]; then best=$w; fi
    done
    if sudo -n perf stat -x, -e instructions:u true >/dev/null 2>&1; then
        stat=$(sudo -n perf stat -x, -e instructions:u,task-clock -r 5 "$b" $extra -r 'echo 1;' 2>&1 >/dev/null |
            awk -F, '/instructions/{i=$1} /task-clock/{t=$1} END{printf "%8.1f M instr  %7.1f ms CPU", i/1e6, t}')
    else
        stat=$( { /usr/bin/time -f '%U %S' "$b" $extra -r 'echo 1;' >/dev/null; } 2>&1 | awk '{printf "%7.1f ms CPU", ($1+$2)*1000}')
    fi
    printf '%-28s %s  %6d ms wall (best of 5)\n' "$(basename "$b")" "$stat" "$best"
done
