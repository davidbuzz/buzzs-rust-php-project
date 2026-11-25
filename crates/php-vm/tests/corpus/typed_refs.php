<?php
class T { public int $i = 0; public ?string $s = null; public float $f = 0.0; public array $a = [1, 2]; public static int $si = 0; public int|bool $ib = 0; }
function run($v) {
  $t = new T; $out = [];
  $r = &$t->i; try { $r = $v; } catch (TypeError $e) { $out[] = $e->getMessage(); } $out[] = $t->i;
  try { $r .= '1'; } catch (TypeError $e) { $out[] = $e->getMessage(); } $out[] = $t->i;
  $t->i = PHP_INT_MAX; try { $t->i++; } catch (Error $e) { $out[] = get_class($e) . ': ' . $e->getMessage(); } $out[] = $t->i;
  try { $r++; } catch (Error $e) { $out[] = get_class($e) . ': ' . $e->getMessage(); }
  $t->i = PHP_INT_MIN; try { --$t->i; } catch (Error $e) { $out[] = $e->getMessage(); } $out[] = $t->i;
  $rs = &$t->s; try { $rs = $v; } catch (TypeError $e) { $out[] = $e->getMessage(); } $out[] = $t->s;
  $rf = &$t->f; $rf = 5; $out[] = $t->f; try { $rf = 'x'; } catch (TypeError $e) { $out[] = $e->getMessage(); }
  $both = &$t->i; $both2 = &$t->f; $out[] = [$t->i, $t->f];
  try { [$t->i, $t->s] = [$v, $v]; } catch (TypeError $e) { $out[] = $e->getMessage(); } $out[] = [$t->i, $t->s];
  try { foreach ([$v, 2] as $t->i) {} } catch (TypeError $e) { $out[] = $e->getMessage(); } $out[] = $t->i;
  foreach ($t->a as &$el) { $el = "x$el"; } unset($el); $out[] = $t->a;
  $rsi = &T::$si; try { $rsi = $v; } catch (TypeError $e) { $out[] = $e->getMessage(); } $out[] = T::$si;
  try { T::$si += 1.5; } catch (TypeError $e) { $out[] = $e->getMessage(); } $out[] = T::$si;
  try { $t->ib = $v; } catch (TypeError $e) { $out[] = $e->getMessage(); } $out[] = $t->ib;
  $arr = [&$t->i]; try { $arr[0] = 'str'; } catch (TypeError $e) { $out[] = $e->getMessage(); } $out[] = $t->i;
  return $out;
}
set_error_handler(function ($no, $msg, $f, $l) { echo "E[$no] $msg @$l\n"; return true; });
foreach ([5, '7', 2.0, 2.5, 'abc', true, null, '1e2'] as $v) { for ($k = 0; $k < 2; $k++) { var_export(run($v)); echo "\n"; } }
