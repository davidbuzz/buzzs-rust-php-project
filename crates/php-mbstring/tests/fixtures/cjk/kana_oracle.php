<?php
// Generates kana_oracle.tsv: php tests/fixtures/cjk/kana_oracle.php > tests/fixtures/cjk/kana_oracle.tsv
// Lines: <mode>\t<input hex (UTF-8)>\t<output hex>   (mb_convert_kana in UTF-8)
//    or: <mode>\tERR\t<ValueError message>
mt_srand(3611);

$cps = [];
for ($c = 0x20; $c <= 0x7E; $c++) $cps[] = $c;
foreach ([0xA5, 0x203E, 0x2018, 0x2019, 0x201C, 0x201D, 0x2212, 0x3000, 0x3001, 0x3002, 0x300C, 0x300D,
          0x309B, 0x309C, 0x309D, 0x309E, 0x30FB, 0x30FC, 0x30FD, 0x30FE, 0x30F5, 0x30F6, 0x30F7, 0xFFE3, 0xFFE5, 0x4E00] as $c) $cps[] = $c;
for ($c = 0xFF01; $c <= 0xFF5E; $c++) $cps[] = $c;
for ($c = 0xFF61; $c <= 0xFF9F; $c++) $cps[] = $c;
for ($c = 0x3041; $c <= 0x3096; $c++) $cps[] = $c;
for ($c = 0x30A1; $c <= 0x30FA; $c++) $cps[] = $c;

function u($cps) { $s = ''; foreach ($cps as $c) $s .= mb_chr($c, 'UTF-8'); return $s; }

$inputs = [u($cps)];
$singles = [];
// every halfwidth kana followed by each sound mark
$pairs = [];
for ($c = 0xFF61; $c <= 0xFF9F; $c++) { $pairs[] = $c; $pairs[] = 0xFF9E; $pairs[] = $c; $pairs[] = 0xFF9F; }
$inputs[] = u($pairs);
foreach ($cps as $c) $singles[] = u([$c]);
// glue across the 64-code point chunk boundary
for ($i = 58; $i < 70; $i++) {
    $inputs[] = str_repeat('a', $i) . u([0xFF76, 0xFF9E, 0xFF8A, 0xFF9F, 0x30AC]);
    $inputs[] = str_repeat('a', $i) . u([0xFF76]);
    $inputs[] = str_repeat('a', $i) . u([0x30AC, 0x3071]);
}
for ($n = 0; $n < 8; $n++) {
    $r = [];
    for ($i = 0, $l = mt_rand(1, 100); $i < $l; $i++) $r[] = $cps[mt_rand(0, count($cps) - 1)];
    $inputs[] = u($r);
}
$inputs[] = "a\xFFb\xE3\x81"; // invalid UTF-8

$modes = ['KV', 'HV', 'K', 'H', 'V', 'A', 'R', 'N', 'S', 'M', 'C', 'a', 'r', 'n', 's', 'm', 'c', 'k', 'h',
          'KVa', 'AKV', 'RNKVS', 'asrnkm', 'ASKVM', 'askhm', 'kc'.'', 'HVC', 'Kc', 'Hc', 'hm', 'Rn', 'rN', 'KVC', 'Ah', 'AH', ''];
$letters = str_split('ARNSKHMCarnskhmcV');
$base = count($modes);
for ($n = 0; $n < 80; $n++) {
    $m = '';
    for ($i = 0, $l = mt_rand(1, 5); $i < $l; $i++) $m .= $letters[mt_rand(0, 16)];
    $modes[] = $m;
}
$modes = array_merge($modes, ['X', 'Kx', 'Aa', 'Rr', 'An', 'Na', 'HK', 'hC', 'hc', 'kC', 'kc', 'Cc', 'Ss', 'Mm', 'Kk', 'Hh', 'NAa']);


foreach ($modes as $idx => $m) {
    try {
        mb_convert_kana('', $m, 'UTF-8');
    } catch (ValueError $e) {
        echo "$m\tERR\t" . $e->getMessage() . "\n";
        continue;
    }
    foreach ($idx < $base ? array_merge($inputs, $singles) : $inputs as $in) {
        echo "$m\t" . bin2hex($in) . "\t" . bin2hex(mb_convert_kana($in, $m, 'UTF-8')) . "\n";
    }
}
