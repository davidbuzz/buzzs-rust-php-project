<?php
// Oracle fixtures for JIS, ISO-2022-JP, ISO-2022-JP-MOBILE#KDDI and
// ISO-2022-JP-MS. Regenerate with:
//   php tests/fixtures/cjk/jis_oracle.php > tests/fixtures/cjk/jis_oracle.tsv
require __DIR__ . '/oracle.php';

mt_srand(20261103);

// oracle.php's conv() leaves the substitute character set when switching to
// "long"/"entity"/"none" (which use it for bad input), so always reset it to
// '?' first: the fixture format assumes '?' for those modes.
function xdec(string $enc, string $in, $mode = 0x3F): void {
    mb_substitute_character(0x3F);
    dec($enc, $in, $mode);
}
function xenc(string $enc, array $cps, $mode = 0x3F): void {
    mb_substitute_character(0x3F);
    enc_cps($enc, $cps, $mode);
}
function xconv(string $from, string $to, string $in): void {
    mb_substitute_character(0x3F);
    conv($from, $to, $in);
}

$encs = ['JIS', 'ISO-2022-JP', 'ISO-2022-JP-MOBILE#KDDI', 'ISO-2022-JP-MS'];

function rnd_bytes(int $len): string {
    static $alpha = null;
    if ($alpha === null) {
        $alpha = "\x1B\x1B\x1B\x1B\x24\x24\x24\x24((((BBB@@DDIIJJH??\x0E\x0F\x21\x22\x30\x41\x5C\x7E\x7F\x75\x76\x7A\x2D\x30\x74\x23\x80\xA1\xB1\xDF\xE0\xFF\x00\x0A";
    }
    $s = '';
    for ($i = 0; $i < $len; $i++) {
        if (mt_rand(0, 3) == 0) {
            $s .= chr(mt_rand(0x21, 0x7E));
        } else {
            $s .= $alpha[mt_rand(0, strlen($alpha) - 1)];
        }
    }
    return $s;
}

// Single bytes, alone and followed by 'A'
foreach ($encs as $enc) {
    for ($i = 0; $i < 256; $i++) {
        xdec($enc, chr($i));
        xdec($enc, chr($i) . 'A');
    }
}

// Escape sequences (incl. truncated / invalid ones) followed by text
$tails = ['', 'A', '!!', '!', "\x30\x21", "\x75\x21\x21", "\x5C\x7E", "\x21\x7F", "\x7F\x21", "\x30\x21A"];
foreach ($encs as $enc) {
    foreach (['', "\x24", '(', "\x24(", ',', 'A'] as $mid) {
        for ($i = 0x20; $i < 0x80; $i++) {
            $esc = "\x1B" . $mid . chr($i);
            foreach ([$tails[mt_rand(0, count($tails) - 1)], $tails[mt_rand(0, count($tails) - 1)]] as $tail) {
                xdec($enc, $esc . $tail);
            }
        }
    }
    foreach (["\x1B", "\x1B\x24", "\x1B(", "\x1B\x24(", "A\x1B", "A\x1B\x24", "\x1B\x24(X", "\x1B\x24B!", "\x1B\x24(D!", "\x1B(I!",
              "\x0E!\x0F", "\x0E\x60\x0F", "\x1B(J\x5C\x7E", "\x1B\x24(?!!", "\x1B\x24(?\x34\x7E", "\x1B\x24(?\x35\x21",
              "\x1B\x24B\x7F\x21", "\x1B\x24B\x75\x21\x75\x22", "\x1B\x24B\x7A\x7B"] as $s) {
        xdec($enc, $s);
        xdec($enc, $s, 'long');
        xdec($enc, $s, 'none');
    }
}

// 2-byte codes in kanji modes (sampled), per encoding
foreach ($encs as $enc) {
    foreach (["\x1B\x24B", "\x1B\x24(D", "\x1B\x24(?"] as $esc) {
        $s = $esc;
        for ($c1 = 0x21; $c1 <= 0x7F; $c1++) {
            for ($c2 = 0x20; $c2 <= 0x80; $c2 += mt_rand(1, 9)) {
                $s .= chr($c1) . chr($c2);
            }
            if (strlen($s) > 300) {
                xdec($enc, $s);
                $s = $esc;
            }
        }
        xdec($enc, $s);
    }
}

// Random byte strings, short and long (long ones cross 128-codepoint chunks)
foreach ($encs as $enc) {
    for ($n = 0; $n < 400; $n++) {
        xdec($enc, rnd_bytes(mt_rand(1, 12)));
    }
    for ($n = 0; $n < 40; $n++) {
        xdec($enc, rnd_bytes(mt_rand(100, 600)));
    }
    for ($n = 0; $n < 30; $n++) {
        xdec($enc, rnd_bytes(mt_rand(1, 40)), ['long', 'none', 'entity', 0x3013][mt_rand(0, 3)]);
    }
}

// Encoding
$cp_pool = [0, 0x41, 0x5C, 0x7E, 0x0E, 0x0F, 0x1B, 0x80, 0xA5, 0xA9, 0xAE, 0xA1, 0xA6, 0x203E, 0x2015, 0x2016, 0x2225, 0x2212, 0x301C,
    0xFF3C, 0xFF0D, 0xFFE0, 0xFFE1, 0xFFE2, 0xFFE3, 0xFFE5, 0xFF5E, 0xFF61, 0xFF71, 0xFF9F, 0x3000, 0x3042, 0x30A2, 0x4E00, 0x4E02, 0x4E9C,
    0x9ED1, 0x7E8A, 0x2460, 0x2473, 0x2160, 0x2170, 0x3231, 0x2252, 0xFFE4, 0xFF07, 0xFF02, 0x7E7B, 0xE000, 0xE001, 0xE05D,
    0xE757, 0xE758, 0xF8F0, 0x10000, 0x1F600, 0x20E3, 0x23, 0x30, 0x31, 0x39, 0x1F1EF, 0x1F1F5, 0x1F1FA, 0x1F1F8, 0x1F1E8, 0x1F1F3,
    0x1F1EC, 0x1F1E7, 0x1F1F7, 0x1F1F0, 0x1F1E9, 0x1F1EA, 0x1F1EB, 0x1F1EE, 0x1F1F9,
    0x2600, 0x2601, 0x2614, 0x26C4, 0x1F004, 0x1F17F, 0x1F300, 0x1F301, 0x1F31F, 0x1F680, 0xFE82D, 0xFE82E, 0xFE838, 0xFEE40, 0xFEE4A, 0x2002, 0x2122,
    0x3299, 0x00F1, 0x0100, 0x02D8, 0x0384, 0xFFFF, 0xD7FF, 0x10FFFF, 0x2EBB, 0x9FA0];
for ($i = 0; $i < 200; $i++) {
    $cp_pool[] = mt_rand(0x4E00, 0x9FFF);
    $cp_pool[] = mt_rand(0x2000, 0x33FF);
    $cp_pool[] = mt_rand(0xFF00, 0xFFEF);
    $cp_pool[] = mt_rand(0x80, 0x45F);
}
foreach ($encs as $enc) {
    foreach ($cp_pool as $cp) {
        xenc($enc, [$cp]);
        xenc($enc, [$cp, 0x41]);
    }
    foreach ([[0x23, 0x20E3], [0x30, 0x20E3], [0x35, 0x20E3], [0x35], [0x35, 0x41], [0x4E00, 0x35], [0x4E00, 0x35, 0x20E3],
              [0x1F1EF, 0x1F1F5], [0x1F1FA, 0x1F1F8], [0x1F1EC, 0x1F1E7], [0x1F1F7, 0x1F1FA], [0x1F1EF], [0x1F1EF, 0x41],
              [0x1F1E8, 0x1F1F3, 0x1F1E8], [0x1F1F0, 0x1F1F7], [0x1F1E9, 0x1F1EA], [0xFF71, 0x41, 0x3042, 0xA5, 0x203E, 0x41],
              [0x3042, 0xFF71], [0xE000, 0x3042, 0xE757, 0x41]] as $cps) {
        xenc($enc, $cps);
        xenc($enc, $cps, 'long');
    }
    for ($n = 0; $n < 300; $n++) {
        $len = mt_rand(1, 10);
        $cps = [];
        for ($k = 0; $k < $len; $k++) {
            $cps[] = $cp_pool[mt_rand(0, count($cp_pool) - 1)];
        }
        xenc($enc, $cps, ['long', 'none', 'entity', 0x3F, 0x3042, 0x1F600][mt_rand(0, 5)]);
    }
    for ($n = 0; $n < 20; $n++) {
        $cps = [];
        for ($k = 0; $k < 300; $k++) {
            $cps[] = $cp_pool[mt_rand(0, count($cp_pool) - 1)];
        }
        xenc($enc, $cps);
    }
}

// Same-encoding conversions (decode errors re-encoded)
foreach ($encs as $enc) {
    for ($n = 0; $n < 100; $n++) {
        xconv($enc, $enc, rnd_bytes(mt_rand(1, 20)));
    }
}

// mb_check_encoding
foreach ($encs as $enc) {
    for ($i = 0; $i < 256; $i++) {
        check($enc, chr($i));
    }
    for ($n = 0; $n < 600; $n++) {
        check($enc, rnd_bytes(mt_rand(1, 14)));
    }
    foreach (["\x1B\x24B\x30\x21\x1B(B", "\x1B\x24B\x30\x21", "\x1B(J\x5C\x1B(B", "\x1B(I\x21\x1B(B", "\x0E\x21\x0F", "\x0E\x21", "\x0F",
              "\x1B\x24(D\x22\x42\x1B(B", "\x1B(H", "\x1B\x24(?\x21\x21\x1B(B", "\xA1", "\x1B\x24@\x30\x21\x1B(B", "\x1B\x24(B\x30\x21\x1B(B"] as $s) {
        check($enc, $s);
    }
}
