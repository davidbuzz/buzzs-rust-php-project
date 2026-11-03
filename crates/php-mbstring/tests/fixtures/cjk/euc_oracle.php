<?php
// Generates euc_oracle.tsv (replayed by testutil::replay_oracle):
// targeted conversion/check cases for EUC-JP, eucJP-win, CP51932, EUC-CN,
// EUC-TW, EUC-KR, UHC and ISO-2022-KR.
// Run: php euc_oracle.php > euc_oracle.tsv
require __DIR__ . '/oracle.php';

// Named modes ("long", "entity", "none") keep the previously set substitute
// character, which the fixture format assumes to be '?': reset it first.
function named(string $mode): string {
    mb_substitute_character(0x3F);
    return $mode;
}
mt_srand(4242);

$encs = ['EUC-JP', 'eucJP-win', 'CP51932', 'EUC-CN', 'EUC-TW', 'EUC-KR', 'UHC', 'ISO-2022-KR'];

// Cases from the php-src .phpt tests
foreach (['long', 0x25, 'none', 'entity'] as $mode) {
    if (is_string($mode)) named($mode);
    foreach ($encs as $enc) {
        dec($enc, "\x80", $mode);
        dec($enc, "\xFE\xFF", $mode);
        dec($enc, "\xA1\x50", $mode);
        dec($enc, "\xA7\xF0", $mode);
        dec($enc, "\xF7\xFF", $mode);
        dec($enc, "\x8E\x20", $mode);
        dec($enc, "\x8E\xA1\x20", $mode);
        dec($enc, "\x8E\xA1\xA1\x20", $mode);
        dec($enc, "\x8E\xA2\xA3\x20", $mode);
        dec($enc, "\x8F", $mode);
        dec($enc, "\x8F\xA1", $mode);
        dec($enc, "\x8F\xA2\xB7", $mode);
        dec($enc, "\xFD\xCC", $mode);
        dec($enc, "\xE4\xA4\xB4<", $mode);
        dec($enc, "\xC9\x9E\x98", $mode);
        dec($enc, "\xC9\xA1\x41", $mode);
        dec($enc, "\xC9", $mode);
        dec($enc, "abc\xA4", $mode);
        dec($enc, "abc\xA4\xA2", $mode);
        enc_cps($enc, [0x41, 0x1F600, 0x42], $mode);
        enc_cps($enc, [0x2603, 0x5C, 0x7E, 0xA5, 0x203E, 0xAF], $mode);
    }
}

// Truncated sequences at the end of the input, and at every position of
// a few multi-byte strings
$samples = [
    'EUC-JP' => "\xA4\xA2\x8E\xB1\x8F\xB0\xA1x\xB0\xA1",
    'eucJP-win' => "\xA4\xA2\x8E\xB1\x8F\xB0\xA1\xAD\xA1\xF5\xA1\x8F\xF5\xA1\x8F\xF3\xF3",
    'CP51932' => "\xA4\xA2\x8E\xB1\xAD\xA1\xF9\xA1\xFC\xFE",
    'EUC-CN' => "\xB0\xA1\xA1\xA1x\xF7\xFE",
    'EUC-TW' => "\xC4\xA1\x8E\xA1\xC4\xA1\x8E\xA2\xA1\xA1\x8E\xAE\xA1\xA1",
    'EUC-KR' => "\xB0\xA1\xA1\xA1x\xC8\xFE",
    'UHC' => "\x81\x41\xB0\xA1\xC6\x52\xFE\xFE\xC7\xA1",
    'ISO-2022-KR' => "\x1B\$)C\x0E\x30\x21\x21\x21\x0Fab\x0E\x48\x7E\x0F",
];
foreach ($samples as $enc => $s) {
    for ($i = 0; $i <= strlen($s); $i++) {
        dec($enc, substr($s, 0, $i));
        dec($enc, substr($s, $i));
        check($enc, substr($s, 0, $i));
    }
    check($enc, $s);
    check($enc, $s . "\x80");
}

// Random valid text, long enough to cross the 128-code-point buffer boundary
$ranges = [
    'EUC-JP' => [[0x3041, 0x3093], [0x30A1, 0x30F6], [0x4E00, 0x9FA0], [0x20, 0x7E], [0xFF61, 0xFF9F]],
    'eucJP-win' => [[0x3041, 0x3093], [0x4E00, 0x9FA0], [0x2160, 0x2169], [0xE000, 0xE7FF], [0x20, 0x7E]],
    'CP51932' => [[0x3041, 0x3093], [0x4E00, 0x9FA0], [0x2160, 0x2169], [0x20, 0x7E], [0xFF61, 0xFF9F]],
    'EUC-CN' => [[0x4E00, 0x9FA0], [0x20, 0x7E], [0x3000, 0x3020]],
    'EUC-TW' => [[0x4E00, 0x9FA0], [0x20, 0x7E], [0x3000, 0x3020]],
    'EUC-KR' => [[0xAC00, 0xD7A3], [0x4E00, 0x9FA0], [0x20, 0x7E]],
    'UHC' => [[0xAC00, 0xD7A3], [0x4E00, 0x9FA0], [0x20, 0x7E]],
    'ISO-2022-KR' => [[0xAC00, 0xD7A3], [0x20, 0x7E], [0x4E00, 0x9FA0]],
];
foreach ($ranges as $enc => $rs) {
    for ($n = 0; $n < 6; $n++) {
        $cps = [];
        $len = mt_rand(100, 400);
        for ($i = 0; $i < $len; $i++) {
            $r = $rs[mt_rand(0, count($rs) - 1)];
            $cps[] = mt_rand($r[0], $r[1]);
        }
        enc_cps($enc, $cps);
        $utf8 = '';
        foreach ($cps as $cp) $utf8 .= mb_chr($cp, 'UTF-8');
        $bytes = mb_convert_encoding($utf8, $enc, 'UTF-8');
        dec($enc, $bytes);
        check($enc, $bytes);
        // Corrupt a few bytes
        for ($k = 0; $k < 5; $k++) {
            $pos = mt_rand(0, strlen($bytes) - 1);
            $bytes[$pos] = chr(mt_rand(0, 255));
        }
        dec($enc, $bytes);
        check($enc, $bytes);
    }
}

// ISO-2022-KR specifics
$kr = [
    "", "\x0F", "\x0E", "\x0E\x0F", "\x1B\$)Cabc", "\x1B", "\x1B\$", "\x1B\$)", "\x1B\$)D",
    "\x1B\$(C", "\x1BX)C", "\x1BXYZ", "\x1B\$X", "\x0E\x21", "\x0E\x21\x21", "\x0E\x22\x66",
    "\x0E\x49\x21", "\x0E\x7E\x21", "\x0E\x7D\x7E", "\x0Ea\x80", "\x0E\x30\x0A", "a\x0Eb",
    "\x0E\x30\x21abc", "\x0E\x47\x21", "\x80\x0E\xA1\xA1",
];
foreach ($kr as $s) {
    dec('ISO-2022-KR', $s);
    check('ISO-2022-KR', $s);
}
enc_cps('ISO-2022-KR', []);
enc_cps('ISO-2022-KR', [0x41]);
enc_cps('ISO-2022-KR', [0xAC00]);
enc_cps('ISO-2022-KR', [0xAC00, 0x41, 0xAC01]);
enc_cps('ISO-2022-KR', [0xAC02, 0x2603]);
enc_cps('ISO-2022-KR', [0xAC02, 0x2603], 0x3000);
enc_cps('ISO-2022-KR', [0xAC02, 0x2603], named('long'));
enc_cps('ISO-2022-KR', [0x2603, 0xAC02, 0x2603], named('none'));
enc_cps('ISO-2022-KR', [0x2603, 0xAC02], 0xAC00);
