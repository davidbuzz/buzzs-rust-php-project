<?php
// Generates cp5022x_oracle.tsv: php tests/fixtures/cjk/cp5022x_oracle.php > tests/fixtures/cjk/cp5022x_oracle.tsv
require __DIR__ . '/oracle.php';
mt_srand(5022);

$encs = ['CP50220', 'CP50221', 'CP50222'];
$modes = [0x3F, 'none', 'long', 'entity', 0x3013];

// ---- Decoding ----
$dec = [
    "abc\x00\x7F",
    "\x1B\$B\x30\x21\x1B(B", "\x1B\$@\x30\x21", "\x1B\$(B\x30\x21\x1B(H", "\x1B\$(@\x30\x21",
    "\x1B\$(D\x22\x2F\x1B(B", "\x1B\$(D\x7E\x7E", "\x1B\$(D\x21\x21",
    "\x1B(I\x21\x5F\x60\x20\x1B(B", "\x0E\x21\x31\x5F\x0Fabc", "\x0E\x60\x7E\x0F",
    "\x1B(J\x5C\x7E\x41\x1B(B\x5C\x7E",
    "\x1B\$B\x2D\x21\x2D\x7C\x79\x21\x7C\x7E\x1B(B", // NEC & IBM ext
    "\x1B\$B\x7F\x21\x75\x21\x7E\x7E\x80\x21\x97\x7E\x1B(B", // UDA etc
    "\x1B\$B\x22\x2F\x29\x21\x2F\x7E\x74\x27\x1B(B", // unmapped
    "\x1B\$B\x30\x20\x30\x7F\x30\x80\x30\xFF",
    "\xA1\xDF\xA0\xE0\x80\xFF\x8E",
    "\x1B", "\x1B\$", "\x1B(", "\x1B\$(", "\x1BX", "\x1B\$X", "\x1B(X", "\x1B\$(X",
    "a\x1B", "a\x1B\$", "a\x1B(", "a\x1B\$(", "\x1B\$(Xab", "\x1B\$Xab", "\x1B(Xab", "\x1BXab",
    "\x1B\$B\x30", "\x1B\$B\x30\x21\x30", "\x1B\$(D\x22", "\x1B\x1B\$B\x30\x21",
    "\x1B\$B\x1B(B", "\x1B\$B\x0E\x21\x0F\x30\x21", "\x0E\x1B\$B\x30\x21\x0F",
];
// Long strings that cross the 128-code point chunk boundary near escapes
for ($i = 120; $i < 132; $i++) {
    $dec[] = str_repeat('a', $i) . "\x1B\$(Xbc";
    $dec[] = str_repeat('a', $i) . "\x1B\$Xbc";
    $dec[] = str_repeat('a', $i) . "\x1B(Xbc";
    $dec[] = str_repeat('a', $i) . "\x1B\$B\x30\x21\x30\x22\x1B(B";
}
// Random byte strings over an interesting alphabet
$alpha = ["\x1B", '$', '(', 'B', '@', 'D', 'J', 'I', 'H', "\x0E", "\x0F", "\x21", "\x30", "\x5C", "\x7E", "\x7F", "\x80", "\x97", "\xA1", "\xDF", "\xE0", "\x20", "\x2D", "\x79", "\x7C", "\x22", 'a'];
for ($n = 0; $n < 600; $n++) {
    $s = '';
    $len = mt_rand(1, 14);
    for ($i = 0; $i < $len; $i++) $s .= $alpha[mt_rand(0, count($alpha) - 1)];
    $dec[] = $s;
}
foreach ($dec as $s) {
    foreach ($encs as $e) {
        dec($e, $s);
    }
    dec('CP50221', $s, 'long');
    dec('CP50221', $s, 'none');
    check('CP50221', $s);
}

// ---- Encoding ----
$cps = [
    [0x61, 0x62], [0], [0x61, 0, 0x62], [0x3042, 0x61], [0xFF71, 0xFF9E, 0x61],
    [0xFF76, 0xFF9E], [0xFF8A, 0xFF9F], [0xFF73, 0xFF9E], [0xFF9E], [0xFF9F, 0xFF9E], [0xFF61],
    [0xFF76], [0xFF76, 0x61], [0xFF76, 0x4E00], [0x4E00, 0xFF76, 0xFF9E, 0xFF61, 0x41],
    [0xA5, 0x203E, 0x5C, 0x7E], [0xA5, 0xFF71, 0xA5], [0xFF3C, 0x2225, 0xFF0D, 0xFFE0, 0xFFE1, 0xFFE2],
    [0x2460, 0x2170, 0x7E8A, 0x2252, 0xFFE4, 0x2116, 0xF929], // NEC/IBM ext
    [0xE000, 0xE001, 0xE757, 0xE758], // PUA
    [0x02D8, 0x4E02, 0x9FA5], // JIS X 0212 only / unmapped
    [0x1F600, 0x61], [0x80], [0x3000, 0xFF01, 0x2015, 0x2014],
    [0x00A6, 0x00A9], [0x2225, 0x2016],
];
// chunk-boundary halfwidth kana (CP50220 caches them between buffers)
for ($i = 124; $i < 132; $i++) {
    $cps[] = array_merge(array_fill(0, $i, 0x61), [0xFF76, 0xFF9E, 0xFF8A, 0xFF9F, 0x62]);
    $cps[] = array_merge(array_fill(0, $i, 0x61), [0xFF76, 0x1F600, 0xFF76]);
}
$pool = [0x61, 0x5C, 0x7E, 0xA5, 0x203E, 0x3042, 0x4E00, 0xFF61, 0xFF71, 0xFF76, 0xFF73, 0xFF8A, 0xFF9E, 0xFF9F,
         0x2460, 0xE000, 0x02D8, 0x1F600, 0xFF3C, 0, 0x30AB, 0x309B];
for ($n = 0; $n < 600; $n++) {
    $c = [];
    $len = mt_rand(1, 10);
    for ($i = 0; $i < $len; $i++) $c[] = $pool[mt_rand(0, count($pool) - 1)];
    $cps[] = $c;
}
foreach ($cps as $c) {
    foreach ($encs as $e) {
        foreach ($modes as $m) {
            enc_cps($e, $c, $m);
        }
    }
}
