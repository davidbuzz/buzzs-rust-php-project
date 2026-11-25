<?php
class Th extends Exception {}
$mode = 'return';
set_error_handler(function ($no, $msg, $f, $l) { global $mode, $G; echo "H[$mode] $msg @$l\n";
  if ($mode === 'throw') { throw new Th($msg); } if ($mode === 'mutate') { $G['mut'] = 'by handler'; } return $mode !== 'false'; });
class P { public $arr = []; public int $n = 0; public $v; }
function ops($i) {
  global $G; $out = [];
  try { $a = ['x' => 1]; $a['y'] .= 'z'; $out[] = $a; } catch (Th $e) { $out[] = ['T', $e->getMessage(), $a]; }
  try { $b = [1]; $b[$undef] = 'v'; $out[] = $b; } catch (Th $e) { $out[] = ['T', $e->getMessage(), $b]; }
  try { $c = 5; $c += $undef2; $out[] = $c; } catch (Th $e) { $out[] = ['T', $e->getMessage(), $c]; }
  try { $p = new P; $p->n = 1.5; $out[] = $p->n; } catch (Th $e) { $out[] = ['T', $e->getMessage(), $p->n]; }
  try { $p = new P; $p->arr[] = $undef3; $out[] = $p->arr; } catch (Th $e) { $out[] = ['T', $e->getMessage(), $p->arr]; }
  try { $p = new P; $p->v = $p->missing; $out[] = $p; } catch (Th $e) { $out[] = ['T', $e->getMessage(), $p]; }
  try { $G = ['k' => 1]; $G['k2'] = $G['nope'] . 'x'; $out[] = $G; } catch (Th $e) { $out[] = ['T', $e->getMessage(), $G]; }
  try { $s = 'abc'; $s[5] = 'z'; $out[] = $s; } catch (Th $e) { $out[] = ['T', $e->getMessage(), $s]; }
  try { $n = null; $r = $n['k']; $out[] = $r; } catch (Th $e) { $out[] = ['T', $e->getMessage(), isset($r)]; }
  try { $t = []; $t[1.5] = 1; $out[] = $t; } catch (Th $e) { $out[] = ['T', $e->getMessage(), $t]; }
  try { $q = [1, 2]; [$q1, $q2, $q3] = $q; $out[] = [$q1, $q2, $q3]; } catch (Th $e) { $out[] = ['T', $e->getMessage(), $q1 ?? 'u', $q3 ?? 'u']; }
  try { $z = "5 apples" + $i; $out[] = $z; } catch (Th $e) { $out[] = ['T', $e->getMessage(), isset($z)]; }
  try { $w = (int) "12abc" . strlen(null); $out[] = $w; } catch (Th $e) { $out[] = ['T', $e->getMessage()]; }
  try { $loop = []; for ($k = 0; $k < 3; $k++) { $loop[] = $loop[$k + 5] ?? $undefLoop; } $out[] = $loop; } catch (Th $e) { $out[] = ['T', $e->getMessage(), $loop]; }
  return $out;
}
foreach (['return', 'false', 'throw', 'mutate'] as $m) { $mode = $m; for ($i = 0; $i < 3; $i++) { echo "-- $m $i\n"; var_export(ops($i)); echo "\n"; } }
