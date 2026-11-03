<?php
// Oracle fixtures for the mobile UTF-8 variants (utf8_mobile.rs).
// Regenerate: php utf8mobile_oracle.php > utf8mobile_oracle.tsv
require __DIR__ . '/oracle.php';
mt_srand(20261104);

$encs = ['UTF-8-Mobile#DOCOMO', 'UTF-8-Mobile#KDDI-A', 'UTF-8-Mobile#KDDI-B', 'UTF-8-Mobile#SOFTBANK'];

foreach ($encs as $enc) {
    // PUA emoji code points (the carriers' private encodings) -> Unicode
    foreach ([[0xE000, 0xE800], [0xE800, 0xF100]] as [$lo, $hi]) {
        for ($c = $lo; $c < $hi; $c += 256) {
            $s = '';
            for ($k = $c; $k < min($c + 256, $hi); $k++) $s .= mb_chr($k, 'UTF-8') . 'A';
            dec($enc, $s);
        }
    }
    // Unicode -> carrier PUA
    foreach ([[0x20, 0x100], [0x2000, 0x2800], [0x2900, 0x2C00], [0x3000, 0x3300], [0x1F000, 0x1F700], [0xFE4C0, 0xFEE80]] as [$lo, $hi]) {
        $cps = range($lo, $hi - 1);
        foreach (array_chunk($cps, 256) as $chunk) enc_cps($enc, $chunk);
    }
    $seqs = [
        [0x23, 0x20E3], [0x30, 0x20E3], [0x31, 0x20E3], [0x39, 0x20E3], [0x31, 0x32, 0x20E3], [0x23], [0x31],
        [0x1F1EF, 0x1F1F5], [0x1F1FA, 0x1F1F8], [0x1F1E8, 0x1F1F3], [0x1F1F7, 0x1F1FA], [0x1F1EC, 0x1F1E7],
        [0x1F1EF, 0x41], [0x1F1EF], [0x1F1E9, 0x1F1EA, 0x1F1EA, 0x1F1F8], [0xA9, 0xAE, 0xE63E, 0xE468],
    ];
    foreach ($seqs as $seq) {
        enc_cps($enc, $seq);
        enc_cps($enc, array_merge([0x41], $seq, [0x42]));
    }
    // Invalid UTF-8, truncation; and error modes
    $bad = ["\x80", "\xC2", "\xC2A", "\xE0\x80\x80", "\xE0\xA0", "\xE3\x81", "\xED\xA0\x80", "\xEF\xBF", "\xF0\x9F\x98",
            "\xF0\x80\x80\x80", "\xF4\x90\x80\x80", "\xF5", "\xFF", "\xF1\x80\x80", "\xF0\x9F\x98A", "\xE3\x81A", "\xF0\x9F" ];
    foreach ($bad as $b) {
        dec($enc, $b);
        dec($enc, $b . "z");
        check($enc, $b);
    }
    foreach (['none', 'long', 'entity', 0x3013] as $mode) {
        dec($enc, "a\xFFb\xE3\x81", $mode);
    }
    foreach (["abc", "\xEE\x98\xBE", "\xE3\x81\x82"] as $s) check($enc, $s);
    // Random strings mixing ASCII, kana, kanji, emoji, keypad and flags
    for ($k = 0; $k < 20; $k++) {
        $cps = [];
        $n = mt_rand(1, 300);
        for ($i = 0; $i < $n; $i++) {
            switch (mt_rand(0, 6)) {
                case 0: $cps[] = mt_rand(0x20, 0x7E); break;
                case 1: $cps[] = mt_rand(0x3041, 0x3093); break;
                case 2: $cps[] = mt_rand(0x4E00, 0x9FFF); break;
                case 3: $cps[] = mt_rand(0x1F300, 0x1F64F); break;
                case 4: $cps[] = mt_rand(0x30, 0x39); $cps[] = 0x20E3; break;
                case 5: $cps[] = 0x1F1EF; $cps[] = 0x1F1F5; break;
                case 6: $cps[] = mt_rand(0xE000, 0xF0FF); break;
            }
        }
        enc_cps($enc, $cps);
        $u = '';
        foreach ($cps as $cp) $u .= mb_chr($cp, 'UTF-8');
        dec($enc, mb_convert_encoding($u, $enc, 'UTF-8'));
    }
}
