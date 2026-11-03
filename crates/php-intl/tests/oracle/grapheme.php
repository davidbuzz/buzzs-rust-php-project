<?php
require __DIR__ . '/common.inc';
$strs = ['abc', 'ab\r\nc', "a\u{0301}bc\u{0308}", 'Ação é boa', "👨‍👩‍👧x🇵🇹y", "o\u{0308}o\u{0308}O\u{0308}", '', "zz\xff", 'ÄÖÜäöü', 'ABCabcABC', "a\r\nb\r\nc", 'Straße STRASSE', "日本語テキスト日本"];
$needles = ['a', 'b', 'c', "\u{0301}", "a\u{0301}", 'ö', "o\u{0308}", 'O', 'x', 'ab', '', 'ä', 'Ä', '本', 'ss', 'ß', "\r\n", "\n", 'zz'];
foreach ($strs as $s) {
    row('len', $s, attempt(fn() => grapheme_strlen($s)));
    foreach ($needles as $n) {
        foreach ([0, 1, 2, -1, -2, -5, 4] as $off) {
            foreach (['grapheme_strpos', 'grapheme_stripos', 'grapheme_strrpos', 'grapheme_strripos'] as $f) {
                row('pos', $f, $s, $n, $off, attempt(fn() => $f($s, $n, $off)));
            }
        }
        foreach ([false, true] as $before) {
            row('strstr', $s, $n, $before, attempt(fn() => grapheme_strstr($s, $n, $before)), attempt(fn() => grapheme_stristr($s, $n, $before)));
        }
    }
    foreach ([0, 1, 2, -1, -2, -3, 5, 20, -20] as $st) {
        foreach ([null, 0, 1, 2, -1, -2, 10, -10] as $l) {
            row('substr', $s, $st, $l, attempt(fn() => grapheme_substr($s, $st, $l)));
        }
    }
    foreach ([0, 1, 2] as $type) {
        foreach ([0, 1, 2, 3, 5, 100] as $size) {
            foreach ([0, 1, 2, 3, -1, 50] as $start) {
                $next = 'unset';
                $r = attempt(function () use ($s, $size, $type, $start, &$next) { return grapheme_extract($s, $size, $type, $start, $next); });
                row('extract', $s, $size, $type, $start, $r, $next);
            }
        }
    }
    foreach ([1, 2, 3, 10] as $l) row('split', $s, $l, attempt(fn() => grapheme_str_split($s, $l)));
}
