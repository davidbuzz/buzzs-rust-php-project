<?php
set_error_handler(function ($no, $msg, $f, $l) { echo "$msg @$l\n"; return true; });
class K { function __construct($a, $b) {} static function s($x) { return $x; } }
function g(...$a) { return $a; }
function f($n, $arr, $str) {
  $a = $n
    + $u1;
  $b = $u2 ?
    1 :
    $u3;
  $c = [
    'k' => $u4,
    $u5,
  ];
  $d = g(
    $u6,
    $u7
  );
  $e = new K(
    $u8,
    2
  );
  $f = "a
    $u9 b
    {$arr['zz']}";
  $h = <<<EOT
    x $u10
    y {$arr['yy']}
    EOT;
  $i = match (true) {
    default => $u11,
  };
  echo $u12,
    $u13;
  foreach ($u14
    as $v) {}
  $j = K::s(
    $u15
  );
  $k = $str
    [10];
  $l = -$u16
    * 2;
  $m = (int)
    $u17;
  $o = $arr
    ['nope']
    ?? 'x';
  $p = fn() =>
    $u18;
  $p();
  list(
    $q1,
    $q2
  ) = [1];
  return
    $u19;
}
for ($i = 0; $i < 3; $i++) { f(1, [], 'ab'); }
