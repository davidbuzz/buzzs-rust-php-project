<?php
$v = [1, 1.5, "x\0y", true, false, null, ["a" => [], "b'\\" => "q'\\"], -0.0, 1e100, NAN, INF, -INF, PHP_INT_MIN, 0.1+0.2, 1e15, "07" => 7, -3 => "neg"];
var_dump($v); echo "--\n"; echo print_r($v, true); echo "--\n"; var_export($v); echo "\n--\n";
$x = 1; $r = [&$x, 2]; var_dump($r);
$a = []; $a[] = &$a; $a["k"] = 1; var_dump($a); echo print_r($a, true), "\n"; var_export($a); echo "\n";
