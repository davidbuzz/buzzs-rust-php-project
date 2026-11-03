<?php
// Integer arithmetic in a tight loop: CV reads/writes, ADD/MUL/MOD, IS_SMALLER, PRE_INC.
$s = 0;
for ($i = 0; $i < 5_000_000; $i++) {
    $s += $i * 2 % 7;
}
echo $s, "\n";
