<?php
// php big5_oracle.php > big5_oracle.tsv
require __DIR__ . '/gb_oracle_lib.php';
mt_srand(950);

$uni = [[0x20, 0x7E], [0x80, 0x4FF], [0x2000, 0x33FF], [0x4E00, 0x9FFF], [0xE000, 0xF848],
        [0xF900, 0xFFFF]];
foreach (['BIG-5', 'CP950'] as $enc) {
    rand_valid($enc, $uni, 60, 1, 20);
    rand_valid($enc, $uni, 4, 130, 300);
    rand_encode($enc, $uni, 80);
    rand_bytes($enc, [[0, 0x7F], [0x80, 0xFF], [0xA1, 0xF9], [0x40, 0x7E], [0xA1, 0xFE]], 200);
}
foreach (["\x80", "\xB0\x9F", "\xA3\xED", "\xC8\x40", "\xC8\x41A", "\xC8\xA1", "\xA1", "A\xA1",
          "\xA1\x45", "\xA2\xCC", "\xF9\xFE", "\xC6\xA1", "\x81\x40", "\xFE\xFE", "\xA1\x40\xA1"] as $s) {
    foreach (['BIG-5', 'CP950'] as $enc) {
        foreach ([0x25, 'long', 'none'] as $m) {
            xdec($enc, $s, $m);
        }
        check($enc, $s);
    }
}
xenc('BIG-5', [0x7654, 0x2550, 0x255E, 0x2561, 0x256A, 0x256D, 0x256E, 0x256F, 0x2570], 'long');
xenc('CP950', [0x2609, 0xA2, 0xAF, 0xE000, 0xF848, 0xF6B1, 0x2027, 0xFF0F, 0x5341], 'long');
