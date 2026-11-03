<?php
$n = (int)($argv[1] ?? 1000000);
$a = [1,2,3];
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { count($a); }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'count', $e / $n);
