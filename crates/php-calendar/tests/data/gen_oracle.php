<?php
// Generates calendar_oracle.txt from the C implementation:
//   /usr/bin/php8.5 -d memory_limit=1G gen_oracle.php > calendar_oracle.txt
// Format: func|arg,arg,...|result   (strings hex-encoded with "h:" prefix,
// ints as "i:", null "n", errors "E:<message>").
mt_srand(12345);
function enc($v) {
    if (is_int($v)) return "i:$v";
    if (is_null($v)) return "n";
    if (is_string($v)) return preg_match("/^[ -~]*$/", $v) && strpbrk($v, "|;") === false ? "s:$v" : "h:" . bin2hex($v);
    if (is_array($v)) { $o = []; foreach ($v as $k => $x) $o[] = "$k=" . enc($x); return "a:" . implode(";", $o); }
    if ($v === false) return "f";
    return "?";
}
function call($f, ...$args) {
    try { $r = enc($f(...$args)); } catch (Throwable $e) { $r = "E:" . $e->getMessage(); }
    echo $f, "|", implode(",", array_map(fn($a) => is_bool($a) ? (int)$a : $a, $args)), "|", $r, "\n";
}
$jds = [];
for ($i = -5; $i < 400; $i++) $jds[] = $i;
for ($i = 0; $i < 1200; $i++) $jds[] = mt_rand(0, 3000000);
for ($i = 0; $i < 300; $i++) $jds[] = mt_rand(2375800, 2381000);
for ($i = 0; $i < 300; $i++) $jds[] = mt_rand(347990, 400000);
for ($i = 0; $i < 300; $i++) $jds[] = mt_rand(3000000, 324542846);
foreach ([324542846, 324542847, 536838866, 536838867, 2440588, 2440587, 0, -1, 1] as $x) $jds[] = $x;
foreach ($jds as $jd) {
    call('jdtogregorian', $jd);
    call('jdtojulian', $jd);
    call('jdtojewish', $jd);
    call('jdtofrench', $jd);
    call('jddayofweek', $jd, mt_rand(-1, 3));
    call('jdmonthname', $jd, mt_rand(-1, 6));
    call('cal_from_jd', $jd, mt_rand(0, 3));
}
for ($i = 0; $i < 1500; $i++) {
    $jd = mt_rand(347998, 3000000);
    call('jdtojewish', $jd, true, mt_rand(0, 15));
}
for ($y = 1; $y < 10000; $y += mt_rand(1, 40)) {
    call('jdtojewish', jewishtojd(1, mt_rand(1, 30), $y), true, [0, 2, 4, 6, 8, 10, 12, 14][mt_rand(0, 7)]);
}
for ($i = 0; $i < 2000; $i++) {
    $y = mt_rand(-5000, 10000); $m = mt_rand(-1, 14); $d = mt_rand(-1, 32);
    call('gregoriantojd', $m, $d, $y);
    call('juliantojd', $m, $d, $y);
    call('jewishtojd', $m, $d, mt_rand(-10, 10000));
    call('frenchtojd', $m, $d, mt_rand(-1, 16));
    call('cal_to_jd', mt_rand(-1, 4), max(1, $m), $d, $y);
    call('cal_days_in_month', mt_rand(-1, 4), mt_rand(1, 14), mt_rand(-5000, 10000));
}
for ($y = 1; $y <= 15; $y++) for ($m = 1; $m <= 13; $m++) call('cal_days_in_month', CAL_FRENCH, $m, $y);
for ($y = 5700; $y <= 5800; $y++) for ($m = 1; $m <= 13; $m++) call('cal_days_in_month', CAL_JEWISH, $m, $y);
for ($y = -10; $y <= 10; $y++) for ($m = 1; $m <= 12; $m++) { call('cal_days_in_month', CAL_GREGORIAN, $m, $y); call('cal_days_in_month', CAL_JULIAN, $m, $y); }
for ($y = 1; $y <= 4000; $y++) call('easter_days', $y, $y % 5 - 1);
for ($i = 0; $i < 2000; $i++) call('easter_days', mt_rand(1, PHP_INT_MAX >> 4), mt_rand(0, 3));
for ($i = 0; $i < 2000; $i++) call('jdtounix', mt_rand(2440588, 2500000));
for ($i = -1; $i <= 4; $i++) call('cal_info', $i);
