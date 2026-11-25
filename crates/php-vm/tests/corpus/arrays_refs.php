<?php
function shared($i) {
  $x = $i; $a = [&$x, 'v']; $b = $a; $b[0] = 'changed-through-copy'; $out = [$x, $a, $b];
  unset($x); $c = $a; $c[0] = 'after-unset'; $out[] = [$a, $c];
  $arr = [1, 2, 3]; foreach ($arr as &$v) {} $copy = $arr; foreach ($copy as $v) {} $out[] = [$arr, $copy];
  $m = ['k' => ['n' => 1]]; $r = &$m['k']['n']; $m2 = $m; $m2['k']['n'] = 99; $out[] = [$m, $m2]; unset($r);
  return $out;
}
function funcs($i) {
  $x = 10; $y = 20; $a = ['x' => &$x, 'y' => &$y, 'z' => $i];
  $out = [];
  $out[] = array_map(fn($v) => $v * 2, $a); $out[] = array_values($a); $out[] = array_merge($a, ['w' => 1]);
  $out[] = array_slice($a, 1); $out[] = array_reverse($a); $out[] = array_filter($a); $out[] = array_keys($a, 20);
  $out[] = count($a, COUNT_RECURSIVE); $out[] = array_sum($a); $out[] = json_encode($a); $out[] = serialize($a);
  $s = $a; sort($s); $s[0] = 'sorted-copy'; $out[] = [$x, $y, $s];
  array_walk($a, function (&$v) { $v = is_int($v) ? $v + 1 : $v; }); $out[] = [$x, $y, $a];
  $self = [1]; $self[] = &$self; $out[] = count($self); unset($self);
  $lit = [1, [2, 3]]; [$p, [$q, &$rr]] = $lit; $rr = 'r'; $out[] = [$p, $q, $rr, $lit];
  $src = [5, 6]; [&$f0, $f1] = $src; $f0 = 'via list ref'; $out[] = $src;
  $nest = ['a' => ['b' => 1]]; ['a' => ['b' => &$deep]] = $nest; $deep = 'deep'; $out[] = $nest;
  foreach ([[1, 2], [3, 4]] as $k => [$l, $mm]) { $out[] = "$k:$l:$mm"; }
  $obj = new ArrayObject([1, 2]); $out[] = iterator_to_array($obj); $obj[] = $i; $out[] = count($obj);
  return $out;
}
function coalesce($i) {
  $calls = 0; $key = function () use (&$calls) { $calls++; return 'k'; };
  $a = []; $a[$key()] ??= 'set'; $a[$key()] ??= 'not'; $a[$key() . '2'][] = $i; $o = new stdClass; $o->{$key()} ??= [$i];
  $o->{$key()}[] = 1; $n = null; $n ??= $i; $s = ['x' => null]; $s['x'] ??= 'filled';
  return [$calls, $a, $o, $n, $s];
}
for ($i = 0; $i < 4; $i++) { var_export(shared($i)); echo "\n"; var_export(funcs($i)); echo "\n"; var_export(coalesce($i)); echo "\n"; }
