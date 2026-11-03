<?php
// Shared generator for the GB18030/CP936/BIG5/CP950/HZ detail oracle
// fixtures (replayed with `testutil::replay_oracle`).
require __DIR__ . '/oracle.php';

// Random valid text: pick random code points, keep those $enc can encode,
// and emit both directions plus a check record.
function rand_valid(string $enc, array $ranges, int $n, int $minLen, int $maxLen): void {
    for ($i = 0; $i < $n; $i++) {
        $len = mt_rand($minLen, $maxLen);
        $cps = [];
        while (count($cps) < $len) {
            [$lo, $hi] = $ranges[mt_rand(0, count($ranges) - 1)];
            $cp = mt_rand($lo, $hi);
            if ($cp >= 0xD800 && $cp <= 0xDFFF) {
                continue;
            }
            $u = mb_chr($cp, 'UTF-8');
            mb_substitute_character('none');
            $b = mb_convert_encoding($u, $enc, 'UTF-8');
            if ($b === '' || mb_convert_encoding($b, 'UTF-8', $enc) !== $u) {
                continue; // not (reversibly) encodable
            }
            $cps[] = $cp;
        }
        $u = '';
        foreach ($cps as $cp) {
            $u .= mb_chr($cp, 'UTF-8');
        }
        mb_substitute_character(0x3F);
        $b = mb_convert_encoding($u, $enc, 'UTF-8');
        conv('UTF-8', $enc, $u);
        conv($enc, 'UTF-8', $b);
        check($enc, $b);
    }
}

// Random code points (any), encoded with various error modes.
function rand_encode(string $enc, array $ranges, int $n): void {
    $modes = [0x3F, 'none', 'long', 'entity', 0x3013, 0x25];
    for ($i = 0; $i < $n; $i++) {
        $len = mt_rand(1, 12);
        $cps = [];
        while (count($cps) < $len) {
            [$lo, $hi] = $ranges[mt_rand(0, count($ranges) - 1)];
            $cp = mt_rand($lo, $hi);
            if ($cp < 0xD800 || $cp > 0xDFFF) {
                $cps[] = $cp;
            }
        }
        xenc($enc, $cps, $modes[$i % count($modes)]);
    }
}

// Random byte strings (biased towards the encoding's byte ranges).
function rand_bytes(string $enc, array $alphabet, int $n): void {
    $modes = [0x3F, 'none', 'long', 'entity', 0x25];
    for ($i = 0; $i < $n; $i++) {
        $len = mt_rand(1, 14);
        $s = '';
        for ($j = 0; $j < $len; $j++) {
            [$lo, $hi] = $alphabet[mt_rand(0, count($alphabet) - 1)];
            $s .= chr(mt_rand($lo, $hi));
        }
        xdec($enc, $s, $modes[$i % count($modes)]);
        check($enc, $s);
    }
}

// "long"/"entity"/"none" keep the previously set substitute character for
// bad input; the replay assumes '?', so reset it first.
function xdec(string $enc, string $in, $mode = 0x3F): void {
    mb_substitute_character(0x3F);
    dec($enc, $in, $mode);
}

function xenc(string $enc, array $cps, $mode = 0x3F): void {
    mb_substitute_character(0x3F);
    enc_cps($enc, $cps, $mode);
}
