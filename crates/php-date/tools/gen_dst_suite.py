#!/usr/bin/env python3
"""Convert php-src's DateTime diff/add/sub/days suite (DateTime_data-*.inc +
the DateTime_*-*.phpt files that include them) into tests/data/dst_suite.txt.

Usage: python3 -I crates/php-date/tools/gen_dst_suite.py modules/php-src/ext/date/tests
"""
import glob
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "..", "tests", "data", "dst_suite.txt")


def convert_inc(path):
    lines = []
    for raw in open(path, encoding="latin-1"):
        line = raw.split("//")[0].strip() if "examine_diff" not in raw else raw.strip()
        if not line or line.startswith(("<?php", "/*", "*", "*/", "?>")):
            continue
        m = re.match(r"date_default_timezone_set\('([^']+)'\);", line)
        if m:
            lines.append("tz\t" + m.group(1))
            continue
        m = re.match(r'echo "([^"]*)";', line)
        if m:
            lines.append("echo\t" + m.group(1))
            continue
        m = re.match(r"\$(\w+)\s*=\s*new DateTime\('([^']*)'\);", line)
        if m:
            lines.append("new\t%s\t%s" % m.groups())
            continue
        m = re.match(r"\$(\w+)\s*=\s*new DateTime;", line)
        if m:
            lines.append("new\t%s\t" % m.group(1))
            continue
        m = re.match(r"\$(\w+)->setTimezone\(new DateTimeZone\('([^']+)'\)\);", line)
        if m:
            lines.append("settz\t%s\t%s" % m.groups())
            continue
        m = re.match(r"\$(\w+)->setDate\((-?\d+),\s*(\d+),\s*(\d+)\);", line)
        if m:
            lines.append("setdate\t%s\t%s\t%s\t%s" % m.groups())
            continue
        m = re.match(r"\$(\w+)->setTime\((\d+),\s*(\d+),\s*(\d+)\);", line)
        if m:
            lines.append("settime\t%s\t%s\t%s\t%s" % m.groups())
            continue
        m = re.match(r"examine_diff\((.*)\);", line)
        if m:
            args = [a.strip() for a in re.findall(r"\$\w+|'[^']*'|true|false|-?\d+", m.group(1))]
            conv = []
            for a in args:
                if a.startswith("$"):
                    conv.append("$" + a[1:])
                elif a.startswith("'"):
                    conv.append(a[1:-1])
                else:
                    conv.append(a)
            if len(conv) == 4:
                conv.append("false")
            lines.append("examine\t" + "\t".join(conv))
            continue
        if line in ("}",):
            continue
        raise SystemExit("unhandled line in %s: %r" % (path, line))
    return lines


def main():
    tests = sys.argv[1] if len(sys.argv) > 1 else "modules/php-src/ext/date/tests"
    out = []
    modes = {"PHPT_DATETIME_SHOW_DIFF": "DIFF", "PHPT_DATETIME_SHOW_DAYS": "DAYS", "PHPT_DATETIME_SHOW_ADD": "ADD",
             "PHPT_DATETIME_SHOW_SUB": "SUB"}
    for phpt in sorted(glob.glob(os.path.join(tests, "*.phpt"))):
        t = open(phpt, encoding="latin-1").read()
        m = re.search(r"define\('PHPT_DATETIME_SHOW', (\w+)\);\s*require '(DateTime_data-[^']+)';", t)
        if not m:
            continue
        mode = modes[m.group(1)]
        script = convert_inc(os.path.join(tests, m.group(2)))
        kind = "#expectf" if "--EXPECTF--" in t else "#expect"
        expect = t.split("--EXPECTF--" if kind == "#expectf" else "--EXPECT--", 1)[1].strip("\n")
        out.append("#case\t%s\t%s" % (os.path.basename(phpt)[:-5], mode))
        out.extend(script)
        out.append(kind)
        out.extend(expect.split("\n"))
        out.append("#end")
    with open(OUT, "w", encoding="latin-1", newline="\n") as f:
        f.write("\n".join(out) + "\n")
    print(sum(1 for l in out if l.startswith("#case")), "cases")


if __name__ == "__main__":
    main()
