<?php
$n = (int)($argv[1] ?? 1000000);
$x = 0;
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { $x = $x + $i; }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'int_add', $e / $n);
