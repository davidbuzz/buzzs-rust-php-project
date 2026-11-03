<?php
$n = (int)($argv[1] ?? 1000000);
class R { public $a; function __construct($a) { $this->a = $a; } }
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { $o = new R($i); }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'new_obj', $e / $n);
