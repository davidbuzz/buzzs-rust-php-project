#!/usr/bin/env python3
"""Regenerate php-date's oracle tables (tests/data/*.tsv) using /usr/bin/php.

Usage (from the repository root):

    python3 -I crates/php-date/tools/gen_oracle.py [path/to/php-src/ext/date/tests]

Inputs are strings mined from php-src's ext/date/tests plus deterministic
random combinations of grammar fragments. Each command is evaluated by
tools/probe.php under the system PHP; the expected output is stored next to
the command. Cases where the system PHP (8.3, system tzdata) is known to
differ from php-src master (timelib 2026.02, bundled tzdb) are dropped:
whitespace-only strings, doubled signs, Morocco's DST flags, the sun
rise/set altitude change and DatePeriod's recurrence messages; those are
covered by hand-ported phpt expectations in tests/phpt_cases.rs instead.
"""
import glob
import os
import random
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(HERE, "..", "tests", "data")


def esc(s):
    return s.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n").replace("\0", "\\0")


def mine(tests_dir):
    strs = set()
    pat = re.compile(r"""(?:strtotime|date_parse|new DateTime(?:Immutable)?|date_create(?:_immutable)?|->modify)\s*\(\s*(['"])((?:\\.|(?!\1).)*)\1""")
    pat2 = re.compile(r"""date_modify\(\s*\$\w+\s*,\s*(['"])((?:\\.|(?!\1).)*)\1""")
    arrpat = re.compile(r"""^\s*(['"])([^'"\n]{1,60})\1\s*,?\s*$""", re.M)
    for fn in sorted(glob.glob(os.path.join(tests_dir, "*.phpt"))):
        t = open(fn, encoding="latin-1").read()
        for m in list(pat.finditer(t)) + list(pat2.finditer(t)):
            q, s = m.group(1), m.group(2)
            if q == "'":
                s = s.replace("\\'", "'").replace("\\\\", "\\")
            else:
                if "$" in s:
                    continue
                if "\\" in s:
                    s = s.encode("latin-1").decode("unicode_escape")
            strs.add(s)
        if "strtotime" in t or "date_parse" in t:
            for m in arrpat.finditer(t):
                strs.add(m.group(2))
    return sorted(s for s in strs if all(ord(c) < 256 for c in s))


TOKS = ["2008", "08", "12", "1", "31", "32", "2008-07-01", "07/01/2008", "1/2", "01.02.2008", "1.2.08", "July", "jul", "Sept", "IV", "XII",
        "monday", "Mon", "sat", "weekday", "weekdays", "next", "last", "this", "previous", "first", "third", "twelfth", "day", "days", "week",
        "weeks", "fortnight", "month", "months", "year", "hour", "hours", "min", "sec", "secs", "ms", "usec", "µs", "+1", "-2", "+ 3",
        "+1 week", "2 days", "3 hours ago", "ago", "now", "today", "tomorrow", "yesterday", "midnight", "noon", "back of 7pm", "front of 19",
        "10:00", "10:00:00", "10:00:00.5", "10.30", "1030", "103045", "t1030", "T10:30", "10am", "10 pm", "10:30 p.m.", "12am", "12pm",
        "10:30:45.123456", "UTC", "utc", "GMT", "CEST", "EST", "Europe/Amsterdam", "America/New_York", "+0200", "-05:00", "GMT+1", "+1",
        "(CET)", "Z", "z", "PST8PDT", "2008W05", "2008-W05-3", "2008W053", "2008.123", "2008-123", "@1234567890", "@-1", "@1.5",
        "first day of", "last day of", "first monday of", "last fri of", "2008-07-01T10:00:00Z", "2008-07-01T10:00:00.5+02:00",
        "20080701T10:00:00", "20080701t100000", "2008:07:01 10:00:00", "10/Oct/2000:13:55:36 -0700", "Jul 1", "1 Jul", "1st Jul",
        "Jul 1st 2008", "July 1, 2008", "1-Jul-2008", "Jul-01-2008", "2008-Jul-01", "Jul 2008", "2008 Jul", "20080701", ",", ".", "/",
        "-", ":", "x", "?", "é", "noonish", "1 jan 1970 00:00:00 gmt", "Sat, 01 Jan 2000 00:00:00 +0000", "tues", "thurs", "wednes",
        "+1 weekday", "3 weekdays ago", "next week", "last year", "this week", "monday next week", "sunday this week", "eighth day",
        "+2 mondays", "1/1/70", "12/31/69", "00:00", "24:00", "23:59:60", "25:00", "2008-02-30", "2008-13-01", "0000-00-00",
        "-0001-01-01", "+10000-01-01", "-12345-01-01", "Mar 1 2008 10am", "1 March 2008 10:30pm", "10:30pm Mar 1",
        "2008-03-01 10:30 CET", "2008-03-01 10:30 Europe/Paris"]

ZONES = ["UTC", "Europe/Amsterdam", "America/New_York", "Australia/Sydney", "Asia/Kolkata", "America/Sao_Paulo", "Pacific/Chatham",
         "Europe/London", "America/St_Johns", "Asia/Tehran", "Pacific/Apia", "America/Los_Angeles", "Europe/Moscow", "Asia/Kathmandu"]


def commands(tests_dir):
    rnd = random.Random(20261103)
    out = {}
    mined = mine(tests_dir)
    rand = []
    for _ in range(1500):
        n = rnd.choice([1, 1, 2, 2, 2, 3, 3, 4])
        sep = rnd.choice([" ", " ", " ", "", " ,", "\t"])
        rand.append(sep.join(rnd.choice(TOKS) for _ in range(n)))
    out["parse"] = ["parse\t" + esc(s) for s in mined + rand]
    st = []
    for s in mined:
        for tz, now in (("UTC", 1136073600), ("Europe/Amsterdam", 1162220400), ("America/New_York", 1210000000)):
            st.append("strtotime\t%s\t%d\t%s" % (tz, now, esc(s)))
    rel = ["+1 day", "+1 hour", "-1 hour", "+24 hours", "tomorrow", "midnight", "noon", "+1 week", "last day of next month",
           "first monday of next month", "next sunday", "yesterday 14:00", "+30 minutes", "2:30", "03:30", "+0 day 02:30"]
    for _ in range(1500):
        tz = rnd.choice(ZONES)
        base = rnd.choice([1162170000, 1174784400, 1206838800, 1225000000, 1301187600, 1319936400, 1446339600, 1457830800,
                           rnd.randint(0, 2 * 10**9)])
        st.append("strtotime\t%s\t%d\t%s" % (tz, base + rnd.randint(-7200, 7200), rnd.choice(rel)))
    for s in rand[:800]:
        st.append("strtotime\tEurope/Amsterdam\t1162220400\t" + esc(s))
    out["strtotime"] = st
    fmts = ["Y-m-d H:i:s T e P O Z I U", "D, d M Y H:i:s O", "l jS \\of F Y h:i:s A", "c", "r", "N w z W o t L y B g G h u v", "x X p",
            "\\Y\\-m"]
    out["date"] = ["date\t%s\t%d\t%s" % (rnd.choice(ZONES), rnd.choice([rnd.randint(-2**31, 2**31), rnd.randint(-10**11, 10**11),
                   rnd.randint(0, 4 * 10**9)]), esc(rnd.choice(fmts))) for _ in range(1500)]
    pf = ["Y-m-d", "Y-m-d H:i:s", "!Y-m-d", "Y-m-d|", "d/m/Y H:i", "D, d M Y H:i:s O", "l jS F Y", "U", "U.u", "Y-m-d H:i:s.u",
          "Y-m-d H:i:s.v", "g:i a", "h:i A", "G:i", "z Y", "Y z", "Y-m-d e", "Y-m-d T", "Y-m-d P", "Y-m-d+", "Y-m-d *", "Y#m#d", "Y?m?d",
          "\\Y Y", "y-n-j", "X-m-d", "x-m-d", "H\\h i\\m", "Y-m-d \\T H:i", "!d", "!H", "|H", "Y-m-!d", " Y-m-d", "Y-m-d S", "jS F",
          "M Y", "F Y", "D", "l", "a", "A h"]
    pv = ["2009-02-15", "2009-02-15 15:16:17", "15/02/2009 15:16", "Sun, 15 Feb 2009 15:16:17 +0100", "Sunday 15th February 2009",
          "1234567890", "1234567890.5", "1234567890.123456", "2009-02-15 15:16:17.123456", "2009-02-15 15:16:17.123", "3:05 pm",
          "03:05 PM", "15:05", "045 2009", "2009 045", "2009-02-15 Europe/Amsterdam", "2009-02-15 CET", "2009-02-15 +02:00",
          "2009-02-15 extra", "2009/02/15", "2009x02y15", "Y 2009", "09-2-5", "+12345-02-15", "2009-02-15 T 10:30", "10h 30m", "15",
          "10", "2009-02-30", "2009-13-15", "25:61:61", "x", "", "15th Feb", "Feb 2009", "February 2009", "Mon", "Monday", "am",
          "AM 10", "12 am", "-0012-01-01"]
    out["pff"] = ["pff\t%s\t%s" % (esc(f), esc(rnd.choice(pv))) for f in pf for _ in range(25)]
    dates = ["2006-10-29 01:30:00", "2006-10-29 02:30:00", "2006-10-29 03:30:00", "2007-03-11 02:30:00", "2007-03-11 01:59:59",
             "2007-11-04 01:30:00", "2010-03-28 02:30", "2010-10-31 02:30", "2010-01-31", "2010-02-28 23:59:59", "2008-02-29",
             "2000-01-01 00:00:00", "1970-01-01", "1900-06-15 12:00", "2038-01-19 03:14:08", "2100-07-01", "2020-12-31 23:59:59.999999",
             "2011-12-30", "2011-12-31", "1969-12-31 23:59:59", "2009-06-15 12:00 CEST", "2009-06-15 12:00 +05:30", "2009-06-15 12:00 EST",
             "2009-06-15T12:00:00Z", "@1234567890", "2016-11-06 01:30 EDT", "2016-11-06 01:30 EST"]
    mods = ["+1 day", "-1 day", "+1 hour", "-1 hour", "+24 hours", "+1 month", "-1 month", "+1 year", "last day of this month",
            "first day of next month", "last day of previous month", "next monday", "last friday", "monday this week", "sunday last week",
            "+1 weekday", "+5 weekdays", "-3 weekdays", "midnight", "noon", "tomorrow", "yesterday", "first sat of next month",
            "last sun of march 2010", "+30 minutes", "-90 minutes", "+1 week 2 days 4 hours 2 seconds", "@0", "2009-01-01", "10:00",
            "+0 day", "this week", "next week", "last week", "first day of", "last day of", "saturday", "+2 sundays ago", "back of 7pm",
            "front of 10am", "+1 month -1 day", "Europe/Paris", "CET", "+02:00"]
    fmt = "Y-m-d H:i:s.u T P U"
    obj = []
    for _ in range(1500):
        obj.append("modify\t%s\t%s\t%s\t%s" % (rnd.choice(ZONES), rnd.choice(dates), rnd.choice(mods), fmt))
    for _ in range(400):
        obj.append("create\t%s\t%s\t%s" % (rnd.choice(ZONES), rnd.choice(dates), fmt))
    for _ in range(1500):
        obj.append("diff\t%s\t%s\t%s\t%%R %%y %%m %%d %%h %%i %%s %%f %%a" % (rnd.choice(ZONES), rnd.choice(dates), rnd.choice(dates)))
    out["objects"] = obj
    ivs = ["P1D", "P1M", "P1Y", "PT1H", "PT36H", "P1Y2M3DT4H5M6S", "P2W", "P0D", "PT0S", "P1W3D", "P0001-02-03T04:05:06",
           "2008-03-01T13:00:00Z/2008-05-11T15:30:00Z", "PT", "P", "P1", "XYZ", "P1Q", "p1d", "P1DT", "PT1.5S", "P-1D", "P1Y1Y", "",
           "R5/P1D", "P10000Y", "PT24H", "PT1440M", "P1M1W"]
    dss = ["1 day", "2 weeks", "+1 month -1 day", "last day of next month", "next weekday", "3 weekdays", "1 year + 1 day", "1 hour ago",
           "yesterday", "2008-01-01", "+5 seconds 300 msec", "first monday of next month", "weekday"]
    adates = ["2006-10-29 01:30:00", "2007-03-11 01:30:00", "2010-01-31", "2008-02-29", "2010-03-27 02:30", "2010-10-30 02:30",
              "2000-01-01 00:00:00.5", "2011-01-01 00:00:00 CET"]
    f = "%y %Y %m %M %d %D %h %H %i %I %s %S %f %F %a %R %r %%"
    misc = ["interval\t%s\t%s" % (esc(i), f) for i in ivs] + ["datestr\t%s\t%s" % (d, f) for d in dss]
    for _ in range(1200):
        misc.append("%s\t%s\t%s\t%s\tY-m-d H:i:s.u T U" % (rnd.choice(["add", "sub"]), rnd.choice(ZONES), rnd.choice(adates),
                                                         rnd.choice(ivs[:11] + dss)))
    pers = ["R4/2012-07-01T00:00:00Z/P7D", "2012-07-01T00:00:00Z/P7D/2012-08-01T00:00:00Z", "R2/2012-07-01T00:00:00Z/P1M",
            "R3/20120701T000000Z/PT6H", "R1/2012-01-31T00:00:00Z/P1M", "garbage"]
    for p in pers:
        for o in (0, 1, 2, 3):
            misc.append("period\t%s\t%d\tY-m-d H:i:s e" % (p, o))
    for a in ["CET", "cest", "EST", "edt", "pst", "utc", "GMT", "BST", "IST", "foo", "", "AEST", "acdt", "z", "a", "MSK", "NZDT", "SAST"]:
        for off, dst in ((-1, -1), (3600, 0), (3600, 1), (-18000, 0), (-14400, 1), (0, 0), (19800, 0), (7200, -1)):
            misc.append("abbr\t%s\t%d\t%d" % (a, off, dst))
    for z in ["UTC", "utc", "Europe/Amsterdam", "europe/amsterdam", "CET", "cest", "+01:00", "-05:30", "+0530", "GMT+1", "+1", "-12345",
              "+999999", "+100:00", "Foo", "Foo/Bar", "EST5EDT", "Etc/GMT+5", "(CET)", "Z", "z", "America/Argentina/Buenos_Aires",
              "America/Indiana/Indianapolis", "Europe/London ", "GMT", "+14:00", "-14:00", "+25:00", "UTC+1", "AbC"]:
        misc.append("tzinit\t%s" % z)
    out["misc"] = misc
    return out


SKIP = [
    re.compile(r"^(parse|strtotime(\t[^\t]*){2})\t(\\t| |\\n)*$"),  # whitespace-only (timelib 2026 reports "Empty string")
    re.compile(r"[+-][ \\t]*-|-[ \\t]*\+|--|\+\+"),  # doubled signs (sign toggling changed)
    re.compile(r"Casablanca"),
    re.compile(r"^period\t(R0/|[^R])"),
]


def main():
    tests_dir = sys.argv[1] if len(sys.argv) > 1 else "modules/php-src/ext/date/tests"
    os.makedirs(DATA, exist_ok=True)
    for name, cmds in commands(tests_dir).items():
        cmds = [c for c in cmds if not any(p.search(c) for p in SKIP)]
        inp = "\n".join(cmds) + "\n"
        res = subprocess.run(["php", "-n", "-d", "date.timezone=UTC", os.path.join(HERE, "probe.php")], input=inp.encode("latin-1"),
                             capture_output=True, check=True).stdout.decode("latin-1").split("\n")
        with open(os.path.join(DATA, name + ".tsv"), "w", encoding="latin-1", newline="\n") as f:
            for c, r in zip(cmds, res):
                f.write("%s\t=>\t%s\n" % (c, esc(r)))
        print(name, len(cmds))


if __name__ == "__main__":
    main()
