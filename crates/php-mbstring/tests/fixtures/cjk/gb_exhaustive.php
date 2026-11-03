<?php
// php gb_exhaustive.php > gb_exhaustive.tsv
require __DIR__ . '/gb_exhaustive_lib.php';
foreach (['GB18030', 'CP936'] as $enc) {
    for ($b1 = 0x80; $b1 <= 0xFF; $b1++) {
        ex_dec($enc, '', 'A', $b1);
    }
    ex_bmp_enc($enc);
}
foreach (array_merge(range(0x81, 0x84), range(0x90, 0xE3)) as $b1) {
    for ($b2 = 0x30; $b2 <= 0x39; $b2++) {
        ex_dec4('GB18030', $b1, $b2);
    }
}
for ($start = 0x10000; $start < 0x110000; $start += 4096) {
    ex_enc('GB18030', $start, 4096);
}
