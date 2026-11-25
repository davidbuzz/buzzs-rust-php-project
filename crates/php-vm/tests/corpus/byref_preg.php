<?php
class P { public $u; public array $a = []; public ?array $n = null; public static $s; public static array $ts = [];
  public $arr = ['x' => null];
  function viaThis($s) { $r = preg_match('/(\d+)(\w)?/', $s, $this->u); return [$r, $this->u]; }
  function viaStatic($s) { $r = preg_match('/[a-z]+/', $s, self::$s); return [$r, self::$s]; }
  function viaElem($s) { $r = preg_match('/(.)(.)/', $s, $this->arr['x']); return [$r, $this->arr]; }
}
function local($s) { $r = preg_match('/(a)(b)?/', $s, $m); return [$r, $m]; }
function preset($s, $init) { $m = $init; $r = preg_match('/z(\d)/', $s, $m, PREG_OFFSET_CAPTURE); return [$r, $m]; }
function viaGlobal($s) { global $gm; $r = preg_match('/./', $s, $gm); return $r; }
function viaStatic2($s) { static $sm; $old = $sm; preg_match('/\w+/', $s, $sm); return [$old, $sm]; }
function viaRef($s) { $t = 'keep'; $q = &$t; preg_match('/x/', $s, $q); return $t; }
function nested($s) { $a = []; preg_match_all('/(\d)(\w)/', $s, $a['k'][1], PREG_SET_ORDER); return $a; }
function counts($s) { $n = 'x'; $r = str_replace(['a', 'b'], 'c', $s, $n); $r2 = str_ireplace('A', 'q', $s, $c2);
  $r3 = preg_replace('/\d/', '#', $s, -1, $c3); return [$r, $n, $r2, $c2, $r3, $c3]; }
$p = new P;
foreach (['a1b', 'z9', '', 'ab12c', 'xx', 'ab12c'] as $i => $s) {
  echo "-- $i\n";
  var_export($p->viaThis($s)); echo "\n";
  var_export($p->viaStatic($s)); echo "\n";
  var_export($p->viaElem($s)); echo "\n";
  var_export(local($s)); echo "\n";
  var_export(preset($s, $i % 2 ? 'str' : [1])); echo "\n";
  var_export(viaGlobal($s)); var_export($gm); echo "\n";
  var_export(viaStatic2($s)); echo "\n";
  var_export(viaRef($s)); echo "\n";
  var_export(nested($s)); echo "\n";
  var_export(counts($s)); echo "\n";
}
var_dump($p);
