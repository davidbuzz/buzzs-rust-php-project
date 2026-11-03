<?php
$n = (int)($argv[1] ?? 1000000);
class Q { public $p = 3; } $o = new Q;
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { $o->p = $i; }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'prop_write', $e / $n);
