<?php
$n = (int)($argv[1] ?? 1000000);
function g(int $x): int { return $x; }
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { g($i); }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'userfunc_typed', $e / $n);
