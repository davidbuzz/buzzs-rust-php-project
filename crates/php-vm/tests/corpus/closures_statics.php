<?php
function counter() { static $n = 0; static $arr = []; $n++; $arr[] = $n; return [$n, count($arr)]; }
class S { static function m() { static $c = 10; return ++$c; } function inst() { static $i = 0; return ++$i; } }
class S2 extends S {}
function makers($i) {
  $byVal = $i; $byRef = $i;
  $f = function ($x) use ($byVal, &$byRef) { $byRef += $x; return $byVal + $x; };
  $out = [$f(1), $f(2), $byRef];
  $byVal = 100; $out[] = $f(0);
  $acc = []; $push = function ($v) use (&$acc) { $acc[] = $v; return count($acc); };
  foreach ([1, 2, 3] as $v) { $push($v * $i); } $out[] = $acc;
  $st = static function () { return isset($this); }; $out[] = $st();
  $arrow = fn($x) => $x + $i + $byVal; $byVal = -1; $out[] = $arrow(1);
  $nested = fn($a) => fn($b) => $a . $b . $i; $out[] = $nested('p')('q');
  $gen = function () use ($i) { static $calls = 0; $calls++; yield $calls; yield $i; };
  foreach ($gen() as $g) { $out[] = $g; }
  $rec = function ($n) use (&$rec) { return $n <= 1 ? 1 : $n * $rec($n - 1); }; $out[] = $rec(5 + $i);
  $out[] = counter(); $out[] = S::m(); $out[] = S2::m(); $out[] = (new S)->inst(); $out[] = (new S2)->inst();
  $bound = Closure::bind(fn() => static::class, null, S2::class); $out[] = $bound();
  $vari = function (&...$refs) { foreach ($refs as &$r) { $r .= '!'; } }; $s1 = 'a'; $s2 = 'b'; $vari($s1, $s2); $out[] = [$s1, $s2];
  $args = function ($a, $b = 'dB') { $a = 'changed'; return [func_get_args(), func_num_args()]; }; $out[] = $args('orig'); $out[] = $args('o', 'b', 'c');
  return $out;
}
for ($i = 0; $i < 4; $i++) { var_export(makers($i)); echo "\n"; }
