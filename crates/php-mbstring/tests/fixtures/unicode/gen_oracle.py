#!/usr/bin/env python3
"""Regenerate the oracle fixtures in this directory with /usr/bin/php8.5.

* `phpt.tsv`: every mb_convert_encoding / mb_check_encoding / mb_strcut call
  made by the php-src .phpt tests listed below (run against the real PHP,
  with the calls rewritten to logging wrappers from prelude.php).
* `fuzz.tsv`: deterministic randomised conversions from fuzz.php.

Line formats (tab separated, byte strings hex encoded):
  conv  FROM TO SUBST INPUT OUTPUT     SUBST: MODE:CHAR, MODE one of
  char|none|long|entity and CHAR the substitute code point
  check ENC INPUT 0|1
  cut   ENC START LENGTH|null INPUT OUTPUT

The oracle is PHP 8.3, whose codecs for these encodings match php-src master.
`cut` lines reflect 8.3's mb_strcut: master differs for UTF-16 (fast `cut`
functions) and returns "" for a zero length, so the replay test only checks
the legacy-filter encodings and emulates 8.3's argument handling.

Usage: python3 gen_oracle.py [path/to/php-src]
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

PHPTS = """
utf_encodings ucs2_encoding ucs4_encoding utf7imap_encoding base64_encoding
qprint_encoding uuencode_encoding htmlent_encoding htmlent gh10192_utf7
gh10192_utf7imap mb_check_encoding mb_strcut mb_convert_encoding
mb_convert_encoding_basic mb_convert_encoding_stateful bug30549 bug43840
bug45722 bug46843 bug48645 bug49528 bug54494 bug65045 bug68846 bug69086
bug71298 bug71606 bug76958 bug79371 bug79441 bug81298 gh7902 gh8086 gh9008
gh9535 gh17503
""".split()

REWRITES = [
    (re.compile(r"\bmb_convert_encoding\s*\("), "__L_conv("),
    (re.compile(r"\bmb_check_encoding\s*\("), "__L_check("),
    (re.compile(r"\bmb_strcut\s*\("), "__L_cut("),
    (re.compile(r"\bmb_substitute_character\s*\("), "__L_subst("),
]


def rewrite(code: str) -> str:
    for rx, rep in REWRITES:
        code = rx.sub(rep, code)
    return code


def sections(text: str) -> dict:
    out, cur = {}, None
    for line in text.splitlines(keepends=True):
        m = re.match(r"^--([A-Z_]+)--\s*$", line)
        if m:
            cur = m.group(1)
            out[cur] = ""
        elif cur:
            out[cur] += line
    return out


def run_phpt(name: str, workdir: Path, log: Path) -> None:
    sec = sections((TESTS / f"{name}.phpt").read_text(encoding="latin-1"))
    code = rewrite(sec["FILE"])
    script = workdir / f"{name}.php"
    prelude = (HERE / "prelude.php").read_text()
    # Prepend the prelude without disturbing line structure of the test.
    code = re.sub(r"^\s*<\?php", "", code, count=1)
    script.write_text(prelude + "\n" + code, encoding="latin-1")
    args = [PHP, "-n", "-d", "extension=mbstring", "-d", "error_reporting=0"]
    for ini in sec.get("INI", "").splitlines():
        if ini.strip():
            args += ["-d", ini.strip()]
    env = dict(os.environ, ORACLE_LOG=str(log))
    subprocess.run(args + [str(script)], cwd=workdir, env=env,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)


MINE = {
    "UCS-2", "UCS-2BE", "UCS-2LE", "UCS-4", "UCS-4BE", "UCS-4LE", "UTF-32",
    "UTF-32BE", "UTF-32LE", "UTF-16", "UTF-16BE", "UTF-16LE", "UTF-7",
    "UTF7-IMAP", "BASE64", "Quoted-Printable", "UUENCODE", "HTML-ENTITIES",
}


def canonical_names() -> dict:
    """Map lower-cased names/aliases/MIME names to canonical names."""
    code = (
        'foreach (mb_list_encodings() as $e) { $n = [$e, mb_preferred_mime_name($e) ?: $e];'
        ' foreach (array_merge($n, mb_encoding_aliases($e)) as $a) echo strtolower($a), "\\t", $e, "\\n"; }'
    )
    res = subprocess.run([PHP, "-n", "-d", "extension=mbstring", "-d", "error_reporting=0", "-r", code],
                         capture_output=True, text=True, check=True)
    names = {}
    for line in res.stdout.splitlines():
        alias, name = line.split("\t")
        names.setdefault(alias, name)
    return names


def relevant(line: str, names: dict) -> bool:
    f = line.split("\t")
    encs = f[1:3] if f[0] == "conv" else f[1:2]
    return any(names.get(e.lower()) in MINE for e in encs)


def main() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        for inc in TESTS.glob("*.inc"):
            (work / inc.name).write_text(rewrite(inc.read_text(encoding="latin-1")), encoding="latin-1")
        # Data files some tests read.
        for d in TESTS.iterdir():
            if d.is_dir():
                os.symlink(d, work / d.name)
        log = work / "log.tsv"
        log.touch()
        for name in PHPTS:
            run_phpt(name, work, log)
        lines = log.read_text(encoding="latin-1").splitlines()
    # Deduplicate, keeping order; keep only calls involving our encodings.
    names = canonical_names()
    # Conversion pairs with very many calls (utf_encodings.phpt sweeping
    # whole code point ranges) are thinned to every 4th call (always keeping
    # calls whose input is short, i.e. the targeted error cases) to bound the
    # fixture size.
    seen, uniq = set(), []
    for line in lines:
        if line not in seen and relevant(line, names):
            seen.add(line)
            uniq.append(line)
    per_pair = {}
    for line in uniq:
        key = tuple(line.split("\t")[:3])
        per_pair[key] = per_pair.get(key, 0) + 1
    kept, idx = [], {}
    for line in uniq:
        f = line.split("\t")
        key = tuple(f[:3])
        idx[key] = idx.get(key, 0) + 1
        if per_pair[key] > 300 and len(f[4]) > 64 and idx[key] % 4 != 1:
            continue
        kept.append(line)
    uniq = kept
    (HERE / "phpt.tsv").write_text("\n".join(uniq) + "\n")
    print(f"phpt.tsv: {len(uniq)} lines")
    env = dict(os.environ)
    with open(HERE / "fuzz.tsv", "w") as f:
        subprocess.run([PHP, "-n", "-d", "extension=mbstring", "-d", "error_reporting=0",
                        str(HERE / "fuzz.php")], stdout=f, check=True, env=env)
    print("fuzz.tsv written")


if __name__ == "__main__":
    main()
