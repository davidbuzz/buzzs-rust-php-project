<?php
// Cross-family smoke fixture: mixed Japanese/Korean/Chinese text, emoji and
// unmappable characters through every CJK encoding, both directions.
// Usage: php smoke_oracle.php > smoke_oracle.tsv
require __DIR__ . '/oracle.php';

$encodings = ['EUC-JP', 'SJIS', 'eucJP-win', 'EUC-JP-2004', 'SJIS-Mobile#DOCOMO',
    'SJIS-Mobile#KDDI', 'SJIS-Mobile#SOFTBANK', 'SJIS-mac', 'SJIS-2004',
    'UTF-8-Mobile#DOCOMO', 'UTF-8-Mobile#KDDI-A', 'UTF-8-Mobile#KDDI-B',
    'UTF-8-Mobile#SOFTBANK', 'CP932', 'SJIS-win', 'CP51932', 'JIS', 'ISO-2022-JP',
    'ISO-2022-JP-MS', 'GB18030', 'EUC-CN', 'CP936', 'HZ', 'EUC-TW', 'BIG-5', 'CP950',
    'EUC-KR', 'UHC', 'ISO-2022-KR', 'ISO-2022-JP-2004', 'ISO-2022-JP-MOBILE#KDDI',
    'CP50220', 'CP50221', 'CP50222'];
$texts = [
    "Hello, 日本語テキスト ｶﾞｷﾞ ①Ⅻ ～ ㈱ 髙 か\u{309A} \u{1F600} 1\u{20E3} \u{1F1EF}\u{1F1F5}",
    "한국어 텍스트 中文简体 繁體中文 € ¥ ß \u{20B9F} \u{2603}",
    str_repeat("あア亜", 50) . "\u{FFFD}",
];
foreach ($encodings as $enc) {
    foreach ($texts as $t) {
        foreach ([0x3F, 'long', 'none'] as $mode) {
            conv('UTF-8', $enc, $t, $mode);
        }
        $bytes = mb_convert_encoding($t, $enc, 'UTF-8');
        conv($enc, 'UTF-8', $bytes);
        conv($enc, 'UTF-8', $bytes . "\x1B\x80\xFF", 'long');
        check($enc, $bytes);
    }
}
