#!/usr/bin/env python3
"""Side-by-side runtime check: the deliverable.

Runs the C implementation and the candidate on identical inputs, each with its
default configuration (C's CLI default has opcache loaded but disabled,
`opcache.enable_cli=0`, and no JIT), and compares the total runtime:

    total candidate runtime <= 1.10 * total C runtime

Inputs (each one process, run as it is, both binaries the same arguments):
  Zend/bench.php, Zend/micro_bench.php     php-src's own benchmarks, whole
  perf/bench/suite/*.php                   whole scripts
  perf/bench/<op>.php                      the micro-op loops at their default size

Every input runs ROUNDS times (default 5) alternating C, candidate, C, ...,
pinned to the same cores (taskset, CORES, default 13-15). Per input and
binary the fastest round counts (other load on the box only adds time); the
totals are the sums. Both wall-clock and CPU time (user + system, from
wait4) are reported; the verdict is on wall-clock.

usage: timing.py <candidate-php> [--ref PHP] [--filter RX] [--rounds N]
Exit status: 0 = total within 1.10 x C, 1 = over, 2 = usage / environment error.
"""
import os
import re
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, '..', '..'))
ZEND = os.path.join(ROOT, 'modules', 'php-src', 'Zend')
FLOOR = 1.10
OPS = ['empty_loop', 'int_add', 'float_mul', 'is_smaller_jmp', 'concat_assign', 'dim_read',
       'dim_write', 'hash_read', 'prop_read', 'prop_write', 'strlen', 'abs', 'count',
       'userfunc', 'userfunc_typed', 'method', 'static_method', 'closure', 'new_obj']
# memory_limit only so that an unbounded script cannot take the box down
# (CLAUDE.md); opcache.enable_cli is C's CLI default, stated for clarity.
COMMON = ['-d', 'memory_limit=1G', '-d', 'opcache.enable_cli=0']


def inputs():
    yield 'zend/bench.php', os.path.join(ZEND, 'bench.php')
    yield 'zend/micro_bench.php', os.path.join(ZEND, 'micro_bench.php')
    suite = os.path.join(HERE, 'suite')
    for f in sorted(os.listdir(suite)):
        if f.endswith('.php'):
            yield f'suite/{f}', os.path.join(suite, f)
    for op in OPS:
        yield f'ops/{op}.php', os.path.join(HERE, f'{op}.php')


def run_once(binary, script, cores):
    """(wall seconds, cpu seconds, exit status) of one run."""
    cmd = ['taskset', '-c', cores, binary] + COMMON + [script]
    t0 = time.perf_counter()
    p = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                         cwd=os.path.dirname(script))
    _, status, ru = os.wait4(p.pid, 0)
    wall = time.perf_counter() - t0
    return wall, ru.ru_utime + ru.ru_stime, os.waitstatus_to_exitcode(status)


def main():
    args = sys.argv[1:]
    if not args or args[0].startswith('-'):
        print(__doc__, file=sys.stderr)
        return 2
    cand = os.path.abspath(args.pop(0))
    ref = '/usr/bin/php'
    filt = None
    rounds = int(os.environ.get('ROUNDS', '5'))
    while args:
        a = args.pop(0)
        if a == '--ref':
            ref = args.pop(0)
        elif a == '--filter':
            filt = re.compile(args.pop(0))
        elif a == '--rounds':
            rounds = int(args.pop(0))
        else:
            print(f'timing.py: unknown argument {a}', file=sys.stderr)
            return 2
    cores = os.environ.get('CORES', '13-15')
    for b in (cand, ref):
        if not os.access(b, os.X_OK):
            print(f'timing.py: {b} is not executable', file=sys.stderr)
            return 2
    print(f'# candidate {cand}\n# reference {ref}\n# {rounds} rounds, cores {cores}, '
          'fastest round per input\n')
    print(f"{'input':<28} {'C wall s':>9} {'ours wall s':>11} {'ratio':>6}   "
          f"{'C cpu s':>8} {'ours cpu s':>10} {'ratio':>6}")
    tot = {'cw': 0.0, 'ow': 0.0, 'cc': 0.0, 'oc': 0.0}
    failed = []
    for name, script in inputs():
        if filt and not filt.search(name):
            continue
        best = {'c': [float('inf'), float('inf')], 'o': [float('inf'), float('inf')]}
        for _ in range(rounds):
            for key, binary in (('c', ref), ('o', cand)):
                wall, cpu, rc = run_once(binary, script, cores)
                if rc != 0:
                    failed.append(f'{name} ({"C" if key == "c" else "ours"} exit {rc})')
                best[key][0] = min(best[key][0], wall)
                best[key][1] = min(best[key][1], cpu)
        (cw, cc), (ow, oc) = best['c'], best['o']
        tot['cw'] += cw
        tot['ow'] += ow
        tot['cc'] += cc
        tot['oc'] += oc
        print(f'{name:<28} {cw:9.3f} {ow:11.3f} {ow / cw:6.2f}   {cc:8.3f} {oc:10.3f} '
              f'{(oc / cc if cc else 0):6.2f}', flush=True)
    wall_ratio = tot['ow'] / tot['cw'] if tot['cw'] else float('inf')
    cpu_ratio = tot['oc'] / tot['cc'] if tot['cc'] else float('inf')
    print(f"\n{'TOTAL':<28} {tot['cw']:9.3f} {tot['ow']:11.3f} {wall_ratio:6.2f}   "
          f"{tot['cc']:8.3f} {tot['oc']:10.3f} {cpu_ratio:6.2f}")
    for f in sorted(set(failed)):
        print(f'# nonzero exit: {f}')
    ok = wall_ratio <= FLOOR
    print(f'\n# criterion: total runtime <= {FLOOR:.2f} x C: '
          f'{"MET" if ok else "NOT MET"} ({wall_ratio:.3f} wall, {cpu_ratio:.3f} cpu)')
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main())
