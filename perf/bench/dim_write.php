<?php
$n = (int)($argv[1] ?? 1000000);
$a = array_fill(0, 1024, 0);
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { $a[$i & 1023] = $i; }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'dim_write', $e / $n);
