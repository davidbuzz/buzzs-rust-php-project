#!/bin/sh
# Builds the libbcmath oracle (php-src master libbcmath + shim) into $1 (default: ./build).
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
SRC=$HERE/../../../../modules/php-src/ext/bcmath/libbcmath/src
OUT=${1:-$HERE/build}
mkdir -p "$OUT/ext/libbcmath/src"
cp "$SRC"/*.c "$SRC"/*.h "$OUT/ext/libbcmath/src/"
cp "$HERE/shim/php_bcmath.h" "$OUT/ext/php_bcmath.h"
gcc -O2 -std=gnu11 -w -I"$HERE/shim" -I"$OUT/ext" -I"$OUT/ext/libbcmath/src" \
    "$OUT"/ext/libbcmath/src/*.c "$HERE/oracle.c" -o "$OUT/bcoracle"
echo "$OUT/bcoracle"
