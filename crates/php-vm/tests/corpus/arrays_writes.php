<?php
set_error_handler(function ($no, $msg, $f, $l) { echo "E[$no] $msg @$l\n"; return true; });
class H { public $p; public array $ta = []; public static $s; }
function create($seed) {
  $out = [];
  $a = null; $a[] = 1; $out[] = $a;
  $b = $seed; try { $b['k'][1] = 2; } catch (Error $e) { $out[] = $e->getMessage(); } $out[] = $b;
  $u[] = 'u'; $out[] = $u;
  $n[5][6][7] = 'deep'; $out[] = $n;
  $h = new H; $h->p['x'][] = 1; $h->ta['a']['b'] = 2; H::$s[3][] = 4; $out[] = [$h, H::$s];
  return $out;
}
function keys($v) {
  $a = [];
  foreach (['5', '05', '-0', '5.0', '-5', ' 5', '9223372036854775808', 1.7, true, false, null, -3, '0x1'] as $k) {
    try { $a[$k] = gettype($k); } catch (Error $e) { echo $e->getMessage(), "\n"; }
  }
  $a[$v] = 'var'; $a[] = 'app';
  $m = [-5 => 'a']; $m[] = 'b'; $m2 = [-10 => 1, -20 => 2]; $m2[] = 3;
  $x = []; $x[1][$v][] = 'n'; $x[1][$v]['q'] ??= 'qq'; $x[1][$v]['q'] ??= 'zz';
  return [$a, $m, $m2, $x];
}
function cow($in) {
  $a = $in; $b = $a; $b[] = 'b'; $c = pass($a); $c['new'] = 1; $d = &$a['ref']; $d = 'via ref'; $e = $a; $e['ref'] = 'e';
  return [$in, $a, $b, $c, $e];
}
function pass($x) { return $x; }
function loops($n) {
  $a = range(1, $n); $seen = [];
  foreach ($a as $k => $v) { if ($k == 1) { unset($a[2]); $a[] = 'added'; } $seen[] = $v; }
  foreach ($a as $k => &$v) { if ($k === 0) { $a[] = 'late'; } $v = is_int($v) ? $v * 10 : $v; } unset($v);
  $pairs = [[1, [2, 3]], [4, [5, 6]]]; $l = [];
  foreach ($pairs as [$p, [$q, $r]]) { $l[] = $p + $q + $r; }
  foreach ($pairs as ['0' => $p]) { $l[] = $p; }
  $w = ['a' => 1, 'b' => 2]; foreach ($w as $k => $v) { unset($w[$k]); $w[$k . $k] = $v; }
  return [$a, $seen, $l, $w];
}
foreach ([null, '', false, 'str', 0, [1]] as $i => $seed) {
  echo "-- $i\n";
  var_export(create($seed)); echo "\n";
  var_export(keys($i)); echo "\n";
  var_export(cow(['k' => $i])); echo "\n";
  var_export(loops($i + 2)); echo "\n";
}
