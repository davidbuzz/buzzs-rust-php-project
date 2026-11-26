#!/usr/bin/env python3
# buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
# Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
"""Differential check of what reflection says about internal functions.

Dumps every parameter of every internal function and method as
`ReflectionParameter` shows it (its string form, whether a default value
is available and whether that default is a constant) under our `php` and
under the C implementation, and compares the parameters both know.  A
generator of test doubles, proxies or stubs reads exactly this, and
declares a method it cannot be compatible with when a default or a type
is missing.  Standard library only.

    python3 refldiff.py [--ours PATH] [--ref PATH] [--kind NAME] [-n N]

Prints the number of parameters that differ, by kind of difference, with
N examples of each (default 5; `--kind` lists one kind in full).  The
reference is PHP 8.5: where php-src master changed a signature the
difference is master's, not a fault.  Exit status 1 when anything differs.
"""
import argparse
import collections
import os
import re
import subprocess
import sys
import tempfile

DUMP = r"""<?php
$out = [];
$param = function (ReflectionParameter $p): string {
    $s = (string) $p;
    $s .= ' | dva=' . var_export($p->isDefaultValueAvailable(), true);
    if ($p->isDefaultValueAvailable()) {
        try {
            $s .= ' const=' . var_export($p->isDefaultValueConstant(), true);
        } catch (Throwable $e) {
            $s .= ' const=!' . get_class($e);
        }
    }
    return $s;
};
foreach (get_defined_functions()['internal'] as $f) {
    foreach ((new ReflectionFunction($f))->getParameters() as $p) {
        $out[] = "$f(): " . $param($p);
    }
}
foreach (array_merge(get_declared_classes(), get_declared_interfaces(), get_declared_traits()) as $c) {
    $rc = new ReflectionClass($c);
    if (!$rc->isInternal()) {
        continue;
    }
    foreach ($rc->getMethods() as $m) {
        if ($m->class !== $rc->getName()) {
            continue;
        }
        foreach ($m->getParameters() as $p) {
            $out[] = "$c::{$m->getName()}(): " . $param($p);
        }
    }
}
sort($out);
echo implode("\n", $out), "\n";
"""

LINE = re.compile(r"^(\S+\(\)): Parameter #(\d+) \[ (.*) \] \| (.*)$")


def dump(binary, extra, script):
    """The parameters `binary` reports: {(function, position): (text, flags)}."""
    cmd = ["taskset", "-c", "12", binary, *extra, script]
    run = subprocess.run(cmd, capture_output=True, timeout=120, check=False)
    found = {}
    for line in run.stdout.decode("utf-8", "replace").splitlines():
        m = LINE.match(line)
        if m:
            found[(m.group(1), int(m.group(2)))] = (m.group(3), m.group(4))
    return found


def kind_of(ref, ours):
    """What differs between the reference's (text, flags) and ours."""
    (rt, rf), (ot, of) = ref, ours
    if "<default>" in ot and "<default>" not in rt:
        return "default missing in ours"
    if "<default>" in rt and "<default>" not in ot:
        return "default only in ours"
    if ot.replace("'", '"') == rt.replace("'", '"'):
        return "quote style"
    if rt.split(" = ")[0] != ot.split(" = ")[0]:
        return "type, name or optionality"
    if rt == ot and rf != of:
        return "isDefaultValueConstant"
    return "default text"


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    root = os.path.normpath(os.path.join(here, "..", "..", "..", ".."))
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    ap.add_argument("--ours", default=os.path.join(root, "target", "release", "php"))
    ap.add_argument("--ref", default="/usr/bin/php8.5")
    ap.add_argument("--kind", help="list every difference of this kind")
    ap.add_argument("-n", type=int, default=5, help="examples per kind")
    args = ap.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        script = os.path.join(tmp, "dump.php")
        with open(script, "w", encoding="utf-8") as f:
            f.write(DUMP)
        ref = dump(args.ref, ["-d", "memory_limit=1G"], script)
        ours = dump(args.ours, [], script)
    common = [k for k in ref if k in ours]
    differ = sorted(k for k in common if ref[k] != ours[k])
    print(f"{len(ref)} parameters under the reference, {len(ours)} under ours, "
          f"{len(common)} in both, {len(differ)} differ")
    kinds = collections.defaultdict(list)
    for k in differ:
        kinds[kind_of(ref[k], ours[k])].append(k)
    for kind, keys in sorted(kinds.items(), key=lambda kv: -len(kv[1])):
        if args.kind and args.kind != kind:
            continue
        print(f"\n{len(keys)}  {kind}")
        for k in keys if args.kind else keys[: args.n]:
            print(f"    {k[0]} #{k[1]}")
            print(f"        ref:  {ref[k][0]} | {ref[k][1]}")
            print(f"        ours: {ours[k][0]} | {ours[k][1]}")
    return 1 if differ else 0


if __name__ == "__main__":
    sys.exit(main())
