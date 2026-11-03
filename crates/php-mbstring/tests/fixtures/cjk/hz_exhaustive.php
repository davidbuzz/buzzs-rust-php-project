<?php
// php hz_exhaustive.php > hz_exhaustive.tsv
require __DIR__ . '/gb_exhaustive_lib.php';
for ($b1 = 0; $b1 <= 0xFF; $b1++) {
    ex_dec('HZ', '~{', '', $b1);
    ex_dec('HZ', '', '', $b1);
    ex_dec('HZ', '~{', '~}', $b1);
}
ex_bmp_enc('HZ');
