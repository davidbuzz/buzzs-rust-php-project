<?php
$n = (int)($argv[1] ?? 1000000);
class C { function get($x) { return $x; } } $o = new C;
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { $o->get(1); }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'method', $e / $n);
