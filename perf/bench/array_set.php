<?php
$n = (int)($argv[1] ?? 1000000);
$a = [];
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { $a[$i] = $i; }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'array_set', $e / $n);
