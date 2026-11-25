<?php
set_error_handler(function ($no, $msg, $f, $l) { echo "E[$no] $msg @$l\n"; return true; });
class Cnt { static $made = 0; public $id; function __construct() { $this->id = ++self::$made; } }
function freshDefault($o = new Cnt, array $a = [1, 2]) { $a[] = $o->id; return $a; }
function refParam(&$r) { $r = is_array($r) ? count($r) : "set:" . gettype($r); return $r; }
function valParam($v) { $v = 'local'; return $v; }
function retArr() { return [3, 2, 1]; }
function &retRef() { static $s = [1]; return $s; }
function inc(&$x) { $x++; return $x; }
function run($i) {
  $out = [];
  $out[] = freshDefault(); $out[] = freshDefault(); $out[] = freshDefault(new Cnt, [9]);
  $out[] = refParam(retArr());
  $out[] = @end(retArr());
  $fns = ['refParam', 'valParam', 'inc', 'refParam'];
  foreach ($fns as $f) { $v = $i; $out[] = [$f, $f($v), $v]; }
  $arr = ['a' => 1]; foreach ($fns as $f) { $out[] = $f($arr['a']); } $out[] = $arr;
  $o = new stdClass; $o->p = 5; foreach ($fns as $f) { $out[] = $f($o->p); } $out[] = $o;
  foreach ($fns as $f) { $out[] = $f($undefinedVar); } $out[] = $undefinedVar;
  $r = &retRef(); $r[] = $i; $out[] = retRef();
  $out[] = inc($arr['new']); $out[] = $arr;
  try { $out[] = inc(...[$i]); } catch (Error $e) { $out[] = $e->getMessage(); }
  $args = [$i]; $out[] = inc(...$args); $out[] = $args;
  $named = ['x' => 7]; $out[] = inc(...$named); $out[] = $named;
  $cb = 'inc'; $val = 1; $out[] = call_user_func($cb, $val); $out[] = $val;
  $out[] = call_user_func_array($cb, [&$val]); $out[] = $val;
  return $out;
}
for ($i = 0; $i < 4; $i++) { echo "-- $i\n"; var_export(run($i)); echo "\n"; }
