<?php
$n = (int)($argv[1] ?? 1000000);
$s = "";
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { $s .= "x"; }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'concat', $e / $n);
