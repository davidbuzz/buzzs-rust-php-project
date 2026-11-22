#!/usr/bin/env bash
#
# Fetch the php-src `.phpt` test tree that the conformance suite runs.
#
# The full php-src clone is 237 MB; the test directories alone are 25,437 files and 46 MB
# (127 MB on disk: mostly small files). This script fetches only those, into the same paths
# upstream uses (`tests/`, `Zend/tests/`, `ext/**/tests/`, `sapi/**/tests/`), so
# `phpt-scope.txt` and `phpt-baseline.txt`, which are written relative to `modules/php-src/`,
# line up as-is.
#
# The commit is pinned: the conformance pass rate and `phpt-baseline.txt` are only
# meaningful against one php-src revision, so moving to a newer one is a deliberate
# edit of PHP_SRC_SHA below, followed by a fresh baseline.
#
#   scripts/fetch-phpt-tests.sh            # fetch into modules/php-src
#   scripts/fetch-phpt-tests.sh --verify   # check what is there; fetch nothing
#
# The tests are under the PHP License 3.01.

set -euo pipefail

PHP_SRC_URL='https://github.com/php/php-src'
# The revision the conformance numbers and `phpt-baseline.txt` are measured against: php-src
# master of 22 Nov 2026 (moved up from 0c7255dc521, 2 Nov, on 22 Nov 2026 at the owner's
# request: 716 commits, 207 more `.phpt` files; the last 3, after 6e4c78a1723 of 21 Nov,
# change no test).
PHP_SRC_SHA='cb90f9a237757209ecd2e67a7e0c950f36ba6ea1'  # "streams: voidify parse_unix_address", 22 Nov 2026
PHP_SRC_DESC='php-src master of 22 Nov 2026'

# Every .phpt in the tree we run lives under a directory named `tests`, and the
# support files they include (`skipif_root.inc`, `helper.inc`, locale data) live in
# those directories too, so these four prefixes are the complete payload. Checked
# against a full clone at this revision: the 23,999 `.phpt` files match one for one,
# none missing and none extra. `**` is needed for `ext/intl/uchar/tests/`, which sits
# two levels below `ext/`.
# Plus php-src's two benchmark scripts, inputs of `perf/bench/timing.py` and
# `scripts/acceptance.sh`.
SPARSE_PATHS=(
    '/tests/'
    '/Zend/tests/'
    '/ext/**/tests/'
    '/sapi/**/tests/'
    '/Zend/bench.php'
    '/Zend/micro_bench.php'
)

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="$repo_root/modules/php-src"
verify_only=false
if [[ "${1:-}" == '--verify' ]]; then
    verify_only=true
fi

current_sha() {
    git -C "$dest" rev-parse HEAD 2>/dev/null || true
}

if $verify_only; then
    have="$(current_sha)"
    if [[ -z "$have" ]]; then
        echo "modules/php-src: not fetched. Run scripts/fetch-phpt-tests.sh" >&2
        exit 1
    fi
    if [[ "$have" != "$PHP_SRC_SHA" ]]; then
        echo "modules/php-src: at ${have:0:12}, but this project is baselined on" >&2
        echo "  ${PHP_SRC_SHA:0:12} ($PHP_SRC_DESC)." >&2
        echo "Re-fetch, or re-baseline phpt-baseline.txt if you intend to move up." >&2
        exit 1
    fi
    echo "modules/php-src: at $PHP_SRC_SHA ($PHP_SRC_DESC)"
    exit 0
fi

have="$(current_sha)"
if [[ "$have" == "$PHP_SRC_SHA" ]]; then
    echo "modules/php-src: already at $PHP_SRC_SHA, nothing to do"
    exit 0
fi
if [[ -n "$have" ]]; then
    echo "modules/php-src: at ${have:0:12}, expected ${PHP_SRC_SHA:0:12}." >&2
    echo "Refusing to touch a checkout this script did not make. Remove it, or" >&2
    echo "git -C modules/php-src checkout $PHP_SRC_SHA" >&2
    exit 1
fi

echo "Fetching php-src test directories at ${PHP_SRC_SHA:0:12} into modules/php-src"
mkdir -p "$dest"
cd "$dest"
git init -q
git remote add origin "$PHP_SRC_URL"
# Non-cone sparse checkout (writing .git/info/sparse-checkout directly), so `**` can
# match `ext/intl/uchar/tests/` — a test directory two levels below `ext/`.
git config core.sparseCheckout true
: >.git/info/sparse-checkout
for p in "${SPARSE_PATHS[@]}"; do
    printf '%s\n' "$p" >>.git/info/sparse-checkout
done
# One commit deep: the 46 MB of test files is the whole download (33 MB of git data).
git fetch --depth 1 --no-tags origin "$PHP_SRC_SHA"
git checkout -q --detach "$PHP_SRC_SHA"

count=$(find . -name '*.phpt' | wc -l | tr -d ' ')
echo "modules/php-src: $count .phpt files at ${PHP_SRC_SHA:0:12} ($PHP_SRC_DESC)"
echo "Run the conformance suite with: ./run-all-tests.sh"
