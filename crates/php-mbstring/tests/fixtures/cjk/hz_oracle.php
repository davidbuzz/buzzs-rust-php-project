<?php
// php hz_oracle.php > hz_oracle.tsv
require __DIR__ . '/gb_oracle_lib.php';
mt_srand(1843);

$uni = [[0x20, 0x7E], [0x80, 0x4FF], [0x2000, 0x33FF], [0x4E00, 0x9FFF], [0xFF00, 0xFFFF]];
rand_valid('HZ', $uni, 80, 1, 20);
rand_valid('HZ', $uni, 4, 130, 300);
rand_encode('HZ', $uni, 100);
rand_bytes('HZ', [[0, 0x7F], [0x80, 0xFF], [0x21, 0x77], [0x7B, 0x7E], [0x7E, 0x7E], [0x0A, 0x0A]], 300);
foreach (['~~', "~\n", '~{~}', "~{~\n~}", '~~?', '~A', "\x80", "~{\x22\x21", '~', '~{', '~{!',
          '~{!!', '~{!!~', '~{!!~}', "~{!\x80", '~}', '~{~{', '~{~~', "~{\x7F\x7F~}", '~{!,~}'] as $s) {
    foreach ([0x25, 'long', 'none'] as $m) {
        xdec('HZ', $s, $m);
    }
    check('HZ', $s);
}
xenc('HZ', [0x7AFA, 0]);
xenc('HZ', [0x7E, 0x4E00, 0x7E, 0x4E00, 0x41, 0x2015, 0x30FB, 0x2225, 0xFF04, 0xFFE5, 0xB7]);
