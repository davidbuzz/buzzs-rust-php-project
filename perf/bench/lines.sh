#!/bin/bash
# Where do the instructions actually go?  A source-line histogram of one run.
#
#   perf/bench/lines.sh <php-binary> <script.php> [reps] [top-N]
#
# `perf report` buckets by symbol, which hides the cost inside a big interpreter
# loop or call path: the answer we act on is usually one line.  This samples one
# run, maps the hot instruction pointers through the binary's line tables
# (CARGO_PROFILE_RELEASE_DEBUG=line-tables-only), and prints the hottest source
# lines with their share of the samples and the function they belong to.
#
# Needs the same permission as the rest of the bench kit (`sudo -n perf`).
bin=$(readlink -f "$1" 2>/dev/null || echo "$1"); shift
script=$(readlink -f "$1" 2>/dev/null || echo "$1"); shift
reps=${1:-2000000}
top=${2:-20}

# perf record creates the file itself (as root), so the name must not exist yet.
data=$(mktemp -u "${TMPDIR:-/tmp}/lines.XXXXXX.data")
ips=$(mktemp -t lines.XXXXXX.ips)
trap 'rm -f "$data" "$ips"' EXIT

sudo -n perf record -q -o "$data" -- "$bin" "$script" "$reps" >/dev/null 2>&1 || {
    echo "record failed (needs: sudo -n perf)" >&2; exit 1; }

# Samples per instruction pointer: kernel frames and unresolved addresses out.
sudo -n perf script -i "$data" -F ip,sym 2>/dev/null |
    awk '$2 !~ /^\[/ && $2 != "" {c[$1]++; n++} END {for (ip in c) printf "%d %s\n", c[ip], ip}' |
    sort -rn > "$ips"
total=$(awk '{s+=$1} END{print s+0}' "$ips")

head -400 "$ips" > "$ips.head"
awk '{print $2}' "$ips.head" | addr2line -e "$bin" -C -f > "$ips.symloc"
awk 'NR%2==1 {fn=$0; next} {print fn "\t" $0}' "$ips.symloc" > "$ips.pair"
paste -d'\t' <(awk '{print $1}' "$ips.head") "$ips.pair" |
    awk -F'\t' -v total="$total" '
        $3 !~ /\?/ && $3 !~ /\/rustc\/|\/rust\/|\/cargo\/registry/ { share[$3] += $1; fn[$3] = $2 }
        END { for (l in share) printf "%6.2f%% %8d  %s  <- %s\n", 100*share[l]/total, share[l], l, fn[l] }' |
    sort -rn | head -"$top"
rm -f "$ips.head" "$ips.symloc" "$ips.pair"
