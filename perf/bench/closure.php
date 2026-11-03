<?php
$n = (int)($argv[1] ?? 1000000);
$f = fn($x) => $x;
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { $f(1); }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'closure', $e / $n);
