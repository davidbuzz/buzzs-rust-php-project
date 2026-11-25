<?php
class Conv extends Exception {}
function h_return($no, $msg, $f, $l) { echo "R[$no] $msg @$l\n"; return true; }
function h_false($no, $msg, $f, $l) { echo "F[$no] $msg @$l\n"; return false; }
function h_throw($no, $msg, $f, $l) { throw new Conv("T: $msg @$l"); }
function work($i) {
  $out = [];
  try { $x = $undef + $i; $out[] = $x; } catch (Conv $e) { $out[] = $e->getMessage(); $out[] = isset($x); }
  try { $a = [1]; $y = $a[5]
      ?? $a[7]; $z = $a[9]; $out[] = [$y, $z]; } catch (Conv $e) { $out[] = $e->getMessage(); }
  try { $s = 'abc'; $c = $s[10]; $out[] = $c; } catch (Conv $e) { $out[] = $e->getMessage(); $out[] = isset($c); }
  try { $r = 10 % 3 + intdiv($i, 1) + (int) '12abc' + array_sum([1, '2x']); $out[] = $r; } catch (Conv $e) { $out[] = $e->getMessage(); }
  try { $o = new stdClass; $v = $o
      ->missing; $out[] = $v; } catch (Conv $e) { $out[] = $e->getMessage(); } catch (Error $e) { $out[] = 'Error ' . $e->getMessage(); }
  try { $arr = 'str'; $q = "{$arr['x']}"; } catch (Conv $e) { $out[] = $e->getMessage(); } catch (Error $e) { $out[] = get_class($e) . ' ' . $e->getMessage(); }
  try { $f = 1.5; $k = [1, 2][$f]; $out[] = $k; } catch (Conv $e) { $out[] = $e->getMessage(); }
  try { $t = []; $t[1.7] = 'f'; $out[] = $t; } catch (Conv $e) { $out[] = $e->getMessage(); $out[] = $t; }
  try { $p = null; $p[] = 1; $fa = false; $fa[] = 2; $out[] = [$p, $fa]; } catch (Conv $e) { $out[] = $e->getMessage(); $out[] = $fa; }
  try { $str = (string) [1]; $out[] = $str; } catch (Conv $e) { $out[] = $e->getMessage(); $out[] = isset($str); }
  try { $out[] = implode(',', [1, [2]]); } catch (Conv $e) { $out[] = $e->getMessage(); }
  try { $w = new Exception('outer', 0, new Exception('inner')); throw $w; } catch (Exception $e) { $out[] = $e->getPrevious()->getMessage(); }
  return $out;
}
foreach (['h_return', 'h_false', 'h_throw'] as $h) {
  set_error_handler($h);
  for ($i = 0; $i < 3; $i++) { echo "-- $h $i\n"; var_export(work($i)); echo "\n"; }
  restore_error_handler();
}
