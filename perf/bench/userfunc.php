<?php
$n = (int)($argv[1] ?? 1000000);
function g($x) { return $x; }
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { g(1); }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'userfunc', $e / $n);
