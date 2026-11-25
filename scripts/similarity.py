#!/usr/bin/env python3
# buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
# Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
"""How similar a Rust file of this repository is to a php-src C file: the figure in the
`// Derived from php-src <path>. Similarity metric to original: N%` line of a ported file
(CLAUDE.md, "Licensing and attribution"; issue #23).

DEFINITION (version 2)
  Vocabulary: the identifiers of php-src's C CODE (.c .h .y .l .re outside tests; comments,
  string literals and numeric literals removed), lower-cased, with the engine prefixes `zend_`,
  `php_`, `zif_`, `zim_`, `z_`, `zstr_`, `zval_` dropped and names shorter than four characters
  dropped. Removed from it, because a port cannot avoid them:
    - every name a PHP program can see: the functions, classes, methods, constants, parameters
      and properties declared in php-src's *.stub.php files, and their @cvalue names;
    - every identifier of the system libraries' headers (scripts/data/similarity-system-names.txt.gz,
      made once by `similarity.py system-names`): two files that call the same library share
      those names without one following the other;
    - the keywords and primitive type names of C and Rust.
  What is left is php-src's internal vocabulary: static helpers, locals, struct fields, macros.
  Tokens of a file: the words of that vocabulary among the identifiers of its code. A name the
  Rust file invented counts for nothing, in either direction.
  Weight of a token: its inverse document frequency over php-src's C files, ln((N + 1) / (df + 1)).
  For a Rust file R and a C file C, with token SETS T(R) and T(C):
    similarity = sum of w(t)^2 over T(R) & T(C) / (|T(R)| * |T(C)|)   (cosine; |X| = sqrt(sum w^2))
    A = weight of the shared tokens / weight of T(C)   ("how much of the original is in the port")
    B = weight of the shared tokens / weight of T(R)   ("how much of what the port took from
        php-src's vocabulary it took from this file")
  A pair has a figure only when it shares at least five tokens (--min-shared).
  Everything the figure depends on is in the repository or in the pinned php-src tree.

THE RULE FOR HEADERS
  A Rust file is labelled `Derived from php-src <C>` with the ONE php-src source file (.c .y .l
  .re; a header only declares what its source file defines) it is most similar to, when that
  figure is at least the threshold: `similarity.py label --min 30`. Nothing else enters.
  The threshold was set on 26 Nov 2026 against the 1,290 Rust files whose origin was known
  (named in the file's own comments, or established by a reader). Labels the rule gives, and
  how many of them name a file the Rust file is known NOT to come from:
      threshold   files labelled   known wrong
         20%          153               2
         25%          101               1
         30%           70               0      (66 name a source the file names itself; the
         35%           50               0       other 4 are sources its comments left out)
  A file below the threshold is not labelled by this rule, which says nothing about its origin:
  most ports rename what they take and score low.

WHAT IT IS AND IS NOT
  It measures shared internal vocabulary. A high figure is evidence that the Rust file follows
  the C file. A low figure or none is NOT evidence that it does not: a port that renames
  everything shares nothing. Whether a file is derived is decided by reading it, never by this.

USAGE
  similarity.py pair RUST_FILE C_FILE...     the figure, A, B and the shared tokens of each pair
  similarity.py nearest RUST_FILE [-n 6]     the php-src files nearest to a Rust file
  similarity.py scan OUT.tsv                 every Rust file against every C file: one row per
                                             near pair, with whether the Rust file's own leading
                                             comments name that C file and its rank among all
  similarity.py label --min N [OUT.tsv]      the rule: each Rust file whose nearest php-src source
                                             file reaches N%, with that file and the figure
  similarity.py system-names [/usr/include]  rewrite the list of system-library identifiers
  Paths of C files are relative to the php-src root (--php-src, default modules/php-src).
  The first run reads php-src (a minute or two); the result is cached under target/ and keyed
  by the php-src commit, the list of system names and this file's version.
"""
import argparse
import collections
import csv
import gzip
import hashlib
import math
import os
import pickle
import re
import subprocess
import sys
from pathlib import Path

VERSION = 2
SYSTEM_NAMES = Path(__file__).resolve().parent / "data" / "similarity-system-names.txt.gz"
REPO = Path(__file__).resolve().parent.parent
C_KEYWORDS = set(
    "auto break case char const continue default do double else enum extern float for goto if inline int long"
    " register restrict return short signed sizeof static struct switch typedef union unsigned void volatile"
    " while bool true false null define include ifdef ifndef endif elif defined undef pragma uint32_t uint64_t"
    " int32_t int64_t size_t uint8_t uint16_t zend_long zend_ulong zend_string zval".split()
)
RUST_KEYWORDS = set(
    "as break const continue crate else enum extern false fn for if impl in let loop match mod move mut pub ref"
    " return self static struct super trait true type unsafe use where while async await dyn u8 u16 u32 u64"
    " usize i8 i16 i32 i64 isize f64 f32 bool str string vec option some none ok err result box rc refcell"
    " cell".split()
)
PREFIX = re.compile(r"^(?:_?zend_|_?php_|zif_|zim_|z_|zstr_|zval_)")
IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
NUMBER = re.compile(r"(?<![A-Za-z_0-9])\d[A-Za-z0-9_.]*")
C_SUFFIXES = (".c", ".h", ".y", ".l", ".re")
# Third-party code bundled with php-src (THIRD_PARTY.md): a source under one of these keeps its
# own licence, which the `scan` rows flag.
BUNDLED = (
    "ext/mbstring/libmbfl", "ext/bcmath/libbcmath", "ext/date/lib", "ext/lexbor/lexbor", "ext/uri/uriparser",
    "ext/standard/libavifinfo", "ext/fileinfo/libmagic", "ext/hash/xxhash", "ext/hash/murmur", "ext/hash/sha3",
    "ext/pcre/pcre2lib", "ext/gd/libgd", "ext/opcache/jit/ir",
)


def strip_c(s):
    """C source without its comments, string and character literals."""
    out = []
    i = 0
    n = len(s)
    while i < n:
        c = s[i]
        if s.startswith("/*", i):
            j = s.find("*/", i + 2)
            i = n if j < 0 else j + 2
            out.append(" ")
        elif s.startswith("//", i):
            j = s.find("\n", i)
            i = n if j < 0 else j
        elif c == '"' or c == "'":
            j = i + 1
            while j < n and s[j] != c:
                j += 2 if s[j] == "\\" else 1
            i = j + 1
            out.append(" ")
        else:
            out.append(c)
            i += 1
    return "".join(out)


def strip_rust(s):
    """Rust source without its comments, string, raw string and character literals."""
    out = []
    i = 0
    n = len(s)
    while i < n:
        c = s[i]
        if s.startswith("//", i):
            j = s.find("\n", i)
            i = n if j < 0 else j
        elif s.startswith("/*", i):
            j = s.find("*/", i + 2)
            i = n if j < 0 else j + 2
        elif c == "r" and re.match(r'r#*"', s[i : i + 8]):
            m = re.match(r'r(#*)"', s[i : i + 8])
            end = '"' + m.group(1)
            j = s.find(end, i + len(m.group(0)))
            if j < 0:
                j = n
            i = j + len(end)
            out.append(" ")
        elif c == '"':
            j = i + 1
            while j < n and s[j] != '"':
                j += 2 if s[j] == "\\" else 1
            i = j + 1
            out.append(" ")
        elif c == "'" and i + 2 < n and (s[i + 2] == "'" or (s[i + 1] == "\\" and 0 < s.find("'", i + 2) < i + 8)):
            # A character literal ('x', '\n', '\u{..}'); a lifetime has no closing quote.
            i = s.find("'", i + 2) + 1
            out.append(" ")
        else:
            out.append(c)
            i += 1
    return "".join(out)


def tokens(text, lang, drop):
    """The token set of a file's text (`lang`: "c" or "rust")."""
    code = NUMBER.sub(" ", strip_c(text) if lang == "c" else strip_rust(text))
    out = set()
    for m in IDENT.finditer(code):
        raw = m.group(0).lower()
        t = PREFIX.sub("", raw)
        if len(t) < 4 or raw in drop or t in drop:
            continue
        out.add(t)
    return out


def stub_names(php_src):
    """Every name a PHP program can see, from php-src's stub files."""
    names = set()
    for f in sorted(php_src.rglob("*.stub.php")):
        s = f.read_text(errors="replace")
        for m in re.finditer(r"\b(?:function|class|interface|trait|enum|const|case)\s+&?\s*([A-Za-z_][A-Za-z0-9_]*)", s):
            names.add(m.group(1).lower())
        for m in re.finditer(r"\$([A-Za-z_][A-Za-z0-9_]*)", s):
            names.add(m.group(1).lower())
        for m in re.finditer(r"@cvalue\s+([A-Za-z_][A-Za-z0-9_]*)", s):
            names.add(m.group(1).lower())
        for m in re.finditer(r"\bconst\s+(?:[\w|?\\]+\s+)?([A-Z_][A-Z0-9_]*)\s*=", s):
            names.add(m.group(1).lower())
    return names


def system_names(root):
    """Every identifier of the system headers under `root` (php's own installed headers and the
    C++ and toolkit trees left out)."""
    names = set()
    skip = ("c++", "boost", "llvm", "clang", "rust", "qt5", "qt6")
    for d, dirs, files in os.walk(root):
        dirs[:] = sorted(x for x in dirs if not x.startswith("php") and x not in skip)
        if "/c++" in d or "/php" in d:
            continue
        for f in files:
            if f.endswith(".h"):
                try:
                    text = open(os.path.join(d, f), errors="replace").read()
                except OSError:
                    continue
                names.update(t.lower() for t in IDENT.findall(text))
    return names


def c_files(php_src):
    out = []
    for d, _, files in os.walk(php_src):
        if "/.git" in d or "/tests" in d:
            continue
        for f in files:
            if f.endswith(C_SUFFIXES):
                out.append(os.path.relpath(os.path.join(d, f), php_src))
    return sorted(out)


def git(args, cwd):
    return subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, check=False).stdout


def load_system_names():
    if not SYSTEM_NAMES.is_file():
        return set()
    with gzip.open(SYSTEM_NAMES, "rt") as f:
        return set(f.read().split())


class Corpus:
    """php-src's vocabulary and weights, and the token sets of its C files and of Rust files."""

    def __init__(self, php_src, min_shared=5, system=None):
        self.php_src = php_src
        self.min_shared = min_shared
        system = load_system_names() if system is None else system
        commit = git(["rev-parse", "HEAD"], php_src).strip() or "unknown"
        digest = hashlib.sha1("\n".join(sorted(system)).encode()).hexdigest()
        key = hashlib.sha1(f"{VERSION}|{commit}|{digest}".encode()).hexdigest()[:16]
        cache = REPO / "target" / f"similarity-{key}.pickle"
        if cache.is_file():
            with cache.open("rb") as f:
                self.drop, self.cset = pickle.load(f)
        else:
            self.drop = stub_names(php_src) | C_KEYWORDS | RUST_KEYWORDS | system
            self.cset = {
                c: tokens((php_src / c).read_text(errors="replace"), "c", self.drop) for c in c_files(php_src)
            }
            cache.parent.mkdir(exist_ok=True)
            with cache.open("wb") as f:
                pickle.dump((self.drop, self.cset), f)
        df = collections.Counter()
        for s in self.cset.values():
            df.update(s)
        self.df_c = df
        n = len(self.cset)
        self.idf = {t: math.log((n + 1) / (k + 1)) for t, k in df.items()}
        self.cnorm = {c: math.sqrt(sum(self.idf[t] ** 2 for t in s)) or 1.0 for c, s in self.cset.items()}
        self.cweight = {c: sum(self.idf[t] for t in s) or 1.0 for c, s in self.cset.items()}
        self.postings = None
        self._rust = None

    @property
    def rust(self):
        """The repository's tracked Rust files."""
        if self._rust is None:
            self._rust = [p for p in git(["ls-files", "--", "*.rs"], REPO).splitlines() if (REPO / p).is_file()]
        return self._rust

    def rust_tokens(self, path):
        """The tokens of a Rust file: its identifiers that are words of php-src's vocabulary."""
        full = Path(path) if Path(path).is_absolute() else (REPO / path if (REPO / path).is_file() else Path(path))
        return {t for t in tokens(full.read_text(errors="replace"), "rust", self.drop) if t in self.idf}

    def pair(self, c, rs):
        """(similarity, A, B, shared tokens) of C file `c` and the Rust token set `rs`."""
        shared = self.cset[c] & rs
        rnorm = math.sqrt(sum(self.idf[t] ** 2 for t in rs)) or 1.0
        rweight = sum(self.idf[t] for t in rs) or 1.0
        sw = sum(self.idf[t] for t in shared)
        cos = sum(self.idf[t] ** 2 for t in shared) / (self.cnorm[c] * rnorm)
        return cos, sw / self.cweight[c], sw / rweight, shared

    def nearest(self, rs, top=6, floor=0.02, sources_only=False):
        """The C files nearest to a Rust token set, best first: (similarity, A, B, shared, path)
        of those that share at least `min_shared` tokens. Every C file sharing a token is scored;
        `sources_only` leaves the headers (.h) out of the ranking."""
        if self.postings is None:
            self.postings = collections.defaultdict(list)
            for c, s in self.cset.items():
                for t in s:
                    self.postings[t].append(c)
        acc = collections.Counter()
        for t in rs:
            w2 = self.idf[t] ** 2
            if w2:
                for c in self.postings[t]:
                    acc[c] += w2
        rnorm = math.sqrt(sum(self.idf[t] ** 2 for t in rs)) or 1.0
        out = []
        for score, c in sorted(((v / (rnorm * self.cnorm[c]), c) for c, v in acc.items()), reverse=True):
            if score < floor or len(out) >= top:
                break
            if sources_only and c.endswith(".h"):
                continue
            shared = self.cset[c] & rs
            if len(shared) >= self.min_shared:
                cos, a, b, shared = self.pair(c, rs)
                out.append((cos, a, b, shared, c))
        return out


# What a Rust file's own leading comments name: php-src paths, and file names unique in php-src.
RE_PATH = re.compile(r"\b((?:Zend|main|ext|sapi|TSRM|scripts|build)/[A-Za-z0-9_./{},*-]*[A-Za-z0-9_}*])|\b(run-tests\.php)\b")
RE_FILE = re.compile(r"\b([A-Za-z0-9_-]+\.(?:c|h|y|l|re|stub\.php))\b")


def named_sources(text, php_src, by_name):
    """The php-src files the first 120 lines of a Rust file name: by path, or by a file name
    that only one php-src file has."""
    lead = "\n".join(text.split("\n")[:120])
    out = set()
    for m in RE_PATH.finditer(lead):
        p = (m.group(1) or m.group(2)).rstrip(".,;)")
        if (php_src / p).is_file():
            out.add(p)
    for m in RE_FILE.finditer(lead):
        hits = by_name.get(m.group(1), ())
        if len(hits) == 1:
            out.add(hits[0])
    return out


def cmd_pair(corpus, args):
    rs = corpus.rust_tokens(args.rust)
    status = 0
    for c in args.c:
        if c not in corpus.cset:
            print(f"{c}: not a C file of php-src (paths are relative to {corpus.php_src})", file=sys.stderr)
            status = 2
            continue
        cos, a, b, shared = corpus.pair(c, rs)
        few = f"  (fewer than {corpus.min_shared} shared tokens: too few to label by)" if len(shared) < corpus.min_shared else ""
        print(f"{c}: similarity {round(cos * 100)}%  A {round(a * 100)}%  B {round(b * 100)}%  shared {len(shared)}{few}")
        if args.verbose and shared:
            ranked = sorted(shared, key=lambda t: -corpus.idf[t])
            print("   " + " ".join(ranked))
    return status


def cmd_nearest(corpus, args):
    for cos, a, b, shared, c in corpus.nearest(corpus.rust_tokens(args.rust), top=args.n):
        print(f"{round(cos * 100):3d}%  A {round(a * 100):3d}%  B {round(b * 100):3d}%  shared {len(shared):4d}  {c}")
    return 0


def cmd_scan(corpus, args):
    by_name = collections.defaultdict(list)
    for c in corpus.cset:
        by_name[os.path.basename(c)].append(c)
    with open(args.out, "w", newline="") as f:
        w = csv.writer(f, delimiter="\t", lineterminator="\n")
        w.writerow(["rust", "c", "similarity", "A_original_in_port", "B_port_in_original", "shared", "rank", "named_by_the_file", "bundled_library"])
        files = pairs = 0
        for p in corpus.rust:
            named = named_sources((REPO / p).read_text(errors="replace"), corpus.php_src, by_name)
            rs = corpus.rust_tokens(p)
            near = corpus.nearest(rs)
            rank = {c: k + 1 for k, (_, _, _, _, c) in enumerate(near)}
            rows = {c: (cos, a, b, shared) for cos, a, b, shared, c in near}
            for c in named:
                if c not in rows and c in corpus.cset:
                    cos, a, b, shared = corpus.pair(c, rs)
                    if len(shared) >= corpus.min_shared:
                        rows[c] = (cos, a, b, shared)
            for c, (cos, a, b, shared) in sorted(rows.items(), key=lambda kv: -kv[1][0]):
                bundled = "y" if c.startswith(BUNDLED) else ""
                w.writerow([p, c, f"{cos:.3f}", f"{a:.3f}", f"{b:.3f}", len(shared), rank.get(c, ""), "y" if c in named else "", bundled])
                pairs += 1
            files += bool(rows)
    print(f"{len(corpus.rust)} Rust files, {len(corpus.cset)} C files: {pairs} near pairs of {files} Rust files written to {args.out}")
    return 0


def cmd_label(corpus, args):
    """The rule for headers: each Rust file with the php-src source file it is most similar to,
    when the figure reaches --min (the module's documentation gives the threshold and how it
    was set)."""
    rows = []
    for p in corpus.rust:
        near = corpus.nearest(corpus.rust_tokens(p), top=1, sources_only=True)
        if near and round(near[0][0] * 100) >= args.min:
            cos, a, b, shared, c = near[0]
            rows.append((p, c, round(cos * 100), len(shared), "y" if c.startswith(BUNDLED) else ""))
    out = open(args.out, "w", newline="") if args.out else sys.stdout
    w = csv.writer(out, delimiter="\t", lineterminator="\n")
    w.writerow(["rust", "c", "similarity_percent", "shared", "bundled_library"])
    w.writerows(rows)
    if args.out:
        out.close()
        print(f"{len(rows)} of {len(corpus.rust)} Rust files reach {args.min}%: {args.out}")
    return 0


def cmd_system_names(args):
    """Rewrites the list of system-library identifiers: those of the headers under `root` that
    php-src's code also uses (the others can never be shared by a pair)."""
    names = system_names(args.root)
    used = set()
    for c in c_files(args.php_src):
        text = NUMBER.sub(" ", strip_c((args.php_src / c).read_text(errors="replace")))
        for m in IDENT.finditer(text):
            raw = m.group(0).lower()
            used.add(raw)
            used.add(PREFIX.sub("", raw))
    keep = sorted(names & used)
    SYSTEM_NAMES.parent.mkdir(exist_ok=True)
    with gzip.GzipFile(SYSTEM_NAMES, "wb", mtime=0) as f:
        f.write(("\n".join(keep) + "\n").encode())
    print(f"{len(keep)} of {len(names)} system identifiers are used by php-src: {SYSTEM_NAMES}")
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0], formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--php-src", type=Path, default=REPO / "modules" / "php-src")
    ap.add_argument("--min-shared", type=int, default=5, help="shared tokens a pair needs to have a figure")
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("pair", help="the figure of a Rust file against php-src files")
    p.add_argument("rust")
    p.add_argument("c", nargs="+")
    p.add_argument("-v", "--verbose", action="store_true", help="list the shared tokens, heaviest first")
    p = sub.add_parser("nearest", help="the php-src files nearest to a Rust file")
    p.add_argument("rust")
    p.add_argument("-n", type=int, default=6)
    p = sub.add_parser("scan", help="every Rust file against every C file")
    p.add_argument("out")
    p = sub.add_parser("label", help="the rule for headers: nearest php-src file at or above --min")
    p.add_argument("--min", type=int, required=True, help="threshold in percent")
    p.add_argument("out", nargs="?")
    p = sub.add_parser("system-names", help="rewrite the list of system-library identifiers")
    p.add_argument("root", nargs="?", default="/usr/include")
    args = ap.parse_args(argv)
    if not args.php_src.is_dir():
        ap.error(f"php-src not found at {args.php_src}")
    if args.cmd == "system-names":
        return cmd_system_names(args)
    corpus = Corpus(args.php_src, args.min_shared)
    return {"pair": cmd_pair, "nearest": cmd_nearest, "scan": cmd_scan, "label": cmd_label}[args.cmd](corpus, args)


if __name__ == "__main__":
    sys.exit(main())
