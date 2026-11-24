#!/usr/bin/env python3
# buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
# Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
"""Writes one PHP script per operation into the directory given: each runs
the operation N times inside a function (so it runs as ordinary code does,
not at the top level), and `baseline.php` runs the empty loop. run.sh counts
the instructions of each, minus the baseline, per operation."""
import os, sys
d = sys.argv[1]
N = 200000
prelude = r'''<?php
function f1($a) { return $a; }
function f2($a, $b) { return $a; }
function rfalse($a) { return false; }
function rnull($a) { if ($a) { return null; } return 1; }
function fd($a, $b = null, $c = []) { return $a; }
function fargs() { return func_get_args(); }
function fstatic() { static $n = 0; return ++$n; }
$gv = ['x' => 1];
function fglobal() { global $gv; return $gv['x']; }
class C { public $p = 1; public $arr = ['k' => 1]; public function m($a, $b) { return $a; } public static function sm($a) { return $a; }
  public function getp() { return $this->p; } }
class K { public $a; public $b; public function __construct($a, $b = 2) { $this->a = $a; $this->b = $b; } }
class Hook {
  public $callbacks = [10 => ['f1' => ['function' => 'f1', 'accepted_args' => 1]]];
  public function dispatch($value, $args) {
    foreach ($this->callbacks as $priority => $cbs) {
      foreach ($cbs as $the_) {
        $args[0] = $value;
        $value = call_user_func_array($the_['function'], array_slice($args, 0, (int) $the_['accepted_args']));
      }
    }
    return $value;
  }
}
'''
setup = r'''  $obj = new C; $arr = ['key' => 1, 'b' => 2, 'c' => 3]; $small = ['a', 'b', 'c', 'd', 'e'];
  $assoc = ['a' => 1, 'b' => 2, 'c' => 3, 'd' => 4, 'e' => 5];
  $s = '  Hello World  '; $csv = 'a,b,c,d,e'; $cb = 'f2'; $mcb = [$obj, 'm']; $args = [1, 2];
  $fn = function ($a) { return $a; }; $hook = new Hook; $x = 'xyz'; $y = 5; $null = null;
'''
ops = {
 'baseline': '',
 'fcall': '$r = f2($x, $y);',
 'ret_false': '$r = rfalse($x);',
 'ret_null': '$r = rnull($x);',
 'fcall_defaults': '$r = fd($x);',
 'mcall': '$r = $obj->m($x, $y);',
 'getter': '$r = $obj->getp();',
 'scall': '$r = C::sm($x);',
 'closure': '$r = $fn($x);',
 'cuf': '$r = call_user_func($cb, $x, $y);',
 'cufa': '$r = call_user_func_array($cb, $args);',
 'cufa_method': '$r = call_user_func_array($mcb, $args);',
 'hook_dispatch': '$r = $hook->dispatch($x, [$x, $y]);',
 'func_get_args': '$r = fargs($x, $y);',
 'static_var': '$r = fstatic();',
 'global': '$r = fglobal();',
 'new_ctor': '$r = new K($x);',
 'isset_dim': '$r = isset($arr[\'key\']);',
 'dim_read': '$r = $arr[\'key\'];',
 'dim_write': '$arr[\'key\'] = $i;',
 'array_key_exists': '$r = array_key_exists(\'key\', $arr);',
 'prop_read': '$r = $obj->p;',
 'prop_write': '$obj->p = $i;',
 'prop_dim_write': '$obj->arr[\'k\'] = $i;',
 'foreach_kv': 'foreach ($assoc as $k => $v) {}',
 'array_literal': '$r = [\'id\' => $i, \'name\' => $x, \'n\' => 3];',
 'array_merge': '$r = array_merge($assoc, $arr);',
 'array_slice': '$r = array_slice($small, 1);',
 'in_array': '$r = in_array(\'d\', $small, true);',
 'count': '$r = count($small);',
 'is_array': '$r = is_array($arr) && is_string($x);',
 'coalesce': '$r = $null ?? \'default\';',
 'concat': '$r = \'a\' . $x . \'b\' . $y;',
 'interp': '$r = "a{$x}b{$y}";',
 'concat_assign': '$s2 = \'\'; $s2 .= $x; $s2 .= $x;',
 'str_replace': '$r = str_replace(\'World\', \'There\', $s);',
 'strpos': '$r = strpos($s, \'World\');',
 'sprintf': '$r = sprintf(\'%s-%d\', $x, $i);',
 'implode': '$r = implode(\',\', $small);',
 'explode': '$r = explode(\',\', $csv);',
 'trim': '$r = trim($s);',
 'strtolower': '$r = strtolower($x);',
 'substr': '$r = substr($s, 2, 5);',
 'preg_match': '$r = preg_match(\'/^[a-z]+$/\', $x);',
 'preg_replace': '$r = preg_replace(\'/[^a-z]/\', \'\', $s);',
 'compact_extract': '$a1 = 1; $b1 = 2; $r = compact(\'a1\', \'b1\'); extract($r);',
 'is_callable': '$r = is_callable($cb);',
 'instanceof': '$r = $obj instanceof C;',
 'identical': '$r = $x === \'xyz\';',
}
for name, op in ops.items():
    with open(os.path.join(d, name + '.php'), 'w') as f:
        f.write(prelude + 'function bench() {\n' + setup + f'  for ($i = 0; $i < {N}; $i++) {{ {op} }}\n}}\nbench();\n')
print(len(ops), 'ops,', N, 'iterations')
