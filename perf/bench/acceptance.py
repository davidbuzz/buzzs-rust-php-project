#!/usr/bin/env python3
"""Deterministic instruction-count performance acceptance check.

Counts user-space instructions (``perf stat -e instructions:u``, or callgrind's
``Ir`` when perf is unavailable: BACKEND=callgrind) of each workload for the
candidate php, and compares each count against the recorded goal: the
instruction counts measured for the C implementation (/usr/bin/php), stored in
goal.tsv. This mirrors php-src's benchmark/benchmark.php, which tracks
callgrind instruction counts of Zend/bench.php per commit, as a pass/fail check.

The acceptance criterion is one number per workload:
    candidate instructions <= 1.10 * recorded C instructions
The check PASSES only when every workload meets the criterion. There is no
aggregate statistic and no progressive checkpoints: the goal is the final
deliverable — every workload within 10% of the C implementation.

Workload sets (ACCEPTANCE_SET, comma separated; default "zend,micro,suite,ops"):
  zend   one process per Zend/bench.php test function (simple, mandel, ...)
  micro  one process per Zend/micro_bench.php section, N=ACCEPTANCE_MICRO_N (300000)
  suite  perf/bench/suite/*.php whole scripts
  ops    perf/bench/<op>.php micro-ops, instructions per loop iteration
         (runs at n and n/10 and takes the difference, like irun.sh)

usage: acceptance.py <candidate-php> [--goal FILE] [--ref PHP] [--filter RX]
  --record  measure the reference C php and write goal.tsv (the goal numbers
            come from a real measurement of /usr/bin/php, never hand-written)
Exit status: 0 = every workload within the goal, 1 = at least one over goal or
failed, 2 = usage / environment error.
"""
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
FLOOR = 1.10
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
    sys.exit('acceptance.py: need `sudo -n perf` or valgrind')


BACKEND = None


def count(binary, args, timeout=None):
    """User-space instructions of one run of `binary args` with a hard timeout."""
    if timeout is None:
        timeout = float(os.environ.get('ACCEPTANCE_TIMEOUT', '15.0' if BACKEND == 'callgrind' else '2.5'))
    cmd = [binary] + COMMON + args
    timeout_cmd = ['timeout', '--kill-after=0.5s', f'{timeout:.1f}s']
    if BACKEND == 'perf':
        full = ['sudo', '-n', 'perf', 'stat', '-x', ',', '-e', 'instructions:u', '--',
                'setpriv', f'--reuid={os.getuid()}', f'--regid={os.getgid()}', '--init-groups'] + timeout_cmd + cmd
        try:
            p = subprocess.run(full, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, cwd=ZEND,
                               timeout=timeout + 1.0)
        except subprocess.TimeoutExpired:
            subprocess.run(['sudo', '-n', 'pkill', '-9', '-f', os.path.basename(binary)],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            raise RuntimeError(f'timed out after {timeout}s (hard kill)')
        if p.returncode in (124, 137):
            raise RuntimeError(f'timed out after {timeout}s (exit {p.returncode})')
        for line in p.stderr.decode(errors='replace').splitlines():
            f = line.split(',')
            if len(f) > 2 and 'instructions' in f[2]:
                return int(f[0]), p.returncode
        raise RuntimeError(f'no instruction count for {cmd}: {p.stderr[-500:]!r}')
    full = ['valgrind', '--tool=callgrind', '--callgrind-out-file=/dev/null'] + timeout_cmd + cmd
    try:
        p = subprocess.run(full, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, cwd=ZEND,
                           timeout=timeout + 2.0)
    except subprocess.TimeoutExpired:
        subprocess.run(['pkill', '-9', '-f', os.path.basename(binary)],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        raise RuntimeError(f'timed out after {timeout}s (hard kill)')
    if p.returncode in (124, 137):
        raise RuntimeError(f'timed out after {timeout}s (exit {p.returncode})')
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
        lib, calls = split_micro(tmp, int(os.environ.get('ACCEPTANCE_MICRO_N', '300000')))
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
    n = int(os.environ.get('ACCEPTANCE_OPS_N', '200000'))
    a, rc = count(binary, [payload, str(n)])
    c, _ = count(binary, [payload, str(n // 10)])
    return (a - c) // (n - n // 10), rc


def load_goal(path):
    goal = {}
    if os.path.exists(path):
        for line in open(path):
            if line.startswith('#') or not line.strip():
                continue
            f = line.rstrip('\n').split('\t')
            goal[f[0]] = int(f[1])
    return goal


def save_goal(path, rows):
    with open(path, 'w') as f:
        f.write('# goal.tsv — the goal: user-space instruction counts measured for the C implementation\n')
        f.write(f'# Acceptance criterion: candidate instructions <= {FLOOR:.2f} x goal per workload. Regenerate: perf/bench/acceptance.py --record\n')
        f.write('# workload\tC_instructions\n')
        for k in sorted(rows):
            f.write(f'{k}\t{rows[k]}\n')


def main():
    global BACKEND
    args = sys.argv[1:]
    if not args:
        sys.exit(__doc__)
    cand = None
    goal_path = os.path.join(HERE, 'goal.tsv')
    mode = 'check'
    filt = re.compile('.')
    ref = REF
    while args:
        a = args.pop(0)
        if a == '--goal':
            goal_path = args.pop(0)
        elif a == '--ref':
            ref = args.pop(0)
        elif a == '--record':
            mode = 'record'
        elif a == '--filter':
            filt = re.compile(args.pop(0))
        else:
            cand = os.path.abspath(a)
    BACKEND = backend()
    sets = os.environ.get('ACCEPTANCE_SET', 'zend,micro,suite,ops').split(',')
    if mode == 'record':
        target = ref  # record mode measures the C implementation itself
        goal = {}
    else:
        if cand is None:
            sys.exit('acceptance.py: <candidate-php> required (or use --record)')
        target = cand
        if not os.path.exists(goal_path):
            sys.exit(f'acceptance.py: goal file {goal_path} missing — run acceptance.py --record')
        goal = load_goal(goal_path)
    measured = {}
    failed = []
    cache = {}
    print(f'# acceptance backend={BACKEND} target={target} goal={goal_path}')
    print('workload\tcand_ins\tgoal\tratio\tstatus' if mode == 'check' else 'workload\tC_instructions')
    with tempfile.TemporaryDirectory() as tmp:
        for name, kind, payload in workloads(sets, tmp, filt):
            if not filt.search(name):
                continue
            try:
                c, crc = measure(target, kind, payload, cache)
            except RuntimeError as e:
                print(f'{name}\t-\t-\t-\tERROR {e}', flush=True)
                failed.append(name)
                continue
            if mode == 'record':
                measured[name] = c
                print(f'{name}\t{c}', flush=True)
                continue
            g = goal.get(name)
            ratio = c / max(g, 1) if g is not None else float('nan')
            if crc != 0:
                status = f'FAIL(exit {crc})'
                failed.append(name)
            elif g is None:
                status = 'NEW'
                failed.append(name)
            elif ratio > FLOOR:
                status = 'OVER GOAL'
                failed.append(name)
            else:
                status = 'ok'
            print(f'{name}\t{c}\t{"-" if g is None else g}\t{ratio:.2f}\t{status}', flush=True)
    if mode == 'record':
        save_goal(goal_path, measured)
        print(f'# goal recorded: {len(measured)} workloads -> {goal_path}')
        return 0
    print(f'# criterion: candidate instructions <= {FLOOR:.2f}x the recorded C count, every workload')
    if failed:
        print(f'# FAILED: {len(failed)} workload(s) over goal or failing: {" ".join(failed)}')
        return 1
    print('# PASS')
    return 0


if __name__ == '__main__':
    sys.exit(main())
