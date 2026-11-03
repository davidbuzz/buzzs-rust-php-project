<?php
// Array element reads and writes: packed and string-keyed (hash) arrays, foreach, isset.
$n = 300_000;
$p = array_fill(0, 1000, 0);
for ($i = 0; $i < $n; $i++) {
    $p[$i % 1000] += $i;
}
$h = [];
for ($i = 0; $i < $n; $i++) {
    $k = 'k' . ($i % 5000);
    $h[$k] = ($h[$k] ?? 0) + 1;
}
$sum = 0;
foreach ($h as $k => $v) {
    if (isset($p[$v])) {
        $sum += $v;
    }
}
foreach ($p as $v) {
    $sum += $v & 0xff;
}
echo $sum, "\n";
