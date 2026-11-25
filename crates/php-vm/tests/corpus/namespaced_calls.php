<?php
namespace NS;
function f($i) { return [strlen("ab$i"), abs(-$i), trim(" x$i "), substr("abc$i", 1), \strtoupper("q"), strtoupper("q$i"), in_array($i, [1, 2]), is_numeric("5"), count([1, $i]), defined('X')]; }
function g($i) { return strrev("ab$i"); }
for ($i = 0; $i < 3; $i++) {
  echo json_encode(f($i)), json_encode(g($i)), "\n";
  if ($i == 0) { eval('namespace NS; function strlen($s) { return "ns-strlen"; } function abs($x) { return "ns-abs"; } function trim($s) { return "ns-trim"; } function strrev($s) { return "ns-strrev"; } function strtoupper($s) { return "ns-upper"; } function count($a) { return "ns-count"; } function substr($s, $o) { return "ns-substr"; } function in_array($a, $b) { return "ns-in"; } function is_numeric($s) { return "ns-isnum"; } function defined($c) { return "ns-def"; }'); }
}
function h($i) { return [strlen("x"), abs(-1), trim(" y "), strrev("ab"), strtoupper("z"), count([]), substr("abc", 1), in_array(1, [1]), is_numeric("1"), defined('X')]; }
for ($i = 0; $i < 3; $i++) { echo json_encode(h($i)), "\n"; }
