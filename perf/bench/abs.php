<?php
$n = (int)($argv[1] ?? 1000000);
$x = -3;
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { abs($x); }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'abs', $e / $n);
