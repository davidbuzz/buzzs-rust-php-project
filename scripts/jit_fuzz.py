#!/usr/bin/env python3
"""Differential fuzzer for the JIT (crates/php-vm/src/exec/tape/jit.rs).

Generates random PHP programs made of what the JIT compiles (numeric loops,
nested loops, array reads and writes, comparisons, `break`/`continue`, values
at the int range's ends) and runs each one with the JIT off and in every JIT
mode (`PHPRUST_JIT=0`, `tier1`, `1`, `both`, `all-tier1`, `all`). The interpreter is
the reference: any difference in output or exit status is a finding, and the
program is saved next to a note of the modes that differ.

Usage: scripts/jit_fuzz.py <php-binary> [--count N] [--seed S] [--out DIR]
                           [--timeout SECONDS]
Exit:  0 no difference, 1 at least one, 2 usage.
"""
import argparse
import os
import random
import re
import subprocess
import sys

MODES = ['0', 'tier1', '1', 'both', 'all-tier1', 'all']

INTS = ['0', '1', '2', '3', '7', '-1', '-5', '63', '64', '1000', 'PHP_INT_MAX',
        'PHP_INT_MIN', 'PHP_INT_MAX - 3', 'PHP_INT_MIN + 2', '4611686018427387904']
FLOATS = ['0.0', '0.5', '1.5', '-2.25', '1e308', '-0.0', '3.0', 'NAN', 'INF']
OTHERS = ["'5'", "'x'", 'null', 'true', 'false', "'1.5'"]
ARITH = ['+', '-', '*', '/', '%', '<<', '>>', '&', '|', '^']
CMP = ['<', '<=', '>', '>=', '==', '!=', '===', '!==']


# What every generated function returns (also from the middle of a loop).
RESULT = '[$a, $b, $c, $d, $e, $f, $g, $h, $i, $j, $k, $fv, $fk, $p, $q, $m, $o->x, $o->f, $o->n, $o->u, $z, $fo->x, $fo->n, $fs, $s]'
# Constants (a typed loop reads them as their values) and properties.
CONSTS = ['KI', 'KF', 'KB', 'KS', 'P::CI', 'P::CF', 'P::CB', 'PHP_INT_SIZE', 'M_PI']
PROPS = ['$o->x', '$o->f', '$o->n', '$o->u']
# Elements under string keys (the local `$z`).
STATICS = ['P::$sx', 'P::$sf', 'P::$st']
KEYED = ["$z['k']", "$z['f']", "$z['s']", "$z['none']"]


class Gen:
    def __init__(self, rng):
        self.r = rng
        self.depth = 0
        self.loops = 0

    def pick(self, xs):
        return self.r.choice(xs)

    def var(self):
        return '$' + self.pick('abcdefgh')

    def arr(self):
        return '$' + self.pick(['p', 'q'])

    def index(self):
        k = self.r.random()
        if k < 0.5:
            return f'{self.var()} % 8'
        if k < 0.7:
            return str(self.r.randrange(-1, 10))
        if k < 0.9:
            return '$i'
        return self.var()

    def atom(self):
        k = self.r.random()
        if k < 0.45:
            return self.var()
        if k < 0.6:
            return str(self.r.randrange(-4, 40))
        if k < 0.7:
            return self.pick(INTS)
        if k < 0.8:
            return self.pick(FLOATS)
        if k < 0.86:
            return f'{self.arr()}[{self.index()}]'
        if k < 0.9:
            return f'$m[{self.index()}][{self.index()}]'
        if k < 0.93:
            return self.pick(['$i', '$j', '$n', '$fv', '$fk'])
        if k < 0.96:
            return self.pick(PROPS)
        if k < 0.98:
            return self.pick(CONSTS + KEYED + STATICS)
        return self.pick(OTHERS)

    def expr(self, d=0):
        if d > 2 or self.r.random() < 0.35:
            return self.atom()
        k = self.r.random()
        if k < 0.03:
            return f'{self.pick(["sqrt", "floor", "ceil", "sin", "round", "log", "exp"])}({self.expr(d + 1)})'
        # Small user functions, which a typed loop runs inline.
        if k < 0.05:
            return f'{self.pick(["h0", "h2", "h4", "h5", "h6", "h7", "h9", "hm0", "hm1"])}({self.expr(d + 1)})'
        if k < 0.07:
            return f'{self.pick(["h1", "h3", "h7", "h9"])}({self.expr(d + 1)}, {self.expr(d + 1)})'
        # A literal that is not a number for a parameter the function
        # ignores, takes typed, tests or returns.
        if k < 0.076:
            lit = self.pick(['"s"', "'0'", '""', '[1, 2]', '[]', 'null'])
            return f'{self.pick(["hu", "ht", "hq", "hr"])}({lit}, {self.expr(d + 1)})'
        # Functions that call themselves (a few dozen deep at most).
        if k < 0.082:
            f, bound = self.pick([("hrec", 40), ("hfib", 14)])
            return f'{f}((int)({self.expr(d + 1)}) % {bound}, {self.expr(d + 1)})'
        # Static methods and methods of `$o`, likewise.
        if k < 0.085:
            return f'{self.pick(["P::sm", "$o->im", "P::sd", "$o->gsum", "$fo->gsum"])}({self.expr(d + 1)})'
        if k < 0.092:
            return self.pick(['$o->gx()', '$o->gn()', '$fo->gx()', '$fo->gf()', '$o->gf()', '$o->len()', '$fo->len()'])
        if k < 0.095:
            return f'{self.pick(["$o->pm", "P::sd"])}({self.expr(d + 1)}, {self.atom()})'
        if k < 0.1:
            return f'{self.pick(["intdiv", "min", "max"])}({self.expr(d + 1)}, {self.atom()})'
        if k < 0.14:
            return f'{self.pick(["(int)", "(float)"])}({self.expr(d + 1)})'
        if k < 0.18:
            return f'({self.cond()} ? {self.atom()} : {self.atom()})'
        if k < 0.21:
            return f'({self.arr()}[{self.index()}] ?? {self.atom()})'
        op = self.pick(ARITH if self.r.random() < 0.8 else ['+', '-', '*'])
        a, b = self.expr(d + 1), self.expr(d + 1)
        # Mostly operands the operator takes: an exception ends the whole
        # function, and little of it would have run.
        if self.r.random() < 0.85:
            if op in ('<<', '>>'):
                b = f'({b} & 7)' if self.r.random() < 0.5 else str(self.r.randrange(0, 9))
            elif op in ('%', '/'):
                b = str(self.pick([2, 3, 7, 10, -3, 1000])) if self.r.random() < 0.6 else f'({b} | 1)'
        return f'({a} {op} {b})' if d else f'{a} {op} {b}'

    def cond(self):
        c = f'{self.atom()} {self.pick(CMP)} {self.atom()}'
        k = self.r.random()
        if k > 0.9:
            return self.pick([f'isset({self.arr()}[{self.index()}])', f'empty({self.arr()}[{self.index()}])',
                              f'is_int({self.var()})', f'is_float({self.var()})', f'isset({self.var()})',
                              f'isset({self.pick(PROPS)})', f'empty({self.pick(PROPS)})',
                              f'isset({self.pick(STATICS)})', f'empty({self.pick(STATICS)})',
                              f'h8({self.atom()}, {self.atom()})', f'h8({self.var()}, {self.expr(1)})'])
        if k < 0.1:
            return self.var()
        if k < 0.12:
            # Strings (and what a variable holds) compared.
            return f"{self.pick(['$fs', '$s', '$fk', '$fv', self.var()])} {self.pick(CMP)} {self.pick(['$fs', "'bcd'", "'7'", "''", 'null', '$s', self.atom()])}"
        if k < 0.13:
            return f"{self.pick(['$fo', '$o', '$fv', '$fs', self.var()])} instanceof {self.pick(['P', 'Nope', 'stdClass'])}"
        if k < 0.14:
            # The truth of strings, of what a foreach gives.
            return self.pick(['$fs', '!$fs', '$s && $fs', '$fv || !$fs', '!$fk', '$fo && $fs'])
        if k < 0.15:
            return '!' + self.var()
        if k < 0.25:
            return f'{c} && {self.atom()} {self.pick(CMP)} {self.atom()}'
        return c

    def stmt(self):
        k = self.r.random()
        v = self.var()
        k2 = self.r.random()
        if k2 < 0.04:
            return f'{v} = {self.cond()};'
        if k2 < 0.07:
            return f'{v} = ({self.var()} = {self.expr()}) {self.pick(["+", "-", "*"])} {self.atom()};'
        if k2 < 0.12:
            return self.pick([f'$o->x {self.pick(["+", "-", "*"])}= {self.atom()};', f'{v} = $o->x;',
                              f'{self.pick(PROPS)} {self.pick(["+", "-", "*"])}= {self.atom()};',
                              f'{self.pick(PROPS)} = {self.atom()};', f'{self.pick(PROPS)}{self.pick(["++", "--"])};',
                              f'{v} = {self.pick(KEYED)};', f'{v} = {self.pick(KEYED[:2])};',
                              f'{v} = {self.pick(STATICS)};', f'{self.pick(STATICS)} = {self.atom()};',
                              f'{self.pick(["$o", "$fo"])}->ax({self.atom()});', f'{self.pick(["$o", "$fo"])}->bump();',
                              f'$o->su({self.expr()});', f'{v} = $o->sr({self.atom()});',
                              # Strings: built, copied, read from rows.
                              f"$s .= $z['s'] . {self.atom()};", f'$s = "<{{$i}}>" . $fs;', f'$fs = $s . {self.atom()};',
                              f"$fs = $z['s']; {v} = strlen($fs) + {self.atom()};", f'$s = (string)({self.expr()});',
                              f'{v} = strlen($s . {self.atom()});',
                              # Frameless builtin calls the loop leaves to the interpreter.
                              f"{v} = strpos($s . {self.atom()}, 'b') !== false ? 1 : 0;", f"{v} = str_contains($fs, 'c') === true ? {self.atom()} : 2;",
                              # Array literals.
                              f"{v} = count([{self.var()}, 'k' => {self.atom()}, $s, {self.var()} % 4 => 1]) + {self.atom()};",
                              # What the loop leaves to the interpreter and comes back after.
                              f'{v} = array_sum($q) % 7;', f'$fs = str_repeat("ab", {self.var()} & 3);',
                              f'{v} = in_array({self.atom()}, $p) ? 1 : 2;', f'{v} = strlen(strrev($s));',
                              f'{v} = count(array_keys($q)) + {self.atom()};', f'{v} = (int)round({self.expr()});',
                              f'{self.pick(STATICS[:2])} = {v};', f'{v} = {self.pick(STATICS[:2])};',
                              f'{self.pick(STATICS)}++;', f'{v} = {self.pick(STATICS)}--;', f'{v} += ++{self.pick(STATICS)};',
                              f'{self.pick(KEYED[:3])} = {self.atom()};',
                              f'{v} = {self.pick(PROPS)} {self.pick(ARITH[:3])} {self.pick(CONSTS)};',
                              f'$o->f = {self.expr()};', '$o->x++;', f'$o->x = {self.atom()};',
                              f'{v} = $o->f;'])
        if k2 < 0.15:
            return f'$s .= {self.pick(["\'x\'", self.var(), "$i", "1.5"])};'
        if k2 < 0.165 and self.loops:
            return f'if ({self.cond()}) {{ return {RESULT}; }}'
        if k < 0.22:
            return f'{v} = {self.expr()};'
        if k < 0.34:
            op = self.pick(["+", "-", "*", "+", "-", "*", "%", "|", "^", "&", "<<", ">>", "/"])
            rhs = self.atom()
            if op in ('%', '/', '<<', '>>') and self.r.random() < 0.85:
                rhs = str(self.r.randrange(1, 8))
            return f'{v} {op}= {rhs};'
        if k < 0.42:
            return self.pick([f'{v}++;', f'{v}--;', f'++{v};', f'--{v};',
                              f'{self.var()} = {v}++;', f'{self.var()} = --{v};'])
        if k < 0.52:
            return f'{self.arr()}[{self.index()}] = {self.expr()};'
        if k < 0.54:
            return f'$m[{self.index()}][{self.index()}] {self.pick(["", "", "+", "*"])}= {self.expr()};'
        if k < 0.56:
            return f'{self.arr()}[] = {self.atom()};'
        if k < 0.6:
            return f'{self.arr()}[{self.index()}] {self.pick(["+", "-", "*"])}= {self.atom()};'
        if k < 0.63:
            return f'{self.arr()}[{self.index()}]++;'
        if k < 0.67:
            return f'{v} = count({self.arr()}) + strlen($s);'
        if k < 0.8 and self.depth < 3:
            self.depth += 1
            body = self.block(self.r.randrange(1, 3))
            alt = f' else {{ {self.block(1)} }}' if self.r.random() < 0.3 else ''
            self.depth -= 1
            return f'if ({self.cond()}) {{ {body} }}{alt}'
        if k < 0.84 and self.loops:
            return self.pick(['break;', 'continue;']) if self.r.random() < 0.5 else \
                f'if ({self.cond()}) {{ {self.pick(["break;", "continue;"])} }}'
        if k < 0.94 and self.depth < 2:
            return self.loop()
        return f'{v} = {self.atom()};'

    def block(self, n):
        return ' '.join(self.stmt() for _ in range(n))

    def loop(self):
        self.depth += 1
        self.loops += 1
        ctr = self.pick(['$i', '$j', '$k']) if self.r.random() < 0.8 else self.var()
        n = self.r.randrange(1, 40)
        body = self.block(self.r.randrange(1, 5))
        k = self.r.random()
        if k < 0.06:
            # Over objects and strings: the loop variable is no number.
            head = self.pick(['foreach ($os as $fo)', 'foreach ($os as $fk => $fo)',
                              'foreach ($ss as $fs)', 'foreach ($ss as $fk => $fs)'])
            extra = self.pick([f'{self.var()} += $fo->x;', f'$fo->n++; {self.var()} = $fo->n;',
                               f'{self.var()} = strlen($fs) + {self.atom()};', f'{self.var()} = $fo->im({self.atom()});',
                               f'$fo->f = {self.expr()};', f'{self.var()} = $fo->f * {self.atom()};'])
            self.depth -= 1
            self.loops -= 1
            return f'{head} {{ {extra} {body} }}'
        if k < 0.2:
            subject = self.pick(['$p', '$q', '$m[0]', '$m[1]', '$m', '[1, 2, 3]',
                                 # Numbers under string keys (and an int one).
                                 "['a' => 3, 'b' => -1, 'c' => 7, 9 => 2]", "['x' => 1.5, 'y' => -2.5]",
                                 # The outer foreach's element (an array of $m's, or no array), nested rows, mixed values.
                                 '$fv', '[[1, 2], [3], []]', '$z'])
            head = self.pick(['foreach ({} as $fv)', 'foreach ({} as $fk => $fv)']).format(subject)
            self.depth -= 1
            self.loops -= 1
            return f'{head} {{ {body} }}'
        k = self.r.random()
        if k < 0.08:
            self.depth -= 1
            self.loops -= 1
            return (f'$w = 0; while ({self.pick(["1", "true"])}) {{ {body} '
                    f'if ($w++ >= {n} || {self.cond()}) {{ break; }} }}')
        k = self.r.random()
        if k < 0.45:
            step = self.pick([f'{ctr}++', f'++{ctr}', f'{ctr} += {self.r.randrange(1, 4)}',
                              f'{ctr} = {ctr} + 1'])
            head = f'for ({ctr} = 0; {ctr} {self.pick(["<", "<=", "!="])} {n}; {step})'
            out = f'{head} {{ {body} }}'
        elif k < 0.6:
            out = f'for ({ctr} = {n}; {ctr} > 0; {ctr}--) {{ {body} }}'
        elif k < 0.8:
            out = f'$w = 0; while ($w++ < {n}) {{ {body} }}'
        elif k < 0.9:
            out = f'$w = {n}; do {{ {body} }} while (--$w > 0);'
        else:
            out = f'$w = 0; while ({self.cond()} && $w < {n}) {{ $w++; {body} }}'
        self.depth -= 1
        self.loops -= 1
        return out

    def function(self, name):
        inits = ' '.join(f'${v} = {self.pick(["$x", "$y", "0", "1", "$n"])};' for v in 'abcdefgh')
        body = ' '.join(self.loop() if self.r.random() < 0.7 else self.stmt()
                        for _ in range(self.r.randrange(2, 6)))
        return (f'function {name}($x, $y, $n, $p, $q, $m, $o) {{ $s = "abc"; $i = 0; $j = 0; $k = 0; $w = 0; '
                f'$fv = 0; $fk = 0; $z = ["k" => 3, "f" => 1.5, "s" => "x"]; '
                f'$fo = $o; $fs = ""; $os = [new P(1), $o, new P(2.5), new P(4)]; $ss = ["a", "bcd", "", "ef"]; '
                f'{inits} {body} return {RESULT}; }}')

    def program(self):
        fns = [f'f{i}' for i in range(self.r.randrange(1, 4))]
        out = ['<?php']
        if self.r.random() < 0.3:
            # In a namespace: unqualified calls and constants have a global fallback.
            out.append('namespace Fz;')
        out.append("const KI = 3; const KF = 1.5; const KB = true; const KS = '7';")
        out.append('function h0($x) { return $x * 2 + 1; } function h1($a, $b) { return $a - $b; } '
                   'function h2($x) { if ($x > 5) { return $x - 5; } $y = $x; $y++; return $y; } '
                   'function h3($a, $b) { $t = $a * $b; return $t / 2.0; } function h4($x) { return $x % 7; } '
                   'function h5($x) { return $x < 3 ? $x << 2 : (int)($x * 0.5); } '
                   'function h6(int $x): int { return $x + 3; } function h7(float $x, $y = 2): float { return $x * $y; } '
                   'function h8($a, $b) { return $a <= $b; } function h9(int|float $x, int $k = 5): int|float { return $x - $k; } '
                   # Builtins a typed loop computes, in functions it runs inline.
                   'function hm0($x) { return sqrt(abs($x)) + floor($x * 0.5); } function hm1($x) { return max($x, 3) - intdiv($x, 2); } '
                   'function hu($s, $x) { return $x + 1; } function ht(string $s, $x) { return $x * 2; } '
                   'function hq($s, $x) { return $s ? $x : 1; } function hr($s, $x) { return $s; } '
                   'function hrec($n, $k) { return $n <= 0 ? $k : hrec($n - 1, $k + $n % 3) * 1; } '
                   'function hfib($n, $k) { if ($n < 2) { return $n + $k; } return hfib($n - 1, $k) - hfib($n - 2, 1) + 1; }')
        out.append('class P { const CI = 5; const CF = 0.25; const CB = false; public $x; public $f = 0.5; '
                   'public int $n = 1; public int|float $u = 2; static $sx = 1; static $sf = 0.5; static int $st = 2; '
                   'static function sm($x) { return $x + 2; } function im($x) { return $x * 3; } '
                   'function pm($a, $b) { return $a - $b; } static function sd(int|float $x, int $y = 4) { return $x * $y; } '
                   'function gx() { return $this->x; } function gn(): int { return $this->n; } function gf() { return $this->f * 2; } '
                   'function gsum($k) { return $this->n + $k; } function ax($v) { $this->x += $v; } function bump() { $this->n++; } '
                   'function len() { return sqrt($this->x * $this->x + $this->f * $this->f); } function su($v) { $this->u = $v; } function sr($v) { $this->f = $v; return $v; } '
                   'function __construct($x) { $this->x = $x; } }')
        out += [self.function(f) for f in fns]
        out.append('function show($v) { if (is_float($v)) { return is_nan($v) ? "NAN" : sprintf("%.17g", $v); } '
                   'if (is_array($v)) { $o = []; foreach ($v as $k => $e) { $o[] = $k . "=>" . show($e); } '
                   'return "[" . implode(",", $o) . "]"; } return var_export($v, true); }')
        arrays = ['[1, 2, 3, 4, 5, 6, 7, 8]', '[0.5, 1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5]', '[]',
                  "[1, 'x', 2.5, null, 4, true, 6, 7]", '[3 => 1, 1 => 2, 7 => 3]',
                  '[9, 8, 7, 6, 5, 4, 3, 2, 1, 0]']
        matrices = ['[[1, 2, 3], [4, 5, 6], [7, 8, 9]]', '[[0.5, 1.5], [2.5, 3.5]]', '[]',
                    "[[1, 'x'], [2.5, null], 7]", '[[1, 2, 3, 4, 5, 6, 7, 8], [8, 7, 6, 5, 4, 3, 2, 1]]']
        calls = []
        for f in fns:
            for _ in range(self.r.randrange(2, 6)):
                x = self.pick(INTS + FLOATS + ['3', '10']) if self.r.random() < 0.9 else self.pick(OTHERS)
                y = self.pick(INTS + FLOATS + ['2', '5'])
                calls.append(f'try {{ echo show({f}({x}, {y}, {self.r.randrange(0, 30)}, '
                             f'{self.pick(arrays)}, {self.pick(arrays)}, {self.pick(matrices)}, '
                             f'new P({self.pick(INTS + FLOATS + ["2", OTHERS[0]])}))), "\\n"; }} '
                             f'catch (Throwable $t) {{ echo get_class($t), ": ", $t->getMessage(), "\\n"; }}')
        # Each function several times over: the code is compiled on the
        # first call, the typed loops on the first rounds.
        out.append('for ($round = 0; $round < 3; $round++) {')
        out += calls
        out.append('}')
        return '\n'.join(out) + '\n'


# What the JIT did over the whole run, by mode (`PHPRUST_JIT_STATS`).
TALLY = {}


def run(php, path, mode, timeout):
    env = dict(os.environ, PHPRUST_JIT=mode, PHPRUST_JIT_STATS='1')
    if not mode.startswith('all'):
        # The loops here are short: a low threshold, so that they are
        # compiled part of the way through (the default is 64 rounds).
        env['PHPRUST_JIT_HOT'] = '3'
    try:
        p = subprocess.run([php, '-d', 'memory_limit=256M', '-d', 'max_execution_time=0',
                            '-d', 'error_reporting=0', path],
                           env=env, capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return 'timeout', b''
    m = re.search(rb'jit: (\d+) tapes compiled, (\d+) typed loops, (\d+) failed', p.stderr)
    if m:
        t = TALLY.setdefault(mode, [0, 0, 0])
        for i in range(3):
            t[i] += int(m.group(i + 1))
    return p.returncode, p.stdout


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('php')
    ap.add_argument('--count', type=int, default=200)
    ap.add_argument('--seed', type=int, default=None)
    ap.add_argument('--out', default='jit-fuzz-findings')
    ap.add_argument('--timeout', type=float, default=10.0)
    a = ap.parse_args()
    seed = a.seed if a.seed is not None else random.SystemRandom().randrange(1 << 32)
    print(f'seed {seed}', flush=True)
    os.makedirs(a.out, exist_ok=True)
    work = os.path.join(a.out, 'current.php')
    found = skipped = 0
    for n in range(a.count):
        src = Gen(random.Random(seed * 1_000_003 + n)).program()
        with open(work, 'w') as f:
            f.write(src)
        ref = run(a.php, work, '0', a.timeout)
        if ref[0] == 'timeout':
            # The program itself does not end (a float counter, say).
            skipped += 1
            continue
        bad = [m for m in MODES[1:] if run(a.php, work, m, a.timeout * 3) != ref]
        if bad:
            found += 1
            name = os.path.join(a.out, f'case-{seed}-{n}.php')
            os.replace(work, name)
            print(f'DIFF {name}: modes {",".join(bad)}', flush=True)
    if os.path.exists(work):
        os.remove(work)
    for mode, (tapes, typed, failed) in sorted(TALLY.items()):
        if mode != '0':
            print(f'  PHPRUST_JIT={mode}: {tapes} tapes compiled, {typed} typed loops, {failed} failed')
    print(f'{a.count} programs, {skipped} skipped (no end), {found} differ')
    return 1 if found else 0


if __name__ == '__main__':
    sys.exit(main())
