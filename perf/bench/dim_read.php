<?php
$n = (int)($argv[1] ?? 1000000);
$a = range(0, 1023); $s = 0;
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { $s = $a[$i & 1023]; }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'dim_read', $e / $n);
