<?php
// php big5_exhaustive.php > big5_exhaustive.tsv
require __DIR__ . '/gb_exhaustive_lib.php';
foreach (['BIG-5', 'CP950'] as $enc) {
    for ($b1 = 0x80; $b1 <= 0xFF; $b1++) {
        ex_dec($enc, '', 'A', $b1);
    }
    ex_bmp_enc($enc);
}
