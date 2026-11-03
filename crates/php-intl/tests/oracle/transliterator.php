<?php
require __DIR__ . '/common.inc';
$ids = ['Any-Latin', 'Latin-ASCII', 'Any-Latin; Latin-ASCII; Lower()', 'Greek-Latin', 'Latin-Greek', 'Cyrillic-Latin', 'Upper', 'Lower', 'Title', 'NFD; [:Nonspacing Mark:] Remove; NFC', 'Hex-Any', 'Any-Hex', 'Han-Latin', 'Katakana-Hiragana', 'Hiragana-Katakana', 'de-ASCII', 'Any-Publishing', 'Bogus-Id', 'Null', '::Lower;', 'Remove', '[a-z] Upper'];
$strs = ['Hello World', 'Ἀριστοτέλης', 'Привет мир', '日本語', 'カタカナ', 'ひらがな', 'Ärger über Öl', '"quotes" -- dashes', 'xéy', "\u{00E9}"];
foreach ($ids as $id) {
    foreach ([0, 1] as $dir) {
        $t = attempt(fn() => Transliterator::create($id, $dir));
        if (!is_object($t)) { row('create', $id, $dir, $t); continue; }
        $inv = attempt(fn() => Transliterator::create($id, $dir)->createInverse());
        row('create', $id, $dir, $t->id, is_object($inv) ? $inv->id : $inv);
        foreach ($strs as $s) row('trans', $id, $dir, $s, attempt(fn() => $t->transliterate($s)), attempt(fn() => $t->transliterate($s, 1, 3)));
    }
}
$rules = ['a > b; b > a;', '$vowel = [aeiou]; $vowel > X;', ':: Upper; ::Lower;', 'a > ;!', '[[', 'x } y > z;', '::Null;'];
foreach ($rules as $r) {
    foreach ([0, 1] as $dir) {
        $t = attempt(fn() => Transliterator::createFromRules($r, $dir));
        row('rules', $r, $dir, is_object($t) ? [$t->id, $t->transliterate('abc aeiou xyz')] : $t, intl_get_error_message());
    }
}
$all = Transliterator::listIDs();
row('ids', count($all), array_slice($all, 0, 50));
