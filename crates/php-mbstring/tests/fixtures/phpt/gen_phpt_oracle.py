#!/usr/bin/env python3
"""Record every mb_* call made by php-src's ext/mbstring .phpt tests.

Each test's FILE section is run under /usr/bin/php8.5 (with mbstring) with
`mb_xxx(` rewritten to logging wrappers (prelude.php). The log
(`calls.tsv`) holds, per test, a `#TEST name` line, `#INI key=value` lines
from the --INI-- section, then one line per call:
    func <TAB> args... <TAB> =result <TAB> diagnostics
(values encoded as in tests/fixtures/strings/gen_oracle.php; `A` arrays are
`key=value` pairs; diagnostics are level letter + hex message).
tests/phpt_replay.rs replays it.

Usage: python3 gen_phpt_oracle.py [path/to/php-src]
"""
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
SRC = Path(sys.argv[1]) if len(sys.argv) > 1 else HERE.parents[4] / "modules" / "php-src"
TESTS = SRC / "ext/mbstring/tests"
PHP = "/usr/bin/php8.5"
SKIP_FUNCS = re.compile(r"mb_(ereg|eregi|split|regex_set_options|parse_str|send_mail|output_handler)")


def sections(text):
    out, cur = {}, None
    for line in text.splitlines(keepends=True):
        m = re.match(r"^--([A-Z_]+)--\s*$", line)
        if m:
            cur = m.group(1)
            out[cur] = ""
        elif cur:
            out[cur] += line
    return out


def rewrite(code):
    def rep(m):
        name = m.group(1)
        if SKIP_FUNCS.match(name):
            return m.group(0)
        return f"__mbw_{name}("
    # Only bare calls (not method calls, not strings like 'mb_strlen').
    code = re.sub(r"(?<![\w$>:'\"\\])(mb_[a-z_0-9]+)\s*\(", rep, code)
    return re.sub(r"(?<![\w$>:'\"\\])ini_set\s*\(", "__mbw_ini_set(", code)


def main():
    out = HERE / "calls.tsv"
    prelude = (HERE / "prelude.php").read_text()
    with tempfile.TemporaryDirectory() as tmp, open(out, "w", encoding="latin-1") as fout:
        for phpt in sorted(TESTS.glob("*.phpt")):
            sec = sections(phpt.read_text(encoding="latin-1"))
            if "FILE" not in sec:
                continue
            code = sec["FILE"]
            if not re.search(r"\bmb_\w+\s*\(", code):
                continue
            code = re.sub(r"^\s*<\?php", "", rewrite(code), count=1)
            script = Path(tmp) / "t.php"
            script.write_text(prelude + "\n" + code, encoding="latin-1")
            log = Path(tmp) / "log.tsv"
            if log.exists():
                log.unlink()
            log.touch()
            args = [PHP, "-n", "-d", "extension=mbstring", "-d", "error_reporting=-1", "-d", "display_errors=0", "-d", "memory_limit=1G"]
            inis = []
            for ini in sec.get("INI", "").splitlines():
                if "=" in ini and ini.strip():
                    k, v = ini.split("=", 1)
                    k, v = k.strip(), v.strip()
                    if "memory_limit" in k:
                        continue
                    inis.append((k, v))
                    args += ["-d", f"{k}={v}"]
            env = dict(os.environ, MBW_LOG=str(log))
            try:
                subprocess.run(args + [str(script)], env=env, stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, timeout=60, cwd=tmp)
            except subprocess.TimeoutExpired:
                pass
            fout.write(f"#TEST\t{phpt.stem}\n")
            for k, v in inis:
                fout.write(f"#INI\t{k}\t{v.encode('latin-1').hex()}\n")
            for line in log.read_text(encoding="latin-1").splitlines(keepends=True):
                if len(line) > 20000:
                    # Huge (out-of-memory test) calls are not recorded.
                    fout.write("#BIG\t" + line.split("\t", 1)[0] + "\n")
                else:
                    fout.write(line)
    print(f"wrote {out}")


main()
