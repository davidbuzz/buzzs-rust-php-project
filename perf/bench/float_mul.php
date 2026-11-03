<?php
$n = (int)($argv[1] ?? 1000000);
$x = 1.5; $y = 0.0;
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { $y = $y + $x * 1.0001; }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'float_mul', $e / $n);
