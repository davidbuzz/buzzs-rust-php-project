<?php
// Exhaustive oracle fixtures, stored as FNV-1a-64 hashes of the output to
// keep them small. Replayed by `codecs::cjk::gb18030::exhaustive::replay`.
//
// Line formats (tab-separated):
//   dec  <enc> <prefix-hex> <sep-hex> <b1> <hash> <errors>
//        input = prefix . (chr(b1) . chr(b2) . sep for b2 in 0..255),
//        decoded to UTF-8 with substitute character U+10FFFD
//   dec4 <enc> <b1> <b2> <hash> <errors>
//        input = chr(b1).chr(b2).chr(b3).chr(b4) for b3 in 0x81..0xFE,
//        b4 in 0x30..0x39, decoded like `dec`
//   enc  <enc> <start> <count> <hash> <errors>
//        input = UTF-8 of code points start..start+count-1 (surrogates
//        skipped), encoded to <enc> with substitute character '?'

function ex_errors(callable $f) {
    $before = mb_get_info('illegal_chars');
    $out = $f();
    return [$out, mb_get_info('illegal_chars') - $before];
}

function ex_dec(string $enc, string $prefix, string $sep, int $b1): void {
    $in = $prefix;
    for ($b2 = 0; $b2 < 256; $b2++) {
        $in .= chr($b1) . chr($b2) . $sep;
    }
    mb_substitute_character(0x10FFFD);
    [$out, $err] = ex_errors(fn() => mb_convert_encoding($in, 'UTF-8', $enc));
    echo "dec\t$enc\t" . bin2hex($prefix) . "\t" . bin2hex($sep) . "\t$b1\t" . hash('fnv1a64', $out) . "\t$err\n";
}

function ex_dec4(string $enc, int $b1, int $b2): void {
    $in = '';
    for ($b3 = 0x81; $b3 <= 0xFE; $b3++) {
        for ($b4 = 0x30; $b4 <= 0x39; $b4++) {
            $in .= chr($b1) . chr($b2) . chr($b3) . chr($b4);
        }
    }
    mb_substitute_character(0x10FFFD);
    [$out, $err] = ex_errors(fn() => mb_convert_encoding($in, 'UTF-8', $enc));
    echo "dec4\t$enc\t$b1\t$b2\t" . hash('fnv1a64', $out) . "\t$err\n";
}

function ex_enc(string $enc, int $start, int $count): void {
    $in = '';
    for ($cp = $start; $cp < $start + $count; $cp++) {
        if ($cp >= 0xD800 && $cp <= 0xDFFF) {
            continue;
        }
        $in .= mb_chr($cp, 'UTF-8');
    }
    mb_substitute_character(0x3F);
    [$out, $err] = ex_errors(fn() => mb_convert_encoding($in, $enc, 'UTF-8'));
    echo "enc\t$enc\t$start\t$count\t" . hash('fnv1a64', $out) . "\t$err\n";
}

function ex_bmp_enc(string $enc): void {
    for ($start = 0; $start < 0x10000; $start += 256) {
        if ($start >= 0xD800 && $start <= 0xDFFF) {
            continue;
        }
        ex_enc($enc, $start, 256);
    }
}
