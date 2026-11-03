<?php
// Deterministic randomised oracle cases for the Unicode / transfer-encoding
// codecs; run by gen_oracle.py (output format documented there).
mt_srand(20261103);

function out(...$f) { echo implode("\t", $f), "\n"; }
function pick($arr) { return $arr[mt_rand(0, count($arr) - 1)]; }
function from_alphabet($alpha, $n) {
    $s = '';
    for ($i = 0; $i < $n; $i++) { $s .= $alpha[mt_rand(0, strlen($alpha) - 1)]; }
    return $s;
}
function rand_bytes($n) {
    $s = '';
    for ($i = 0; $i < $n; $i++) { $s .= chr(mt_rand(0, 255)); }
    return $s;
}
function rand_len() {
    $r = mt_rand(0, 9);
    if ($r < 5) return mt_rand(0, 12);
    if ($r < 9) return mt_rand(0, 140);
    return mt_rand(100, 400);
}
function rand_cp() {
    $r = mt_rand(0, 99);
    if ($r < 30) return mt_rand(0, 0x7F);
    if ($r < 40) return pick([0, 0x26, 0x2B, 0x2D, 0x3C, 0x3E, 0x22, 0x27, 0x3F]);
    if ($r < 50) return mt_rand(0x80, 0xFF);
    if ($r < 70) return mt_rand(0x100, 0xFFFF);
    if ($r < 78) return mt_rand(0xD800, 0xDFFF);
    if ($r < 88) return mt_rand(0x10000, 0x10FFFF);
    if ($r < 92) return pick([0xFEFF, 0xFFFE, 0x2260, 0x3013, 0x10FFFD, 0x1F600]);
    if ($r < 96) return mt_rand(0x110000, 0x7FFFFFFF);
    return pick([0xFFFFFFFF, 0x80000000, 0x110000, 0x200000]);
}
function rand_cps($n) {
    $s = '';
    for ($i = 0; $i < $n; $i++) { $s .= pack('N', rand_cp()); }
    return $s;
}
function sub_mode($m) {
    // $m = [mode, char]
    if ($m[0] === 'char') { mb_substitute_character($m[1]); }
    else { mb_substitute_character($m[1]); mb_substitute_character($m[0]); }
    return $m[0] . ':' . $m[1];
}
function conv($in, $from, $to, $m) {
    $tag = sub_mode($m);
    $r = mb_convert_encoding($in, $to, $from);
    out('conv', $from, $to, $tag, bin2hex($in), bin2hex($r));
}
function check($in, $enc) {
    out('check', $enc, bin2hex($in), mb_check_encoding($in, $enc) ? '1' : '0');
}
function cut($in, $enc) {
    $len = strlen($in);
    $start = mt_rand(-2, $len + 1);
    $l = pick([null, mt_rand(0, $len + 2), mt_rand(0, 30), -mt_rand(1, 5)]);
    mb_substitute_character(63);
    $r = mb_strcut($in, $start, $l, $enc);
    out('cut', $enc, (string)$start, $l === null ? 'null' : (string)$l, bin2hex($in), bin2hex($r));
}
$MODES = [['char', 63], ['char', 0x3013], ['none', 63], ['long', 63], ['entity', 63], ['long', 0x2D], ['char', 0x10FFFD]];
$DEC = ['char', 0x10FFFD];

// --- fixed-width and UTF-16 family --------------------------------------
$WIDE = ['UCS-2', 'UCS-2BE', 'UCS-2LE', 'UCS-4', 'UCS-4BE', 'UCS-4LE',
         'UTF-32', 'UTF-32BE', 'UTF-32LE', 'UTF-16', 'UTF-16BE', 'UTF-16LE'];
$wide_alpha = "\x00\x00\x00\x00\xD8\xD9\xDB\xDC\xDD\xDF\xFE\xFF\xFF\x41\x10\x11\x7F\x80";
foreach ($WIDE as $enc) {
    for ($i = 0; $i < 60; $i++) {
        $n = rand_len();
        $in = mt_rand(0, 2) ? from_alphabet($wide_alpha, $n) : rand_bytes($n);
        if (mt_rand(0, 4) == 0) { $in = pick(["\xFE\xFF", "\xFF\xFE", "\x00\x00\xFE\xFF", "\xFF\xFE\x00\x00"]) . $in; }
        conv($in, $enc, 'UCS-4BE', $DEC);
        check($in, $enc);
    }
    for ($i = 0; $i < 40; $i++) {
        conv(rand_cps(rand_len()), 'UCS-4BE', $enc, pick($MODES));
    }
}

// --- UTF-7 / UTF7-IMAP ---------------------------------------------------
$b64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
$u7_alpha = $b64 . $b64 . "++++----//,,&&& !\"#$%*;<=>@[]^_`{|}\t\r\n'(),.:?~\\\x00\x80\xFF";
foreach (['UTF-7', 'UTF7-IMAP'] as $enc) {
    for ($i = 0; $i < 250; $i++) {
        $r = mt_rand(0, 3);
        if ($r == 0) {
            $in = from_alphabet($u7_alpha, rand_len());
        } else {
            // Valid text, possibly mutated.
            mb_substitute_character(63);
            $in = mb_convert_encoding(rand_cps(rand_len()), $enc, 'UCS-4BE');
            if ($r >= 2 && strlen($in) > 0) {
                for ($k = mt_rand(1, 3); $k > 0 && strlen($in) > 0; $k--) {
                    $pos = mt_rand(0, strlen($in) - 1);
                    $op = mt_rand(0, 2);
                    if ($op == 0) $in = substr($in, 0, $pos) . substr($in, $pos + 1);
                    else if ($op == 1) $in[$pos] = $u7_alpha[mt_rand(0, strlen($u7_alpha) - 1)];
                    else $in = substr($in, 0, $pos);
                }
            }
        }
        conv($in, $enc, 'UCS-4BE', $DEC);
        check($in, $enc);
        if ($i % 2 == 0) cut($in, $enc);
    }
    for ($i = 0; $i < 120; $i++) {
        conv(rand_cps(rand_len()), 'UCS-4BE', $enc, pick($MODES));
    }
}

// --- HTML-ENTITIES -------------------------------------------------------
$names = ['quot', 'amp', 'lt', 'gt', 'nbsp', 'copy', 'para', 'there4', 'diams', 'hearts', 'Amp', 'ampx', 'am', 'zwj', 'euro', 'lang', 'rang'];
$html_alpha = "&&&&###;;;;xX0123456789abcdefABCDEFghqz \x00\x80\xFF-!";
for ($i = 0; $i < 300; $i++) {
    $in = '';
    $n = mt_rand(0, 30);
    for ($k = 0; $k < $n; $k++) {
        $r = mt_rand(0, 6);
        if ($r == 0) $in .= '&' . pick($names) . (mt_rand(0, 4) ? ';' : '');
        else if ($r == 1) $in .= '&#' . pick([mt_rand(0, 200), mt_rand(0, 0x10FFFF), mt_rand(0x110000, PHP_INT_MAX), '4294967361', '18446744073709551681', '', '00065']) . (mt_rand(0, 4) ? ';' : '');
        else if ($r == 2) $in .= '&#' . pick(['x', 'X']) . pick([dechex(mt_rand(0, 0x10FFFF)), dechex(mt_rand(0x110000, PHP_INT_MAX)), '100000041', 'g1', '']) . (mt_rand(0, 4) ? ';' : '');
        else $in .= from_alphabet($html_alpha, mt_rand(0, 25));
    }
    conv($in, 'HTML-ENTITIES', 'UCS-4BE', $DEC);
    check($in, 'HTML-ENTITIES');
    if ($i % 2 == 0) cut($in, 'HTML-ENTITIES');
}
for ($i = 0; $i < 120; $i++) {
    conv(rand_cps(rand_len()), 'UCS-4BE', 'HTML-ENTITIES', pick($MODES));
}

// --- BASE64 / Quoted-Printable / UUENCODE --------------------------------
$b64_alpha = $b64 . $b64 . $b64 . "+/=\r\n \t\x00\x80*-";
$qp_alpha = "=====0123456789ABCDEFabcdefgz\r\n\r\n \t\x00\x80\xFF.";
for ($i = 0; $i < 200; $i++) {
    $in = mt_rand(0, 2) ? from_alphabet($b64_alpha, rand_len()) : base64_encode(rand_bytes(rand_len()));
    conv($in, 'BASE64', 'UCS-4BE', pick([['char', 63], ['none', 63], ['char', 0x3013]]));
    check($in, 'BASE64');
    if ($i % 2 == 0) cut($in, 'BASE64');
    $in = mt_rand(0, 2) ? from_alphabet($qp_alpha, rand_len()) : quoted_printable_encode(rand_bytes(rand_len()));
    conv($in, 'Quoted-Printable', 'UCS-4BE', pick([['char', 63], ['none', 63]]));
    check($in, 'Quoted-Printable');
    if ($i % 2 == 0) cut($in, 'Quoted-Printable');
    $in = 'begin 644 file' . pick(["\n", "\r\n", ""]) . convert_uuencode(rand_bytes(rand_len()));
    if (mt_rand(0, 2) == 0) $in = substr($in, 0, mt_rand(0, strlen($in)));
    if (mt_rand(0, 4) == 0) $in = from_alphabet("begin 0644\n`!\"#\$%&'()*+,-./0123456789:;<=>?@ABCXYZ[\\]^_M", rand_len());
    conv($in, 'UUENCODE', 'UCS-4BE', ['char', 63]);
    check($in, 'UUENCODE');
    if ($i % 2 == 0) cut($in, 'UUENCODE');
}
for ($i = 0; $i < 150; $i++) {
    $bytes = rand_bytes(rand_len());
    if (mt_rand(0, 1)) $bytes = from_alphabet("\r\n\r\n=\x00ab cd\tEF\x80\xFF", rand_len());
    conv($bytes, '8bit', 'BASE64', ['char', 63]);
    conv($bytes, '8bit', 'Quoted-Printable', ['char', 63]);
    conv($bytes, '8bit', 'UUENCODE', ['char', 63]);
    conv($bytes, 'UTF-8', 'BASE64', ['char', 63]);
    conv(rand_cps(rand_len()), 'UCS-4BE', 'UUENCODE', pick($MODES));
}
