#!/usr/bin/env python3
"""Deterministic instruction-count performance gate.

Counts user-space instructions (``perf stat -e instructions:u``, or callgrind's
``Ir`` when perf is unavailable: BACKEND=callgrind) of each workload for the
candidate php and for the reference /usr/bin/php, subtracts each binary's own
fixed cost (startup + compiling the driver), and compares the ratio
cand/ref against a per-workload budget stored in budgets.tsv.  This mirrors
php-src's benchmark/benchmark.php, which tracks callgrind instruction counts of
Zend/bench.php per commit, but as a pass/fail gate.

Workload sets (GATE_SET, comma separated; default "zend,micro,suite,ops"):
  zend   one process per Zend/bench.php test function (simple, mandel, ...)
  micro  one process per Zend/micro_bench.php section, N=GATE_MICRO_N (300000)
  suite  perf/bench/suite/*.php whole scripts
  ops    perf/bench/<op>.php micro-ops, instructions per loop iteration
         (runs at n and n/10 and takes the difference, like irun.sh)

usage: gate.py <candidate-php> [--budgets FILE] [--init | --ratchet] [--filter RX]
  --init     write budgets.tsv from this run (measured ratio + MARGIN, default 3 %)
  --ratchet  lower budgets that the run beats by more than MARGIN (never raises)
Exit status: 0 = every workload within budget, 1 = at least one over budget or
failed, 2 = usage / environment error.
"""
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, '..', '..'))
ZEND = os.path.join(ROOT, 'modules', 'php-src', 'Zend')
REF = os.environ.get('REF', '/usr/bin/php')
COMMON = ['-d', 'memory_limit=1G', '-d', 'opcache.enable_cli=0']
MARGIN = float(os.environ.get('MARGIN', '0.03'))
TARGET = 1.10
OPS = ['empty_loop', 'int_add', 'float_mul', 'is_smaller_jmp', 'concat_assign', 'dim_read',
       'dim_write', 'hash_read', 'prop_read', 'prop_write', 'strlen', 'abs', 'count',
       'userfunc', 'userfunc_typed', 'method', 'static_method', 'closure', 'new_obj']


def backend():
    b = os.environ.get('BACKEND')
    if b:
        return b
    try:
        subprocess.run(['sudo', '-n', 'perf', '--version'], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return 'perf'
    except (OSError, subprocess.CalledProcessError):
        pass
    if shutil.which('valgrind'):
        return 'callgrind'
    sys.exit('gate.py: need `sudo -n perf` or valgrind')


BACKEND = None


def count(binary, args):
    """User-space instructions of one run of `binary args`."""
    cmd = [binary] + COMMON + args
    if BACKEND == 'perf':
        full = ['sudo', '-n', 'perf', 'stat', '-x', ',', '-e', 'instructions:u', '--',
                'setpriv', f'--reuid={os.getuid()}', f'--regid={os.getgid()}', '--init-groups'] + cmd
        p = subprocess.run(full, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, cwd=ZEND)
        for line in p.stderr.decode(errors='replace').splitlines():
            f = line.split(',')
            if len(f) > 2 and 'instructions' in f[2]:
                return int(f[0]), p.returncode
        raise RuntimeError(f'no instruction count for {cmd}: {p.stderr[-500:]!r}')
    full = ['valgrind', '--tool=callgrind', '--callgrind-out-file=/dev/null'] + cmd
    p = subprocess.run(full, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, cwd=ZEND)
    m = re.search(rb'Collected : (\d+)', p.stderr)
    if not m:
        raise RuntimeError(f'no Ir for {cmd}')
    return int(m.group(1)), p.returncode


def split_bench(tmp):
    """Zend/bench.php -> (lib.php, [(name, call)])."""
    src = open(os.path.join(ZEND, 'bench.php')).read().splitlines()
    start = next(i for i, l in enumerate(src) if l.startswith('$t0 = $t = start_test();'))
    lib = os.path.join(tmp, 'bench_lib.php')
    open(lib, 'w').write('\n'.join(src[:start]) + '\n')
    calls = []
    for l in src[start + 1:]:
        m = re.match(r'^([a-z0-9_]+)\((\d*)\);$', l)
        if m and m.group(1) != 'total':
            calls.append((m.group(1), l))
    return lib, calls


def split_micro(tmp, n):
    src = open(os.path.join(ZEND, 'micro_bench.php')).read().splitlines()
    cdef = next(i for i, l in enumerate(src) if l.startswith('const N ='))
    lib = os.path.join(tmp, 'micro_lib.php')
    open(lib, 'w').write('\n'.join(src[:cdef]) + f'\nconst N = {n};\n')
    calls = []
    pending = []
    setup = []
    for l in src[cdef + 1:]:
        m = re.match(r"^\$t = end_test\(\$t, '(.*)'(, \$overhead)?\);$", l)
        if m:
            raw = m.group(1)
            for a, b in (('++', ' pp '), ('--', ' mm '), ('+=', ' pluseq '), ('?:', ' elvis '),
                         ('?', ' q '), ('::', ' cc '), ('->', ' arrow '), ('=', ' eq ')):
                raw = raw.replace(a, b)
            name = re.sub(r'[^A-Za-z0-9_]+', '_', raw).strip('_') or 'x'
            calls.append((name, '\n'.join(setup + pending)))
            pending = []
        elif l.startswith('$x = new Foo()'):
            setup.append(l)
        elif re.match(r'^[A-Za-z_$][\w:>$-]*\(N\);$', l):
            pending.append(l)
    return lib, calls


def driver(tmp, lib, name, body):
    path = os.path.join(tmp, f'{name}.php')
    with open(path, 'w') as f:
        f.write(f"<?php\nrequire {lib!r};\nob_start();\n{body}\nob_end_clean();\n")
    return path


def workloads(sets, tmp, filt):
    """Yields (name, kind, payload)."""
    if 'zend' in sets:
        lib, calls = split_bench(tmp)
        base = driver(tmp, lib, 'zend__empty', '')
        for name, call in calls:
            yield f'zend:{name}', 'sub', (driver(tmp, lib, f'zend_{name}', call), base)
    if 'micro' in sets:
        lib, calls = split_micro(tmp, int(os.environ.get('GATE_MICRO_N', '300000')))
        base = driver(tmp, lib, 'micro__empty', '')
        for name, body in calls:
            yield f'micro:{name}', 'sub', (driver(tmp, lib, f'micro_{name}', body), base)
    if 'suite' in sets:
        suite = os.path.join(HERE, 'suite')
        for f in sorted(os.listdir(suite)):
            if f.endswith('.php'):
                yield f'suite:{f[:-4]}', 'startup', os.path.join(suite, f)
    if 'ops' in sets:
        for op in OPS:
            yield f'ops:{op}', 'iter', os.path.join(HERE, f'{op}.php')


def measure(binary, kind, payload, cache):
    if kind == 'sub':
        script, base = payload
        key = (binary, base)
        if key not in cache:
            cache[key] = count(binary, [base])[0]
        n, rc = count(binary, [script])
        return n - cache[key], rc
    if kind == 'startup':
        key = (binary, '-r')
        if key not in cache:
            cache[key] = count(binary, ['-r', ''])[0]
        n, rc = count(binary, [payload])
        return n - cache[key], rc
    n = int(os.environ.get('GATE_OPS_N', '200000'))
    a, rc = count(binary, [payload, str(n)])
    c, _ = count(binary, [payload, str(n // 10)])
    return (a - c) // (n - n // 10), rc


def load_budgets(path):
    b = {}
    if os.path.exists(path):
        for line in open(path):
            if line.startswith('#') or not line.strip():
                continue
            f = line.rstrip('\n').split('\t')
            b[f[0]] = float(f[1])
    return b


def save_budgets(path, budgets, measured):
    with open(path, 'w') as f:
        f.write('# workload\tbudget (max cand/ref instruction ratio)\tlast measured\n')
        f.write(f'# Final target for every row: {TARGET:.2f} (parity goal 1.00). Regenerate: perf/bench/gate.py <php> --init / --ratchet\n')
        for k in sorted(budgets):
            f.write(f'{k}\t{budgets[k]:.2f}\t{measured.get(k, float("nan")):.3f}\n')


def main():
    global BACKEND
    args = sys.argv[1:]
    if not args or args[0].startswith('-'):
        sys.exit(__doc__)
    cand = os.path.abspath(args.pop(0))
    budgets_path = os.path.join(HERE, 'budgets.tsv')
    mode = 'check'
    filt = re.compile('.')
    while args:
        a = args.pop(0)
        if a == '--budgets':
            budgets_path = args.pop(0)
        elif a in ('--init', '--ratchet'):
            mode = a[2:]
        elif a == '--filter':
            filt = re.compile(args.pop(0))
        else:
            sys.exit(f'unknown argument {a}')
    BACKEND = backend()
    sets = os.environ.get('GATE_SET', 'zend,micro,suite,ops').split(',')
    budgets = load_budgets(budgets_path)
    measured = {}
    failed = []
    cache = {}
    print(f'# gate backend={BACKEND} candidate={cand} reference={REF} margin={MARGIN}')
    print('workload\tcand_ins\tref_ins\tratio\tbudget\tstatus')
    with tempfile.TemporaryDirectory() as tmp:
        for name, kind, payload in workloads(sets, tmp, filt):
            if not filt.search(name):
                continue
            try:
                c, crc = measure(cand, kind, payload, cache)
                r, _ = measure(REF, kind, payload, cache)
            except RuntimeError as e:
                print(f'{name}\t-\t-\t-\t-\tERROR {e}', flush=True)
                failed.append(name)
                continue
            ratio = c / max(r, 1)
            measured[name] = ratio
            bud = budgets.get(name)
            if crc != 0:
                status = f'FAIL(exit {crc})'
                failed.append(name)
            elif bud is None:
                status = 'NEW'
            elif ratio > bud:
                status = 'OVER BUDGET'
                failed.append(name)
            else:
                status = 'ok'
            if ratio <= TARGET and status == 'ok':
                status = 'ok (meets 1.10 target)'
            print(f'{name}\t{c}\t{r}\t{ratio:.2f}\t{"-" if bud is None else f"{bud:.2f}"}\t{status}',
                  flush=True)
    if measured:
        geo = math.exp(sum(math.log(max(v, 1e-9)) for v in measured.values()) / len(measured))
        print(f'# geometric mean ratio over {len(measured)} workloads: {geo:.2f}')
    if mode == 'init':
        nb = {k: math.ceil(v * (1 + MARGIN) * 100) / 100 for k, v in measured.items()}
        save_budgets(budgets_path, {**budgets, **nb}, measured)
        print(f'# wrote {len(nb)} budgets to {budgets_path}')
        return 0
    if mode == 'ratchet':
        nb = dict(budgets)
        for k, v in measured.items():
            cand_b = math.ceil(v * (1 + MARGIN) * 100) / 100
            if k not in nb or cand_b < nb[k]:
                nb[k] = cand_b
        save_budgets(budgets_path, nb, measured)
        print(f'# ratcheted budgets in {budgets_path}')
    if failed:
        print(f'# FAILED: {len(failed)} workload(s) over budget or failing: {" ".join(failed)}')
        return 1
    print('# PASS')
    return 0


if __name__ == '__main__':
    sys.exit(main())
