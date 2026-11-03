<?php
$n = (int)($argv[1] ?? 1000000);
$s = 0;
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { if ($i < 10) { $s = 1; } }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'is_smaller_jmp', $e / $n);
