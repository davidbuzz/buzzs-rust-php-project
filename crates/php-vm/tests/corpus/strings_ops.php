<?php
set_error_handler(function ($no, $msg, $f, $l) { echo "E[$no] $msg @$l\n"; return true; });
function offsets($s) {
  $r = [];
  foreach ([0, -1, 2, 10, -10, '1', '1.0', 'x', 1.5, true, null] as $o) {
    try { $r[] = $s[$o]; } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
    $r[] = $s[$o] ?? 'dflt'; $r[] = isset($s[$o]);
  }
  $w = $s;
  foreach ([[0, 'xy'], [-1, 'Z'], [6, '!'], [-20, 'q'], [1, ''], ['a', 'b']] as [$o, $v]) {
    try { $w[$o] = $v; $r[] = $w; } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  }
  return $r;
}
function incs($i) {
  $out = [];
  foreach (['a', 'Az', 'zz', 'a9', 'Zz', '9', '9.5', ' 1', '1 ', '-', '1e2', 'z'] as $s) { $t = $s; $t++; $out[] = $t; }
  foreach (['a', '5', '', '1.5'] as $s) { $t = $s; $t--; $out[] = $t; }
  $e = ''; $e++; $out[] = $e; $n = null; $n++; $out[] = $n; $m = null; $m--; $out[] = $m;
  return $out;
}
function arith($v) {
  $r = [];
  foreach ([' 1', '1 ', ' 1 ', '1abc', '1e3', '0x1A', '.5', '5.', "1\n", '', 'abc'] as $s) {
    try { $r[] = $s + $v; } catch (TypeError $e) { $r[] = $e->getMessage(); }
    try { $r[] = $v * $s; } catch (TypeError $e) { $r[] = $e->getMessage(); }
    $r[] = $s == $v; $r[] = $s <=> $v; $r[] = $s == '1';
  }
  return $r;
}
function concat($f) {
  $x = 'k'; $arr = ['a' => ['b' => 'AB'], 3 => 'three']; $o = new stdClass; $o->p = new stdClass; $o->p->q = 'Q'; $o->arr = [1 => 'one'];
  return [$f . '', 0.1 + 0.2 . '', "$f", 1e100 . '', -0.0 . '', 1/3 . '|' . $f * 3, "{$arr['a']['b']} $arr[a] $arr[3] {$o->p->q} $o->arr ${x} {$x}s $o->p->q", "$o->arr[1] {$o->arr[1]}"];
}
foreach (['hello', '', 'é'] as $s) { echo "-- $s\n"; var_export(offsets($s)); echo "\n"; }
for ($i = 0; $i < 3; $i++) { var_export(incs($i)); echo "\n"; }
foreach ([1, 1.5, '1', 0] as $v) { var_export(arith($v)); echo "\n"; }
foreach ([0.1, 1.0, 1e15, 1e16, -1.5e-7, 7.0E-10, 100000000000000000000.0, NAN, INF] as $f) { var_export(concat($f)); echo "\n"; }
