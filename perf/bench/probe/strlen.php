<?php
$n = 2000000;
$s = "the quick brown fox jumps over the lazy dog 0123456789";
$l = 0;
for ($i = 0; $i < $n; $i++) { $l = strlen($s); }
echo $l, "\n";
