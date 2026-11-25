<?php
class Acc { public array $items = []; public int $count = 0; public $total = 0; public ?string $last = null;
  function add($v) { $this->items[] = $v; $this->count++; $this->total += is_numeric($v) ? $v : 0; $this->last = (string) $v; return $this; } }
function build($n) {
  $a = []; $m = [];
  for ($i = 0; $i < $n; $i++) { $a[] = $i * 2; $m["k" . ($i % 10)][] = $i; $m["k" . ($i % 10)]['n'] = ($m["k" . ($i % 10)]['n'] ?? 0) + 1; }
  $sum = 0; foreach ($a as $k => $v) { $sum += $v - $k; }
  $refsum = 0; foreach ($a as &$v) { $v++; } unset($v); foreach ($a as $v) { $refsum += $v; }
  return [count($a), $sum, $refsum, count($m), $m['k3']['n'], array_sum($m['k7'])];
}
function objs($n) {
  $acc = new Acc;
  for ($i = 0; $i < $n; $i++) { $acc->add($i % 3 == 0 ? (string) $i : ($i % 3 == 1 ? $i : $i + 0.5)); }
  return [$acc->count, $acc->total, $acc->last, count($acc->items), $acc->items[$n - 1]];
}
function strings($n) {
  $s = ''; $parts = [];
  for ($i = 0; $i < $n; $i++) { $s .= $i % 10; if ($i % 37 == 0) { $parts[] = substr($s, -5); } $s[0] = 'X'; }
  $h = 0; for ($i = 0; $i < strlen($s); $i++) { $h = ($h * 33 + ord($s[$i])) & 0xFFFFFF; }
  return [strlen($s), $h, $parts, str_contains($s, '789')];
}
function refs($n) {
  $x = 0; $r = &$x; $arr = [0, 0]; $e = &$arr[1];
  for ($i = 0; $i < $n; $i++) { $r += 2; $e .= ''; $e = (int) $e + 1; $arr[0] = $x; }
  $copy = $arr; $copy[1] = 'c';
  return [$x, $arr, $copy];
}
function nested_break($n) {
  $hits = [];
  for ($i = 0; $i < $n; $i++) { for ($j = 0; $j < 10; $j++) { if ($j * $i > 200) { continue 2; } if ($j == 7) { break; } } $hits[] = $i; }
  return [count($hits), array_slice($hits, -3)];
}
function gen($n) { for ($i = 0; $i < $n; $i++) { $x = yield $i => $i * 2; if ($x) { $i += $x; } } return 'done'; }
function useGen($n) { $g = gen($n); $s = 0; foreach ($g as $k => $v) { $s += $k + $v; } $g2 = gen($n); $c = 0; while ($g2->valid()) { $g2->send($c++ % 2); } return [$s, $g->getReturn(), $c]; }
foreach ([150, 160, 250] as $n) {
  var_export(build($n)); echo "\n"; var_export(objs($n)); echo "\n"; var_export(strings($n)); echo "\n";
  var_export(refs($n)); echo "\n"; var_export(nested_break($n)); echo "\n"; var_export(useGen($n)); echo "\n";
}
