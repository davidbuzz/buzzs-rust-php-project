<?php
$n = (int)($argv[1] ?? 1000000);

$s = 'x';
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { strlen($s); }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'strlen', $e / $n);
