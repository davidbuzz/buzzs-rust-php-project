<?php
$n = (int)($argv[1] ?? 1000000);

$s = 'ab';
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { str_repeat($s, 10); }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'str_repeat', $e / $n);
