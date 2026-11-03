<?php
// php gb_oracle.php > gb_oracle.tsv
require __DIR__ . '/gb_oracle_lib.php';
mt_srand(18030);

$uni = [[0x20, 0x7E], [0x80, 0x4FF], [0x2000, 0x33FF], [0x4E00, 0x9FFF], [0xE000, 0xE864],
        [0xF900, 0xFFFF], [0x10000, 0x10FFFF], [0x20000, 0x2A6DF]];
foreach (['GB18030', 'CP936'] as $enc) {
    rand_valid($enc, $uni, 60, 1, 20);
    rand_valid($enc, $uni, 4, 130, 300); // > 128 code points: chunking
    rand_encode($enc, $uni, 80);
    rand_bytes($enc, [[0, 0x7F], [0x80, 0xFF], [0x81, 0xFE], [0x30, 0x39], [0x40, 0xFE]], 200);
}

// Cases from gb18030_encoding.phpt / cp936_encoding.phpt
foreach (["\x81\x30\x81\xFF", "\x84\x31\xA4\x40", "\x84\x31\xA5\x30", "\x84\x32\x81\x30",
          "\x85\x31\x81\x30", "\x90\x30\x81\x30", "\xE3\x32\x9A\x35", "\x90\x30\x81\xFF",
          "\xE3\x32\x9A\x36", "\xE4\x30\x81\x35", "\x90\x30\x80\x30", "\x81", "\x81\x30",
          "\x81\x30\x81", "\x81\x30\x81\x30", "\x80", "\xFF", "\xA1\xA1", "\xFE\x50", "\xFE\xA0",
          "\xA6\xD9", "\xA8\xBC", "\x84\x31\xA4\x39", "\x84\x31\xA4\x3A"] as $s) {
    foreach (['GB18030', 'CP936'] as $enc) {
        foreach ([0x25, 'long', 'none'] as $m) {
            xdec($enc, $s, $m);
        }
        check($enc, $s);
    }
}
foreach (["\x81\x20", "\x81\x7F", "\xFE\xFF", "\x80", "\xFF", "\x81"] as $s) {
    xdec('CP936', $s, 'long');
}
xenc('GB18030', [0xAAB82D38 & 0xFFFF, 0x23], 0x25);
xenc('GB18030', [0x1F9, 0x20AC, 0xE5E5, 0xE7C7, 0xFFFF, 0x10FFFF, 0xF92C, 0xFA29, 0x80]);
xenc('CP936', [0x203E, 0x2218, 0x223C, 0xF8F5, 0x80, 0x20AC, 0xF92C, 0xFF04, 0xFF5E]);
