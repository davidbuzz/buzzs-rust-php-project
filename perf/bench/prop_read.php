<?php
$n = (int)($argv[1] ?? 1000000);
class Q { public $p = 3; } $o = new Q; $s = 0;
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { $s = $o->p; }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'prop_read', $e / $n);
