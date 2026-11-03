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


def convert_rfc(text):
    """Convert the statement subset used by rfc-datetime_and_daylight_saving_time-*.phpt."""
    body = text.split("--FILE--", 1)[1].split("--EXPECT", 1)[0]
    body = re.sub(r"/\*.*?\*/", "", body, flags=re.S)
    body = body.replace("<?php", "").replace("?>", "")
    stmts = [x.strip() for x in body.split(";\n") if x.strip()]
    svars = {}
    ops = []

    def lit(piece):
        piece = piece.strip()
        if piece.startswith("'"):
            return piece[1:-1]
        if piece.startswith('"'):
            inner = piece[1:-1].replace("\\n", "\n")
            return re.sub(r"\$(\w+)", lambda m: svars[m.group(1)], inner)
        return None

    for st in stmts:
        st = " ".join(st.split())
        st = st.rstrip(";")
        m = re.match(r"date_default_timezone_set\('([^']+)'\)$", st)
        if m:
            ops.append("tz\t" + m.group(1))
            continue
        m = re.match(r"\$(\w+) = '([^']*)'$", st)
        if m:
            svars[m.group(1)] = m.group(2)
            continue
        m = re.match(r"\$interval = new DateInterval\(\$interval_spec\)$", st)
        if m:
            ops.append("iv\t" + svars["interval_spec"])
            continue
        m = re.match(r"\$(\w+) = new DateTime\('([^']*)'\)$", st)
        if m:
            ops.append("new\t%s\t%s" % m.groups())
            continue
        m = re.match(r"\$tz = new DateTimeZone\('([^']*)'\)$", st)
        if m:
            svars["tz"] = m.group(1)
            continue
        m = re.match(r"\$(\w+)->setTimeZone\(\$tz\)$", st)
        if m:
            ops.append("settz\t%s\t%s" % (m.group(1), svars["tz"]))
            continue
        m = re.match(r"echo (.*)$", st)
        if m:
            for piece in re.split(r" \. ", m.group(1)):
                l = lit(piece)
                if l is not None:
                    ops.append("echo\t" + l.replace("\n", "\\n"))
                    continue
                piece = piece.strip()
                fm = re.match(r"\$(\w+)->(?:(add|sub)\(\$interval\)->)?format\((.*)\)$", piece)
                if fm:
                    var, op, f = fm.groups()
                    if op:
                        ops.append("%s\t%s" % (op, var))
                    f = svars[f[1:]] if f.startswith("$") else f.strip("'")
                    ops.append("fmt\t%s\t%s" % (var, f))
                    continue
                dm = re.match(r"\$(\w+)->diff\(\$(\w+)\)->format\((.*)\)$", piece)
                if dm:
                    a, b, f = dm.groups()
                    f = svars[f[1:]] if f.startswith("$") else f.strip("'")
                    ops.append("diff\t%s\t%s\t%s" % (a, b, f))
                    continue
                raise SystemExit("unhandled echo piece %r" % piece)
            continue
        raise SystemExit("unhandled statement %r" % st)
    return ops


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
    for phpt in sorted(glob.glob(os.path.join(tests, "rfc-datetime_and_daylight_saving_time-type*.phpt"))):
        t = open(phpt, encoding="latin-1").read()
        out.append("#case\t%s\tSCRIPT" % os.path.basename(phpt)[:-5])
        out.extend(convert_rfc(t))
        out.append("#expect")
        out.extend(t.split("--EXPECT--", 1)[1].strip("\n").split("\n"))
        out.append("#end")
    with open(OUT, "w", encoding="latin-1", newline="\n") as f:
        f.write("\n".join(out) + "\n")
    print(sum(1 for l in out if l.startswith("#case")), "cases")


if __name__ == "__main__":
    main()
