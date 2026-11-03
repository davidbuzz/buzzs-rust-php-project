<?php
// Internal function call overhead with short strings and small arrays.
$s = 'Hello, World';
$a = [3, 1, 2];
$n = 0;
for ($i = 0; $i < 300_000; $i++) {
    $n += strlen($s);
    $n += count($a);
    $n += abs(-$i) & 1;
    $n += ord(substr($s, $i % 12, 1));
    $n += strpos($s, 'W');
    $n += is_int($i) ? 1 : 0;
    $n += max($i & 3, 1);
    $n += in_array(2, $a, true) ? 1 : 0;
}
echo $n, "\n";
