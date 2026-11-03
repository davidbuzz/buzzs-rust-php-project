<?php
$n = (int)($argv[1] ?? 1000000);

$t = hrtime(true);
for ($i = 0; $i < $n; $i++) {  }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'empty_loop', $e / $n);
