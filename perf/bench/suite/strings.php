<?php
// String building (ASSIGN_OP concat), chr(), md5, str_replace, interpolation.
$str = '';
for ($i = 0; $i < 2_000_000; $i++) {
    $str .= chr(65 + $i % 26);
}
$h = md5($str);
$len = strlen(str_replace('A', 'xx', $str));
$t = '';
for ($i = 0; $i < 500_000; $i++) {
    $t = "item $i of {$len}";
}
echo $h, ' ', $len, ' ', $t, "\n";
