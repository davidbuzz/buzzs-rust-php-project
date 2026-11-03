#!/usr/bin/env python3
"""Rewrite the % column of FEATURE_GAP.md from a `phpt-runner --json` report.

Usage:
    target/debug/phpt-runner --json results.json modules/php-src/Zend/tests modules/php-src/tests ...
    scripts/update_feature_gap.py results.json [--feature-gap FEATURE_GAP.md] [--dry-run]
                                               [--denominator all|ran] [--min-tests N]
                                               [--scope FILE | --no-scope]
    scripts/update_feature_gap.py results.json --summary     # headline numbers, no rewrite

Each row name below maps to one or more path prefixes (relative to the php-src root). A row is
only rewritten when the report contains at least --min-tests tests under its prefixes, so a
partial run leaves the other rows alone. Rows without a mapping are never touched.

The percentage is passed / total by default (skipped tests count as not passing, since in this
project a skip usually means a missing extension or SAPI); `--denominator ran` excludes skipped
and borked tests instead.

Out-of-scope tests (status "OUT" in the report, or under a prefix of the scope file,
default `phpt-scope.txt` at the repository root) are not counted, and rows whose prefixes are
all out of scope are left untouched.

Python 3 standard library only.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# Row name in FEATURE_GAP.md -> list of path prefixes (directories end with "/").
MAPPING: dict[str, list[str]] = {
    # Language front end / compiler.
    "Parser and AST": ["Zend/tests/grammar/", "Zend/tests/ast/"],
    "Compile-time diagnostics": ["Zend/tests/errmsg/"],
    # Runtime core.
    "Type juggling and comparison": [
        "Zend/tests/comparison/",
        "Zend/tests/numeric_strings/",
        "Zend/tests/type_coercion/",
    ],
    "Error and warning system": [
        "Zend/tests/error_handler/",
        "Zend/tests/error_reporting/",
        "Zend/tests/error_suppression/",
    ],
    "Exceptions": ["Zend/tests/exceptions/", "Zend/tests/try/", "Zend/tests/throw/"],
    "Garbage collection": ["Zend/tests/gc/", "Zend/tests/weakrefs/"],
    "Memory limit and resource limits": ["Zend/tests/stack_limit/"],
    "INI system": ["Zend/tests/zend_ini/"],
    # Language features.
    "Functions and closures": [
        "Zend/tests/closures/",
        "Zend/tests/arrow_functions/",
        "Zend/tests/first_class_callable/",
        "Zend/tests/functions/",
        "Zend/tests/function_arguments/",
        "Zend/tests/named_params/",
        "Zend/tests/variadic/",
        "Zend/tests/static_variables/",
    ],
    "Classes and objects": [
        "tests/classes/",
        "Zend/tests/objects/",
        "Zend/tests/clone/",
        "Zend/tests/magic_methods/",
        "Zend/tests/readonly_props/",
        "Zend/tests/readonly_classes/",
        "Zend/tests/ctor_promotion/",
        "Zend/tests/access_modifiers/",
    ],
    "Inheritance, interfaces, traits": [
        "Zend/tests/inheritance/",
        "Zend/tests/interfaces/",
        "Zend/tests/traits/",
        "Zend/tests/lsb/",
    ],
    "Type system": [
        "Zend/tests/type_declarations/",
        "Zend/tests/return_types/",
        "Zend/tests/nullable_types/",
        "Zend/tests/object_types/",
    ],
    "Namespaces and `use`": [
        "Zend/tests/namespaces/",
        "Zend/tests/group_use/",
        "Zend/tests/use_const/",
        "Zend/tests/use_function/",
    ],
    "Autoloading": ["Zend/tests/autoload/"],
    "Enums": ["Zend/tests/enum/"],
    "Generators": ["Zend/tests/generators/"],
    "Fibers": ["Zend/tests/fibers/"],
    "Attributes": ["Zend/tests/attributes/"],
    "`match`, nullsafe, spread, list/destructuring": [
        "Zend/tests/match/",
        "Zend/tests/nullsafe_operator/",
        "Zend/tests/list/",
        "Zend/tests/array_unpack/",
        "Zend/tests/arg_unpack/",
    ],
    "Property hooks and asymmetric visibility": [
        "Zend/tests/property_hooks/",
        "Zend/tests/asymmetric_visibility/",
    ],
    "Lazy objects": ["Zend/tests/lazy_objects/"],
    "Built-in iterators and interfaces": ["Zend/tests/iterators/", "Zend/tests/ArrayAccess/"],
    "`include`/`require`, `eval`, `goto`, `declare`": [
        "Zend/tests/jump/",
        "Zend/tests/declare/",
    ],
    "Zend built-in functions": [
        "Zend/tests/builtin_functions/",
        "Zend/tests/class_exists/",
        "Zend/tests/get_class_methods/",
        "Zend/tests/get_class_vars/",
        "Zend/tests/get_object_vars/",
        "Zend/tests/is_callable/",
    ],
    "Zend test suite overall": ["Zend/tests/"],
    "PHP language test suite overall": [
        "tests/lang/",
        "tests/basic/",
        "tests/classes/",
        "tests/output/",
        "tests/func/",
        "tests/strings/",
        "tests/security/",
    ],
    # main/.
    "Output buffering": ["tests/output/"],
    "Streams layer": ["Zend/tests/streams/", "ext/standard/tests/streams/"],
    "Network streams / sockets transport": ["ext/standard/tests/network/"],
    "Superglobals and request variables": ["tests/basic/"],
    # SAPIs.
    "CLI (`php` binary)": ["sapi/cli/tests/"],
    "CGI": ["sapi/cgi/tests/"],
    "FPM": ["sapi/fpm/tests/"],
    "phpdbg": ["sapi/phpdbg/tests/"],
    # Extensions.
    "ext/standard": ["ext/standard/tests/"],
    "ext/spl": ["ext/spl/tests/"],
    "ext/pcre": ["ext/pcre/tests/"],
    "ext/date": ["ext/date/tests/"],
    "ext/reflection": ["ext/reflection/tests/"],
    "ext/json": ["ext/json/tests/"],
    "ext/random": ["ext/random/tests/"],
    "ext/hash": ["ext/hash/tests/"],
    "ext/ctype": ["ext/ctype/tests/"],
    "ext/tokenizer": ["ext/tokenizer/tests/"],
    "ext/uri": ["ext/uri/tests/"],
    "ext/mbstring": ["ext/mbstring/tests/"],
    "ext/iconv": ["ext/iconv/tests/"],
    "ext/filter": ["ext/filter/tests/"],
    "ext/session": ["ext/session/tests/"],
    "ext/bcmath": ["ext/bcmath/tests/"],
    "ext/gmp": ["ext/gmp/tests/"],
    "ext/zlib": ["ext/zlib/tests/"],
    "ext/bz2": ["ext/bz2/tests/"],
    "ext/zip": ["ext/zip/tests/"],
    "ext/phar": ["ext/phar/tests/"],
    "ext/openssl": ["ext/openssl/tests/"],
    "ext/sodium": ["ext/sodium/tests/"],
    "ext/curl": ["ext/curl/tests/"],
    "ext/ftp": ["ext/ftp/tests/"],
    "ext/sockets": ["ext/sockets/tests/"],
    "ext/fileinfo": ["ext/fileinfo/tests/"],
    "ext/exif": ["ext/exif/tests/"],
    "ext/calendar": ["ext/calendar/tests/"],
    "ext/gettext": ["ext/gettext/tests/"],
    "ext/intl": ["ext/intl/tests/"],
    "ext/gd": ["ext/gd/tests/"],
    "ext/libxml": ["ext/libxml/tests/"],
    "ext/dom": ["ext/dom/tests/"],
    "ext/simplexml": ["ext/simplexml/tests/"],
    "ext/xml": ["ext/xml/tests/"],
    "ext/xmlreader / ext/xmlwriter": ["ext/xmlreader/tests/", "ext/xmlwriter/tests/"],
    "ext/xsl": ["ext/xsl/tests/"],
    "ext/soap": ["ext/soap/tests/"],
    "ext/pdo (core)": ["ext/pdo/tests/"],
    "ext/pdo_sqlite / ext/sqlite3": ["ext/pdo_sqlite/tests/", "ext/sqlite3/tests/"],
    "ext/posix": ["ext/posix/tests/"],
    "ext/pcntl": ["ext/pcntl/tests/"],
    "ext/readline": ["ext/readline/tests/"],
    "ext/opcache": ["ext/opcache/tests/"],
    "ext/ffi": ["ext/ffi/tests/"],
}


REPO_ROOT = Path(__file__).resolve().parent.parent

# Headline groups for --summary: (label, prefixes).
SUMMARY_GROUPS: list[tuple[str, list[str]]] = [
    ("Zend/tests", ["Zend/tests/"]),
    ("tests/", ["tests/"]),
    ("ext/", ["ext/"]),
    ("sapi/", ["sapi/"]),
]


def load_scope(path: Path | None) -> list[str]:
    """Out-of-scope prefixes from a scope file (one per line, '#' comments)."""
    if path is None or not path.is_file():
        return []
    prefixes = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.split("#", 1)[0].strip()
        if line:
            prefixes.append(line.removeprefix("./").rstrip("/"))
    return prefixes


def out_of_scope(path: str, scope: list[str]) -> bool:
    return any(path == p or path.startswith(p + "/") for p in scope)


def in_scope_tests(tests: list[dict], scope: list[str]) -> list[dict]:
    return [
        t for t in tests if t.get("result") != "OUT" and not out_of_scope(t.get("path", ""), scope)
    ]


def load_results(path: Path) -> list[dict]:
    with path.open(encoding="utf-8") as fh:
        report = json.load(fh)
    tests = report.get("tests")
    if not isinstance(tests, list):
        raise SystemExit(f"{path}: not a phpt-runner --json report (no 'tests' list)")
    return tests


def counts(tests: list[dict], prefixes: list[str]) -> dict[str, int]:
    c = {k: 0 for k in ("total", "PASS", "WARN", "FAIL", "SKIP", "XFAIL", "BORK")}
    for t in tests:
        if any(t.get("path", "").startswith(p) for p in prefixes):
            c["total"] += 1
            c[t.get("result", "FAIL")] = c.get(t.get("result", "FAIL"), 0) + 1
    return c


def print_summary(tests: list[dict], scope_count: int) -> None:
    def pct(n: int, d: int) -> str:
        return f"{100.0 * n / d:.1f}%" if d else "-"

    print("| Suite | total | pass | fail | skip | xfail | bork | pass % (ran) | pass % (all) |")
    print("|---|---|---|---|---|---|---|---|---|")
    groups = [g for g in SUMMARY_GROUPS if counts(tests, g[1])["total"]]
    for label, prefixes in groups + [("**in scope, total**", [""])]:
        c = counts(tests, prefixes)
        passed = c["PASS"] + c["WARN"]
        ran = c["total"] - c["SKIP"] - c["BORK"]
        print(
            f"| {label} | {c['total']} | {passed} | {c['FAIL']} | {c['SKIP']} | {c['XFAIL']} "
            f"| {c['BORK']} | {pct(passed, ran)} | {pct(passed, c['total'])} |"
        )
    print(f"\n{scope_count} out-of-scope test(s) not counted.")


def row_stats(tests: list[dict], prefixes: list[str]) -> tuple[int, int, int]:
    """Return (passed, total, ran) over tests whose path starts with any prefix."""
    passed = total = ran = 0
    for t in tests:
        path = t.get("path", "")
        if not any(path.startswith(p) for p in prefixes):
            continue
        result = t.get("result")
        total += 1
        if result not in ("SKIP", "BORK"):
            ran += 1
        if result in ("PASS", "WARN"):
            passed += 1
    return passed, total, ran


def format_percent(passed: int, denominator: int) -> str:
    if denominator == 0:
        return "0%"
    pct = 100.0 * passed / denominator
    if passed == denominator:
        return "100%"
    if pct < 10 and round(pct, 1) != round(pct):
        return f"{pct:.1f}%"
    # Never round a partial pass rate up to 100%, or a non-zero one down to 0%.
    return f"{min(99, max(1, round(pct))) if passed else 0}%"


def split_row(line: str) -> list[str] | None:
    stripped = line.strip()
    if not (stripped.startswith("|") and stripped.endswith("|")):
        return None
    return stripped[1:-1].split("|")


def rewrite(text: str, percents: dict[str, str]) -> tuple[str, list[tuple[str, str, str]]]:
    changes = []
    out = []
    for line in text.splitlines(keepends=True):
        cells = split_row(line)
        if cells and len(cells) >= 2:
            name = cells[0].strip()
            if name in percents:
                old = cells[1].strip()
                new = percents[name]
                if old != new:
                    cells[1] = f" {new} "
                    newline = "\n" if line.endswith("\n") else ""
                    line = "|" + "|".join(cells) + "|" + newline
                    changes.append((name, old, new))
        out.append(line)
    return "".join(out), changes


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("report", type=Path, help="phpt-runner --json output")
    parser.add_argument(
        "--feature-gap",
        type=Path,
        default=Path(__file__).resolve().parent.parent / "FEATURE_GAP.md",
        help="FEATURE_GAP.md to rewrite (default: repository root)",
    )
    parser.add_argument("--dry-run", action="store_true", help="print changes without writing")
    parser.add_argument(
        "--denominator",
        choices=("all", "ran"),
        default="all",
        help="'all': passed/total (default); 'ran': passed/(total - skipped - borked)",
    )
    parser.add_argument(
        "--min-tests", type=int, default=1, help="minimum tests in the report for a row to be updated"
    )
    parser.add_argument("--only", action="append", default=[], help="only update these row names")
    parser.add_argument(
        "--scope",
        type=Path,
        default=REPO_ROOT / "phpt-scope.txt",
        help="scope file of out-of-scope prefixes (default: phpt-scope.txt at the repository root)",
    )
    parser.add_argument("--no-scope", action="store_true", help="ignore the scope file")
    parser.add_argument(
        "--summary",
        action="store_true",
        help="print the in-scope headline numbers (markdown table) and exit without rewriting",
    )
    args = parser.parse_args(argv)

    all_tests = load_results(args.report)
    scope = [] if args.no_scope else load_scope(args.scope)
    tests = in_scope_tests(all_tests, scope)
    if args.summary:
        print_summary(tests, len(all_tests) - len(tests))
        return 0
    percents: dict[str, str] = {}
    for row, prefixes in MAPPING.items():
        if args.only and row not in args.only:
            continue
        if scope and all(out_of_scope(p.rstrip("/"), scope) for p in prefixes):
            continue
        passed, total, ran = row_stats(tests, prefixes)
        if total < max(1, args.min_tests):
            continue
        denominator = total if args.denominator == "all" else ran
        percents[row] = format_percent(passed, denominator)
        print(f"{row}: {passed}/{denominator} -> {percents[row]}")

    text = args.feature_gap.read_text(encoding="utf-8")
    new_text, changes = rewrite(text, percents)
    missing = sorted(set(percents) - {c[0] for c in changes} - {
        r for r in percents if f"| {r} |" in text
    })
    for name in missing:
        print(f"warning: row '{name}' not found in {args.feature_gap}", file=sys.stderr)
    for name, old, new in changes:
        print(f"  {name}: {old} -> {new}")
    if not changes:
        print("No changes.")
    elif not args.dry_run:
        args.feature_gap.write_text(new_text, encoding="utf-8")
        print(f"Updated {len(changes)} row(s) in {args.feature_gap}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
