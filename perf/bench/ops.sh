#!/bin/bash
# Usage: perf/bench/ops.sh <php-binary> [bench...]
# User-space instructions per loop iteration (startup cancelled, see irun.sh)
# for <php-binary> and /usr/bin/php side by side, plus the ratio.  The
# empty_loop row is the loop's own cost; other rows include it.
# Needs `sudo -n perf`; PHP itself runs as the invoking user (setpriv).
dir=$(cd "$(dirname "$0")" && pwd)
bin=$(readlink -f "$1"); shift
n=${N:-200000}
uid=$(id -u); gid=$(id -g)
ins() { sudo -n perf stat -x, -e instructions:u -- setpriv --reuid="$uid" --regid="$gid" --init-groups \
        timeout --kill-after=2s "${BENCH_TIMEOUT:-15s}" "$1" -d memory_limit=1G -d opcache.enable_cli=0 "$dir/$2.php" "$3" 2>&1 >/dev/null | awk -F, '/instructions/{print $1}'; }
per() { local a c; a=$(ins "$1" "$2" "$n"); c=$(ins "$1" "$2" $((n / 10))); echo $(( (a - c) / (n - n / 10) )); }
benches=("$@")
[ ${#benches[@]} -eq 0 ] && benches=(empty_loop int_add float_mul is_smaller_jmp concat_assign dim_read dim_write hash_read prop_read prop_write strlen abs count userfunc userfunc_typed method static_method closure new_obj)
printf 'bench\tcand_ins\tphp_ins\tratio\n'
for b in "${benches[@]}"; do
    c=$(per "$bin" "$b"); r=$(per /usr/bin/php "$b")
    printf '%s\t%s\t%s\t%s\n' "$b" "$c" "$r" "$(awk -v a="$c" -v b="$r" 'BEGIN{printf "%.1f", a/(b>0?b:1)}')"
done
