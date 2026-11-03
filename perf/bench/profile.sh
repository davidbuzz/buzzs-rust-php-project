#!/bin/bash
# Usage: perf/bench/profile.sh <php-binary> <script.php> [outdir]
# Samples one run with `perf record` (needs `sudo -n perf`; the PHP process
# itself runs as the invoking user via setpriv) and prints
#   1. the flat self-time profile (top 40 symbols),
#   2. the same samples bucketed into cost categories (categorize.py).
# Build the binary with CARGO_PROFILE_RELEASE_DEBUG=line-tables-only for
# usable symbols.  Output files land in [outdir] (default: a mktemp dir).
dir=$(cd "$(dirname "$0")" && pwd)
bin=$(readlink -f "$1"); shift
script=$(readlink -f "$1")
out=${2:-$(mktemp -d)}
mkdir -p "$out"
args=("$script")
uid=$(id -u); gid=$(id -g)
sudo -n perf record -q -F "${FREQ:-1999}" -o "$out/perf.data" -- \
    setpriv --reuid="$uid" --regid="$gid" --init-groups \
    taskset -c "${CORE:-$(( $(nproc) - 1 ))}" "$bin" -d memory_limit=1G -d opcache.enable_cli=0 "${args[@]}" >/dev/null
sudo -n chown "$uid:$gid" "$out/perf.data"
perf report -i "$out/perf.data" --no-children --sort symbol --percent-limit 0.5 --stdio 2>/dev/null \
    | grep -v '^#' | grep -v '^$' | head -40 | tee "$out/flat.txt"
echo "--- categories"
perf report -i "$out/perf.data" --no-children --sort symbol --stdio -F overhead,sym 2>/dev/null \
    | grep -v '^#' | python3 -I "$dir/categorize.py" | tee "$out/categories.txt"
echo "(data in $out)"
