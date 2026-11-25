#!/usr/bin/env python3
"""Differential harness for the command-line SAPI and INI handling.

Runs each case through our `php` and through the C implementation and
compares stdout, stderr and the exit status after light normalisation
(binary paths, temp dirs, version strings).  Standard library only.

    python3 clidiff.py [--ours PATH] [--ref PATH] [--filter SUBSTR] [-v]

Cases live in cases.py beside this file.  Each case is a dict:
    name   unique name
    args   argv after the binary (list of str); "{d}" expands to the
           case's temp directory
    stdin  bytes/str fed on standard input (default: empty)
    env    extra environment variables
    files  {relative path: content} written into the temp directory
    nomem  True: do not add `-d memory_limit=1G` for the oracle (cases
           about -n/-c/memory_limit; keep those scripts trivial)
    cwd    run in the temp dir (default True)
Every invocation is pinned to one core with `taskset -c 12` and has a
5 second timeout.
"""
import argparse
import os
import re
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import cases as case_list  # noqa: E402

OURS = "/tmp/409923/scratchpad/agent-bin/php"
REF = "/usr/bin/php8.5"


def run(binary, case, tmp, add_mem):
    args = [a.replace("{d}", tmp) for a in case.get("args", [])]
    pre = []
    if add_mem and not case.get("nomem"):
        pre = ["-d", "memory_limit=1G"]
    # Options must precede the script; insert ours at the front.
    argv = ["taskset", "-c", "12", binary] + pre + args
    env = dict(os.environ)
    env.pop("LANGUAGE", None)
    env.pop("PHPRC", None)
    env.pop("PHP_INI_SCAN_DIR", None)
    for k, v in case.get("env", {}).items():
        env[k] = v.replace("{d}", tmp)
    stdin = case.get("stdin", b"")
    if isinstance(stdin, str):
        stdin = stdin.encode()
    try:
        p = subprocess.run(argv, input=stdin, capture_output=True, env=env,
                           cwd=tmp if case.get("cwd", True) else None,
                           timeout=5)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return "timeout", b"", b""


def norm(data, binary, tmp):
    s = data.decode("utf-8", "replace")
    s = s.replace(os.path.realpath(tmp), "{d}").replace(tmp, "{d}")
    s = s.replace(binary, "{php}")
    s = re.sub(r"PHP \d+\.\d+\.\d+[^ \n]*", "PHP X", s)
    s = re.sub(r"\(built: [^)]*\)", "(built)", s)
    s = re.sub(r"Zend Engine v[\d.]+", "Zend Engine vX", s)
    s = re.sub(r"Copyright \(c\) [^\n]*", "Copyright", s)
    s = re.sub(r"with Zend OPcache[^\n]*\n?", "", s)
    # php-src master no longer names the path in stream open warnings.
    s = re.sub(r"(\w+)\([^)\n]*\): Failed to open stream", r"\1(): Failed to open stream", s)
    # -v: the Ubuntu build line and master's copyright wording.
    s = s.replace("Built by Ubuntu\n", "")
    s = s.replace("Copyright © The PHP Group and Contributors", "Copyright")
    s = re.sub(r"Zend Engine vX(-dev)?, Copyright[^\n]*(\n    )?", "Zend Engine vX, Copyright", s)
    # Build-time paths: PHP_CONFIG_FILE_PATH and the extension API number.
    s = s.replace('"/etc/php/8.5/cli"', '"/usr/local/lib"')
    s = re.sub(r"/usr/lib/php/\d{8}", "/usr/lib/php/API", s)
    # The usage text names basename(argv[0]).
    s = re.sub(r"(?m)(^Usage: |^   )php8\.5 ", r"\1php ", s)
    # Usage lines master added after 8.5 (master is the specification).
    s = s.replace("  -C               Do not chdir to the script's directory"
                  " (no-op in CLI)\n", "")
    s = s.replace("  -q               Do not output HTTP headers"
                  " (no-op in CLI)\n", "")
    return s


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ours", default=OURS)
    ap.add_argument("--ref", default=REF)
    ap.add_argument("--filter", action="append", default=[])
    ap.add_argument("-v", action="store_true", help="print diffs")
    o = ap.parse_args()
    fails = 0
    total = 0
    for case in case_list.CASES:
        if o.filter and not any(f in case["name"] for f in o.filter):
            continue
        total += 1
        res = []
        for binary, add_mem in ((o.ours, True), (o.ref, True)):
            with tempfile.TemporaryDirectory(prefix="clid") as tmp:
                for rel, content in case.get("files", {}).items():
                    path = os.path.join(tmp, rel)
                    os.makedirs(os.path.dirname(path), exist_ok=True)
                    if isinstance(content, str):
                        content = content.replace("{d}", tmp).encode()
                    with open(path, "wb") as f:
                        f.write(content)
                code, out, err = run(binary, case, tmp, add_mem)
                res.append((code, norm(out, binary, tmp),
                            norm(err, binary, tmp)))
        if res[0] != res[1]:
            fails += 1
            print(f"DIFF {case['name']}")
            if o.v:
                for label, i in (("status", 0), ("stdout", 1), ("stderr", 2)):
                    if res[0][i] != res[1][i]:
                        print(f"  {label} ours: {res[0][i]!r}")
                        print(f"  {label} ref : {res[1][i]!r}")
    print(f"{total - fails}/{total} cases match")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
