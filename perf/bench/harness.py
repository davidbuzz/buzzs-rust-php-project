#!/usr/bin/env python3
"""Benchmark harness for phprust vs a reference PHP binary.

Used by run.sh and compare.sh; see perf/bench/README section in perf/PLAN.md.

For every workload it runs the candidate and the reference binary REPS times,
interleaved (cand, ref, cand, ref, ...) so slow drifts in box load hit both,
pinned to one core with taskset, and records per binary:
  * wall  - best wall-clock seconds (time.perf_counter around fork/wait)
  * cpu   - best user+sys CPU seconds of the child (wait4 rusage)
  * rss   - peak RSS in KB (max over runs)
Sub-timings printed by Zend/bench.php and Zend/micro_bench.php ("name  0.123")
become extra rows (best in-script seconds, no cpu column).

Output is a TSV table on stdout ('#' lines are comments), one row per
workload:  bench  cand_wall  cand_cpu  ref_wall  ref_cpu  ratio_wall  ratio_cpu
cand_rss_kb  ref_rss_kb  status.  ratio = cand / ref (1.10 = 10 % slower).
status is 'ok', or 'DIFF' when the candidate's stdout differs from the
reference's (timing lines are masked), or 'FAIL' on a non-zero exit.
"""
import os
import re
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, '..', '..'))
ZEND = os.path.join(ROOT, 'modules', 'php-src', 'Zend')

# name -> argv after the binary (relative to cwd = Zend/ for the php-src ones)
def workloads():
    w = [('startup', ['-r', 'echo 1;'], 20)]  # (name, args, inner repetitions)
    w.append(('bench.php', [os.path.join(ZEND, 'bench.php')], 1))
    w.append(('micro_bench.php', [os.path.join(ZEND, 'micro_bench.php')], 1))
    suite = os.path.join(HERE, 'suite')
    for f in sorted(os.listdir(suite)):
        if f.endswith('.php'):
            w.append((f[:-4], [os.path.join(suite, f)], 1))
    return w

COMMON = ['-d', 'memory_limit=1G', '-d', 'opcache.enable_cli=0']
SUBLINE = re.compile(rb'^([A-Za-z_][\w$()\[\]\->:= ]*?)\s{2,}(\d+\.\d+)\s*$')
MASK = re.compile(rb' *\d+\.\d{3}')


def run_once(binary, args, inner, core):
    timeout_sec = float(os.environ.get('BENCH_TIMEOUT', '3.0'))
    cmd = ['timeout', '--kill-after=0.5s', f'{timeout_sec:.1f}s', binary] + COMMON + args
    if core is not None:
        cmd = ['taskset', '-c', str(core)] + cmd
    wall = cpu = 0.0
    rss = 0
    out = b''
    rc = 0
    for _ in range(inner):
        t0 = time.perf_counter()
        p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, cwd=ZEND)
        try:
            out, _ = p.communicate(timeout=timeout_sec + 2.0)
            status = p.wait()
            rc = rc or status
        except subprocess.TimeoutExpired:
            p.kill()
            out, _ = p.communicate()
            return timeout_sec, timeout_sec, 0, b'TIMEOUT', 124
        wall += time.perf_counter() - t0
        # When timing out via GNU timeout, status is 124
        if rc in (124, 137):
            return timeout_sec, timeout_sec, 0, b'TIMEOUT', 124
        rss = max(rss, 0)
    return wall / inner, cpu / inner, rss, out, rc


def subtimes(out):
    d = {}
    for line in out.splitlines():
        m = SUBLINE.match(line.rstrip())
        if m:
            d[m.group(1).decode().strip()] = float(m.group(2))
    return d


def main():
    if len(sys.argv) < 3:
        sys.exit('usage: harness.py <candidate-binary> <label> [reference-binary]')
    cand, label = sys.argv[1], sys.argv[2]
    ref = sys.argv[3] if len(sys.argv) > 3 else '/usr/bin/php8.5'
    reps = int(os.environ.get('REPS', '3'))
    core = os.environ.get('CORE', str(os.cpu_count() - 1))
    core = None if core == 'none' else int(core)
    filt = re.compile(os.environ.get('FILTER', '.'))
    subs = os.environ.get('SUBS', '1') == '1'
    print(f'# label={label} candidate={cand} reference={ref} reps={reps} core={core} '
          f'date={time.strftime("%Y-%m-%dT%H:%M:%S")} loadavg={os.getloadavg()[0]:.1f}')
    print('bench\tcand_wall\tcand_cpu\tref_wall\tref_cpu\tratio_wall\tratio_cpu\tcand_rss_kb\tref_rss_kb\tstatus')
    tot = {'cw': 0.0, 'cc': 0.0, 'rw': 0.0, 'rc': 0.0}
    for name, args, inner in workloads():
        if not filt.search(name):
            continue
        best = {}
        outs = {}
        for _ in range(reps):
            for who, b in (('c', cand), ('r', ref)):
                w, c, rss, out, rc = run_once(b, args, inner, core)
                cur = best.setdefault(who, {'w': w, 'c': c, 'rss': rss, 'rc': rc, 'sub': {}})
                cur['w'] = min(cur['w'], w)
                cur['c'] = min(cur['c'], c)
                cur['rss'] = max(cur['rss'], rss)
                cur['rc'] = cur['rc'] or rc
                for k, v in subtimes(out).items():
                    cur['sub'][k] = min(cur['sub'].get(k, v), v)
                outs[who] = out
        c, r = best['c'], best['r']
        status = 'ok'
        if c['rc'] != 0:
            status = f'FAIL({c["rc"]})'
        elif MASK.sub(b'#', outs['c']) != MASK.sub(b'#', outs['r']):
            status = 'DIFF'
        print(f"{name}\t{c['w']:.4f}\t{c['c']:.4f}\t{r['w']:.4f}\t{r['c']:.4f}\t"
              f"{c['w'] / r['w']:.2f}\t{c['c'] / max(r['c'], 1e-9):.2f}\t{c['rss']}\t{r['rss']}\t{status}",
              flush=True)
        if name != 'startup':
            tot['cw'] += c['w']; tot['cc'] += c['c']; tot['rw'] += r['w']; tot['rc'] += r['c']
        if subs:
            for k in r['sub']:
                if k in c['sub'] and k.lower() != 'total':
                    cv, rv = c['sub'][k], r['sub'][k]
                    print(f"  {name}:{k}\t{cv:.4f}\t-\t{rv:.4f}\t-\t{cv / max(rv, 1e-4):.2f}\t-\t-\t-\tsub", flush=True)
    if tot['rw'] > 0:
        print(f"TOTAL(excl. startup)\t{tot['cw']:.4f}\t{tot['cc']:.4f}\t{tot['rw']:.4f}\t{tot['rc']:.4f}\t"
              f"{tot['cw'] / tot['rw']:.2f}\t{tot['cc'] / max(tot['rc'], 1e-9):.2f}\t-\t-\t-")


if __name__ == '__main__':
    main()
