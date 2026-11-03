<?php
$n = (int)($argv[1] ?? 300000);

$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { preg_match('/a(b+)c/', 'xxabbbcyy', $m); }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'preg_match', $e / $n);
