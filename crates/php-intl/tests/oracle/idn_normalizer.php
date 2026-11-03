<?php
require __DIR__ . '/common.inc';
$domains = ['bücher.de', 'xn--bcher-kva.de', 'example.com', 'a..b', '-ab.com', 'ab-.com', 'ab--cd.com', 'Faß.de', 'βόλος.com', 'ශ්‍රී.com', 'نامه‌ای.com', '1.2.3.4', str_repeat('a', 64) . '.com', str_repeat('abcdefgh.', 30) . 'com', 'xn--abc', 'xn--a.com', 'EXAMPLE.COM', '☃.net', 'a b.com', "a\u{0301}.com", '.', 'a.', "\u{0661}\u{0662}.com", 'xn--zca.com', 'xn--55qx5d.cn', '中国.cn', 'u-.com'];
foreach ($domains as $d) {
    foreach ([0, 1, 2, 4, 8, 16, 32, 2 | 4 | 8 | 16 | 32] as $opt) {
        foreach (['idn_to_ascii', 'idn_to_utf8'] as $f) {
            $info = null;
            $r = attempt(function () use ($f, $d, $opt, &$info) { return $f($d, $opt, INTL_IDNA_VARIANT_UTS46, $info); });
            row('idn', $f, $d, $opt, $r, $info);
        }
    }
}
$strs = ["e\u{0301}", "é", "ﬁ", "Ⅸ", "ẛ̣", "ｶﾞ", "Å", "Å", "ß", "ǅ", "\u{1E9B}\u{0323}", "한글", "a\u{0308}\u{0301}", "\u{0F73}", "Straße", "", "abc", "\u{FB2C}", "\xff"];
foreach ($strs as $s) {
    foreach ([4, 8, 16, 32, 48] as $f) {
        row('norm', $s, $f, attempt(fn() => Normalizer::normalize($s, $f)), attempt(fn() => Normalizer::isNormalized($s, $f) ? 'yes' : 'no'));
    }
}
foreach (['é', 'a', 'ﬁ', 'Ⅸ', '가', 'ǅ', 'Å', "\u{1E9B}", "\u{0F73}"] as $c) {
    foreach ([4, 8, 16, 32] as $f) row('raw', $c, $f, Normalizer::getRawDecomposition($c, $f));
}
