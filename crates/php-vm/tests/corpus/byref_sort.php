<?php
class S { public array $a = [3, 1, 2]; public $u = ['b' => 2, 'a' => 1]; public static array $st = [5, 4]; public static $su;
  function go($i) {
    sort($this->a); rsort(self::$st); ksort($this->u);
    self::$su = ['z' => 1, 'y' => 2]; asort(self::$su);
    $this->a[] = $i; self::$st[] = $i * 3; $this->u["k$i"] = -$i;
    return [$this->a, self::$st, $this->u, self::$su];
  }
}
function family($v) {
  $a = $v; sort($a); $b = $v; rsort($b); $c = $v; usort($c, fn($x, $y) => $y <=> $x);
  $d = $v; uasort($d, fn($x, $y) => $x <=> $y); $e = $v; uksort($e, fn($x, $y) => strcmp((string)$y, (string)$x));
  $f = $v; natsort($f); $g = $v; natcasesort($g); $h = $v; arsort($h); $k = $v; krsort($k);
  $m = $v; shuffle($m); $m = count($m);
  return [$a, $b, $c, $d, $e, $f, $g, $h, $k, $m];
}
function withRefs() { $x = 3; $y = 1; $a = [&$x, &$y, 2]; sort($a); $x = 99; return [$a, $x, $y]; }
function onElem($i) { $w = ['l' => [3, $i, 1]]; sort($w['l']); $w['m'][2] = [9, 8]; rsort($w['m'][2]); return $w; }
function onUndef() { $r = @sort($undef); return [$r, $undef ?? 'unset']; }
$s = new S;
for ($i = 0; $i < 4; $i++) {
  echo "-- $i\n";
  var_export($s->go($i)); echo "\n";
  var_export(family([10, '9', 'a10', 'A2', $i, 1.5])); echo "\n";
  var_export(family(['x' => 'b', 'y' => 'B', 3 => 'a'])); echo "\n";
  var_export(withRefs()); echo "\n";
  var_export(onElem($i)); echo "\n";
  try { var_export(onUndef()); } catch (TypeError $e) { echo get_class($e), ': ', $e->getMessage(); } echo "\n";
}
