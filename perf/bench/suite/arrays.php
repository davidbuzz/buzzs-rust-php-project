<?php
// Packed-array append, sort, array_map with an arrow function, array_sum.
$a = [];
for ($i = 0; $i < 1_000_000; $i++) {
    $a[] = ($i * 7919) % 1000003;
}
sort($a);
$m = array_sum(array_map(fn($x) => $x & 1, $a));
echo count($a), ' ', $m, "\n";
