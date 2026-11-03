<?php
$n = (int)($argv[1] ?? 1000000);
class D { static function m($x) { return $x; } }
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { D::m(1); }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'static_method', $e / $n);
