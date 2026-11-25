<?php
const DEF = 'cdef';
class K { const C = [1, 2]; public function __construct(public $v = 'kv') {} }
function d($a = DEF, $b = K::C, $c = null, $d = [DEF => 1], $e = new K(5)) { return [func_num_args(), $a, $b, $c, $d, $e->v]; }
function many($x, ...$rest) { return [func_num_args(), func_get_args(), $rest]; }
function fewer($a, $b = 2) { return [$a, $b, func_get_args()]; }
function named(int $a = 1, string $b = 'b', ...$extra) { return [$a, $b, $extra]; }
function &refret(array &$arr) { $arr[] = count($arr); return $arr; }
function byref(&$x, $v) { $x = $v; return gettype($x); }
function typed(int $i, ?float $f = null, string|int $u = 'u', bool $b = false): string { return var_export([$i, $f, $u, $b], true); }
function rec($n) { return $n <= 0 ? [] : [...rec($n - 1), $n]; }
class Box { public $p; public array $arr = []; public static $sp; }
function caller($i) {
  $out = [];
  $out[] = d(); $out[] = d('x'); $out[] = d(c: $i, b: 'bb');
  $out[] = many(1); $out[] = many(1, 2, $i); $out[] = many(...[1, 2, 3]);
  $out[] = fewer($i, $i, $i, $i);
  $out[] = named(b: 'z'); $out[] = named(...['b' => 'q', 'a' => $i]);
  $out[] = named(2, 'c', 3, 4); $out[] = named(...[1, 'w'], ...['k' => 9]);
  $box = new Box; $arr = []; $u = null;
  $out[] = byref($undef, $i); $out[] = $undef;
  $out[] = byref($box->p, $i); $out[] = byref($box->arr['k'], 'v'); $out[] = byref(Box::$sp, [$i]);
  $out[] = byref($arr[1][2], 1.5); $out[] = byref($u, $i);
  $out[] = [$box, Box::$sp, $arr, $u];
  $a2 = [9]; $r = &refret($a2); $r[] = 'r'; $out[] = $a2;
  foreach ([1, '2', 3.0, true, '4 ', 5.5, 'x', null, [], PHP_INT_MAX . '0'] as $v) {
    try { $out[] = typed($v, $v, $v, $v); } catch (TypeError $e) { $out[] = $e->getMessage(); }
  }
  $out[] = rec($i + 1);
  return $out;
}
set_error_handler(function ($no, $msg, $f, $l) { echo "E[$no] $msg @$l\n"; return true; });
for ($i = 0; $i < 4; $i++) { echo "-- $i\n"; var_export(caller($i)); echo "\n"; }
