<?php
function sum_mixed($n, $start) {
  $s = $start; $f = 0; $m = 1;
  for ($i = 0; $i < $n; $i++) {
    $s += $i; $f = $f * 1.5 + ($i % 7); $m = ($m * 31 + $i) % 1000003;
    if ($i == 120) { $s = (string) $s; }
    if ($i == 130) { $s .= ''; $s = $s + 0.5; }
  }
  return [$s, $f, $m];
}
function overflow($n) {
  $x = PHP_INT_MAX - 100; $y = PHP_INT_MIN + 100; $z = 1;
  for ($i = 0; $i < $n; $i++) { $x++; $y--; $z = $z * 3; $z = $z > 1e300 ? 1 : $z; }
  return [$x, $y, $z];
}
function intdivs($n) {
  $a = []; $acc = 0;
  for ($i = 1; $i <= $n; $i++) {
    $acc += intdiv($i * 7, 3) + ($i << 2) - ($i >> 1) + ($i & 5) + ($i | 2) + ($i ^ 9) + (-$i % 4) + (~$i);
    if ($i % 50 == 0) { $a[] = $acc; $a[] = $i ** 2; $a[] = 2 ** -1; $a[] = fmod($i, 7.5); }
  }
  return $a;
}
function compares($n) {
  $c = [0, 0, 0, 0]; $vals = [1, '1', 1.0, '01', 'a', null, true, 2];
  for ($i = 0; $i < $n; $i++) {
    $v = $vals[$i % 8]; $w = $vals[($i * 3) % 8];
    if ($v == $w) { $c[0]++; } if ($v === $w) { $c[1]++; } if ($v < $w) { $c[2]++; } $c[3] += $v <=> $w;
  }
  return $c;
}
function floats($n) {
  $x = 0.1; $out = [];
  for ($i = 0; $i < $n; $i++) { $x = $x + 0.1; if ($x > 5) { $x = -$x; } }
  $out[] = $x; $out[] = (int) $x; $out[] = round($x, 3); $out[] = $x . '';
  $nan = NAN; $cnt = 0; for ($i = 0; $i < $n; $i++) { if ($nan == $nan) { $cnt++; } if ($nan < $i) { $cnt += 2; } }
  $out[] = $cnt;
  return $out;
}
function whiles($n) {
  $i = 0; $s = ''; $k = 0;
  while (true) { $i++; if ($i % 2) { continue; } if ($i > $n) { break; } $k += $i; }
  do { $s .= chr(65 + $i % 26); $i -= 7; } while ($i > 0);
  return [$k, strlen($s), substr($s, 0, 10)];
}
foreach ([150, 151, 300] as $n) {
  foreach ([0, '5', 2.5] as $st) { var_export(sum_mixed($n, $st)); echo "\n"; }
  var_export(overflow($n)); echo "\n";
  var_export(intdivs($n)); echo "\n";
  var_export(compares($n)); echo "\n";
  var_export(floats($n)); echo "\n";
  var_export(whiles($n)); echo "\n";
}
