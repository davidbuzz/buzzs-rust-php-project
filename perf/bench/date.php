<?php
$n = (int)($argv[1] ?? 300000);

$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { date('Y'); }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'date', $e / $n);
