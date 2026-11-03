<?php
// Oracle fixtures for the Shift-JIS family (sjis.rs).
// Regenerate: php sjis_oracle.php > sjis_oracle.tsv
require __DIR__ . '/oracle.php';
mt_srand(20261103);

$encs = ['SJIS', 'SJIS-mac', 'SJIS-Mobile#DOCOMO', 'SJIS-Mobile#KDDI', 'SJIS-Mobile#SOFTBANK', 'CP932', 'SJIS-win'];

function cps_range(int $lo, int $hi): array {
    $r = [];
    for ($c = $lo; $c < $hi; $c++) {
        if ($c >= 0xD800 && $c <= 0xDFFF) continue;
        $r[] = $c;
    }
    return $r;
}

function enc_chunks(string $enc, array $cps, $mode = 0x3F, int $n = 256): void {
    foreach (array_chunk($cps, $n) as $chunk) enc_cps($enc, $chunk, $mode);
}

foreach ($encs as $enc) {
    // Every single byte, alone (truncation) and followed by 'A'
    for ($b = 0; $b < 256; $b++) {
        dec($enc, chr($b));
        dec($enc, chr($b) . 'A');
    }
    // Every byte pair starting with a non-ASCII byte, separated by 'A'
    // (all trail bytes for the emoji/PUA lead bytes of the mobile variants,
    // a sample of interesting + random trail bytes otherwise)
    for ($lead = 0x80; $lead < 0x100; $lead++) {
        $s = '';
        if (str_starts_with($enc, 'SJIS-Mobile') && $lead >= 0xF0) {
            $trails = range(0, 255);
        } else {
            $trails = [0x00, 0x20, 0x3F, 0x40, 0x41, 0x5C, 0x7E, 0x7F, 0x80, 0x9E, 0x9F, 0xA0, 0xDF, 0xE0, 0xFC, 0xFD, 0xFE, 0xFF];
            for ($i = 0; $i < 12; $i++) $trails[] = mt_rand(0x40, 0xFC);
        }
        foreach ($trails as $t) $s .= chr($lead) . chr($t) . 'A';
        dec($enc, $s);
    }
    // Encoder over the interesting ranges
    $ranges = [[0, 0x500], [0x2000, 0x2700], [0x3000, 0x3400], [0xE000, 0xE800], [0xF860, 0xF863], [0xFA00, 0xFB00], [0xFE00, 0x10000],
        [0x1F000, 0x1F700], [0xFE000, 0xFF000]];
    foreach ($ranges as [$lo, $hi]) enc_chunks($enc, cps_range($lo, $hi));
    // Each code point alone at the end of the input (state flushing)
    foreach ([0x23, 0x30, 0x35, 0x2026, 0x3001, 0xF860, 0xF861, 0xF862, 0x1F1EF, 0x1F1FA, 0x1F1E8, 0x41] as $cp) {
        enc_cps($enc, [$cp]);
        enc_cps($enc, [$cp, 0x41]);
        enc_cps($enc, [$cp, 0x20E3]);
        enc_cps($enc, [$cp, 0x20E3, 0x41]);
    }
    // Keypad, flag and MacJapanese sequences
    $seqs = [
        [0x23, 0x20E3], [0x30, 0x20E3], [0x31, 0x20E3], [0x39, 0x20E3], [0x31, 0x32, 0x20E3],
        [0x1F1EF, 0x1F1F5], [0x1F1FA, 0x1F1F8], [0x1F1E8, 0x1F1F3], [0x1F1F7, 0x1F1FA], [0x1F1EC, 0x1F1E7],
        [0x1F1EF, 0x41], [0x1F1EF, 0x1F1EF], [0x1F1E9, 0x1F1EA, 0x1F1EA, 0x1F1F8],
        [0x2026, 0xF87F], [0x2026, 0xF87E], [0x2026, 0x41], [0x3001, 0xF87E], [0x5927, 0x20DD], [0x21E6, 0xF87A],
        [0xF860, 0x30, 0x2E], [0xF860, 0x30, 0x2F], [0xF860, 0x30], [0xF861, 0x46, 0x41, 0x58], [0xF861, 0x46, 0x41],
        [0xF861, 0x46, 0x41, 0x59], [0xF862, 0x58, 0x49, 0x49, 0x49], [0xF862, 0x6709, 0x9650, 0x4F1A, 0x793E],
        [0xF862, 0x6709, 0x9650, 0x4F1A], [0xF862, 0x41], [0xF860, 0x54, 0x42, 0x41], [0xF860, 0x2193, 0x2191],
        [0xA5, 0x203E, 0xAF, 0xA9, 0xAE, 0x2122, 0x5C, 0x7E, 0xA0, 0x2014, 0x2116, 0xFF3C, 0x2225, 0xFF0D, 0xFFE0, 0xFFE1, 0xFFE2, 0],
    ];
    foreach ($seqs as $seq) {
        enc_cps($enc, $seq);
        enc_cps($enc, array_merge([0x41], $seq, [0x42]));
    }
    // Error modes
    foreach (['none', 'long', 'entity', 0x3013, 0x3F, 0x2014] as $mode) {
        enc_cps($enc, [0x41, 0x1F600, 0x42, 0x10FFFF, 0x43, 0x1F1EF, 0xF862, 0x41], $mode);
        dec($enc, "\x81\x00A\xFF\xFC\xFCB\x85\x40\x80\xA0\xEF\x40", $mode);
    }
    // mb_check_encoding
    foreach (["abc", "\x82\xA0", "\x82", "\x82\xA0\x82", "\xFF", "\x80", "\xA0", "\xF0\x40", "\xFD\x40", "\x1B\$G!\x0F", "\x1B\$Gz\x0F", "\x1B\$", "\x1B\$X"] as $s) {
        check($enc, $s);
    }
}

// Random valid text: decode a random sequence of valid characters (taken from
// the encoder's output over many code points) in all the encodings
foreach ($encs as $enc) {
    $pool = [];
    for ($i = 0; $i < 3000; $i++) {
        $cp = [mt_rand(0x20, 0x7E), mt_rand(0x3000, 0x30FF), mt_rand(0x4E00, 0x9FFF), mt_rand(0xFF61, 0xFF9F),
               mt_rand(0x1F300, 0x1F64F), mt_rand(0xE000, 0xE757)][mt_rand(0, 5)];
        $u = mb_chr($cp, 'UTF-8');
        $b = mb_convert_encoding($u, $enc, 'UTF-8');
        if (mb_convert_encoding($b, 'UTF-8', $enc) === $u) $pool[] = $u;
    }
    for ($k = 0; $k < 20; $k++) {
        $s = '';
        $n = mt_rand(1, 300);
        for ($i = 0; $i < $n; $i++) $s .= $pool[mt_rand(0, count($pool) - 1)];
        conv('UTF-8', $enc, $s);
        dec($enc, mb_convert_encoding($s, $enc, 'UTF-8'));
    }
    // SoftBank escapes
    if ($enc === 'SJIS-Mobile#SOFTBANK') {
        foreach (["\x1B\$G!\"#\x0F", "\x1B\$E!z\x0Fa", "\x1B\$O!m", "\x1B\$P!l\x0F", "\x1B\$Q!^\x0F", "\x1B\$Q_\x0F",
                  "\x1B\$G{", "\x1B\$Gz", "\x1B\$X!", "\x1B\$", "\x1B", "\x1BX", "\x1B\$G" . str_repeat('!', 300) . "\x0F"] as $s) {
            dec($enc, $s);
            dec($enc, $s . 'ab');
        }
    }
}
