#!/bin/bash
# run.sh <ext> [letters] [arity]: runs driver.php under our php ($OURS) and the C php,
# writes $OUT/out/<tag>.{ours,c,diff} and prints how many lines differ.
D=$(cd "$(dirname "$0")" && pwd)/driver.php
OURS=${OURS:-target/debug/php}
OUT=${OUT:-/tmp/differential}
ext=$1; let=${2:-*}; ar=${3:-all}
tag=$(echo "${ext}_${let}_${ar}" | tr -c 'a-zA-Z0-9_\n-' 'x')
mkdir -p $OUT/out $OUT/sigs; o=$OUT/out/$tag
EXTS=""
for e in ctype mbstring tokenizer iconv bcmath calendar gmp; do EXTS="$EXTS -d extension=$e"; done
OURS=$(realpath "$OURS"); cd "$OUT"
C="timeout 60 taskset -c 12 /usr/bin/php8.5 -n -d memory_limit=1G -d error_reporting=-1 $EXTS $D"
R="timeout 60 taskset -c 12 $OURS -n -d memory_limit=1G -d error_reporting=-1 $D"
S=$OUT/sigs/$ext
[ -s $S ] || $C "$ext" '*' sigs > $S
$R "$ext" "$let" "$ar" $S > $o.ours 2>&1
$C "$ext" "$let" "$ar" $S > $o.c 2>&1
diff -a $o.c $o.ours > $o.diff
echo "$tag: $(wc -l < $o.c) lines C, $(wc -l < $o.ours) ours, $(grep -c '^<' $o.diff) differ"
