#!/usr/bin/env python3
# buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
# Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
"""What a change affects: the crates to test, and the php-src test folders.

Maps the files changed since a base commit (committed, uncommitted and
untracked) to the workspace crates that contain them, adds every crate that
depends on one of those (normal, dev and build dependencies, transitively,
from `cargo metadata`), and prints what to test:

    cargo test $(scripts/affected.py)            # -p list, or --workspace
    cargo clippy --all-targets $(scripts/affected.py)
    target/debug/phpt-runner --php target/debug/php ... $(scripts/affected.py --phpt)
    scripts/affected.py --list                   # the crates, and why each one
    scripts/affected.py --base REF               # compare against REF

The default base is the merge base with `master` on another branch; on
`master`, the merge base with its upstream (the unpushed commits), else
`HEAD` (the uncommitted changes only).

A change to a file every crate builds with (`Cargo.lock`, the workspace
`Cargo.toml`, `.cargo/`, `rust-toolchain*`), or under `crates/` outside any
member, selects the whole workspace. Other files outside the crates (docs,
`scripts/`, `perf/`, `.github/`) select nothing.

PHPT: a change to a core crate (the engine, the runtime, the `php`
binaries, the stream layer, the runner) selects the whole suite; otherwise
the test folders of the extensions whose binding crates are affected, and
of the affected SAPI crates. Prints nothing when no test folder applies.
"""

import argparse
import json
import os
import subprocess
import sys

# Files every crate is built with: a change to one selects the workspace.
WORKSPACE_FILES = ("Cargo.lock", "Cargo.toml", "rust-toolchain", "rust-toolchain.toml")
WORKSPACE_DIRS = (".cargo/",)

# A change to one of these runs the whole PHPT suite: all PHP code goes
# through them.
PHPT_CORE = {
    "php-vm",
    "php-runtime",
    "php-lexer",
    "php-parser",
    "php-support",
    "php-memlimit",
    "php-streams",
    "php-ext-streams",
    "php-cli",
    "phpt-runner",
}

# php-src extension folders of the binding crates whose name is not
# `php-ext-<folder>`, or that register several extensions.
EXT_DIRS = {
    "php-ext-opcache": ["opcache"],
    "php-ext-pdo": ["pdo", "pdo_sqlite", "pdo_mysql", "sqlite3"],
    "php-ext-stdarr": ["standard"],
    "php-ext-stdmisc": ["standard"],
    "php-ext-stdstr": ["standard"],
    "php-ext-sysv": ["sysvmsg", "sysvsem", "sysvshm", "shmop"],
    "php-ext-xml": ["libxml", "xml", "xmlwriter"],
    "php-ext-zendtest": ["zend_test"],
    "php-ext-zlib": ["zlib", "bz2"],
}

# php-src test folders of the SAPI crates.
SAPI_DIRS = {
    "php-cli-server": ["sapi/cli/tests"],
    "php-sapi-apache2": ["sapi/apache2handler/tests"],
}

PHP_SRC = "modules/php-src"


def git(root, *args):
    out = subprocess.run(
        ["git", *args], cwd=root, capture_output=True, text=True, check=False
    )
    return out.stdout.strip() if out.returncode == 0 else None


def default_base(root):
    branch = git(root, "branch", "--show-current")
    if branch and branch != "master":
        base = git(root, "merge-base", "HEAD", "master")
        if base:
            return base
    upstream = git(root, "merge-base", "HEAD", "@{upstream}")
    return upstream or "HEAD"


def changed_files(root, base):
    diff = git(root, "diff", "--name-only", base)
    if diff is None:
        sys.exit(f"affected.py: cannot diff against {base!r}")
    untracked = git(root, "ls-files", "--others", "--exclude-standard") or ""
    files = {f for f in diff.splitlines() + untracked.splitlines() if f}
    return sorted(files)


def workspace(root):
    """Member name -> (directory relative to root, set of member deps)."""
    meta = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps", "--offline"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    )
    data = json.loads(meta.stdout)
    members = set(data["workspace_members"])
    pkgs = [p for p in data["packages"] if p["id"] in members]
    names = {p["name"] for p in pkgs}
    crates = {}
    for p in pkgs:
        d = os.path.relpath(os.path.dirname(p["manifest_path"]), root)
        # Every kind: a dev-dependency's change can break the tests that
        # use it, a build-dependency's the build.
        deps = {dep["name"] for dep in p["dependencies"] if dep["name"] in names}
        crates[p["name"]] = (d.replace(os.sep, "/") + "/", deps)
    return crates


def crate_of(path, crates):
    best = None
    for name, (d, _) in crates.items():
        if path.startswith(d) and (best is None or len(d) > len(crates[best][0])):
            best = name
    return best


def select(files, crates):
    """(whole workspace?, {crate: reason}) for the changed files."""
    changed = {}
    for f in files:
        if f in WORKSPACE_FILES or f.startswith(WORKSPACE_DIRS):
            return True, {f: "workspace file changed"}
        c = crate_of(f, crates)
        if c is None:
            if f.startswith("crates/"):
                # A crate folder no member owns (a crate removed or added).
                return True, {f: "crate outside the workspace members"}
            continue
        changed.setdefault(c, "changed")
    rdeps = {name: set() for name in crates}
    for name, (_, deps) in crates.items():
        for dep in deps:
            rdeps[dep].add(name)
    affected = dict(changed)
    todo = list(changed)
    while todo:
        c = todo.pop()
        for r in sorted(rdeps[c]):
            if r not in affected:
                affected[r] = f"depends on {c}"
                todo.append(r)
    return False, affected


def phpt_dirs(root, whole, affected, changed):
    if whole or any(c in PHPT_CORE for c in changed):
        return [PHP_SRC]
    dirs = set()
    for c in affected:
        if c.startswith("php-ext-"):
            for ext in EXT_DIRS.get(c, [c[len("php-ext-"):]]):
                dirs.add(f"{PHP_SRC}/ext/{ext}")
        for d in SAPI_DIRS.get(c, []):
            dirs.add(f"{PHP_SRC}/{d}")
    return sorted(d for d in dirs if os.path.isdir(os.path.join(root, d)))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--base", help="compare against this commit (see the header)")
    out = ap.add_mutually_exclusive_group()
    out.add_argument("--phpt", action="store_true", help="print php-src test folders")
    out.add_argument("--list", action="store_true", help="list the crates and why")
    args = ap.parse_args()

    root = git(os.getcwd(), "rev-parse", "--show-toplevel") or os.getcwd()
    base = args.base or default_base(root)
    files = changed_files(root, base)
    crates = workspace(root)
    whole, affected = select(files, crates)
    changed = [c for c, why in affected.items() if why == "changed"]

    if args.phpt:
        print(" ".join(phpt_dirs(root, whole, affected, changed)))
    elif args.list:
        print(f"base {base}: {len(files)} changed file(s)", file=sys.stderr)
        if whole:
            for what, why in affected.items():
                print(f"the whole workspace ({why}: {what})")
        else:
            for c in sorted(affected):
                print(f"{c}  ({affected[c]})")
    elif whole:
        print("--workspace")
    elif affected:
        print(" ".join(f"-p {c}" for c in sorted(affected)))
    else:
        print("affected.py: no crate changed", file=sys.stderr)


if __name__ == "__main__":
    main()
