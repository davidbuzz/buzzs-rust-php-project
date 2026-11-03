<?php
$n = (int)($argv[1] ?? 1000000);
$a = ["alpha" => 1, "beta" => 2, "gamma" => 3]; $s = 0;
$t = hrtime(true);
for ($i = 0; $i < $n; $i++) { $s = $a["beta"]; }
$e = hrtime(true) - $t;
printf("%-14s %10.1f ns/op\n", 'hash_read', $e / $n);
