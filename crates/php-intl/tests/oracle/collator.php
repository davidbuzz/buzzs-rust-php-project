<?php
require __DIR__ . '/common.inc';
ini_set('intl.default_locale', 'en_US');
$words = ['a', 'A', 'b', 'á', 'Ä', 'ae', 'æ', 'côte', 'coté', 'cote', 'côté', 'z', 'ö', 'oe', 'ß', 'ss', 'ch', 'cz', 'll', 'lz', '1', '10', '2', '9', ' ', '', '-', 'ﬁ', 'fi', 'Ω', 'ω', '中', '日本', 'ア', 'あ', 'peach', 'péché', 'pêche', 'sin'];
$locales = ['en_US', 'de_DE', 'de__phonebook', 'sv_SE', 'es_ES@collation=traditional', 'fr_CA', 'ja_JP', 'zh_CN', 'cs_CZ', 'lt_LT', 'root', 'xx'];
foreach ($locales as $loc) {
    $c = new Collator($loc);
    row('meta', $loc, $c->getLocale(0), $c->getLocale(1), $c->getStrength(), intl_get_error_code());
    $cmp = [];
    foreach ($words as $i => $a) foreach ($words as $j => $b) if ($i < $j) $cmp[] = $c->compare($a, $b);
    row('cmp', $loc, implode('', array_map(fn($x) => $x + 1, $cmp)));
    foreach ([0, 1, 2, 3, 15] as $strength) {
        $c2 = new Collator($loc);
        $c2->setStrength($strength);
        $cmp = [];
        foreach ($words as $i => $a) foreach ($words as $j => $b) if ($i < $j) $cmp[] = $c2->compare($a, $b);
        row('cmpstr', $loc, $strength, implode('', array_map(fn($x) => $x + 1, $cmp)));
    }
    $keys = [];
    foreach ($words as $w) $keys[] = bin2hex($c->getSortKey($w));
    row('keys', $loc, $keys);
}
$c = new Collator('en_US');
foreach ([0, 1, 2, 3, 4, 5, 6, 7] as $attr) {
    row('attr', $attr, attempt(fn() => $c->getAttribute($attr)));
    foreach ([16, 17, 20, 21, 0, 1, 2, 3, 15, -1, 99] as $v) {
        $c2 = new Collator('en_US');
        $ok = attempt(fn() => $c2->setAttribute($attr, $v));
        row('setattr', $attr, $v, $ok, attempt(fn() => $c2->getAttribute($attr)), $c2->compare('a', 'A'), $c2->compare('10', '9'), $c2->compare('a b', 'ab'), $c2->compare('côte', 'coté'));
    }
}
$arrays = [
    ['b', 'a', 'C', 'A', 'c', 'B'],
    ['10', '9', '2', '1', '1.5', 'abc', '-1', ' 3', '1e1'],
    [10, '9', 2.5, '1', true, null, false, 'abc', '', 0, '0'],
    ['côte', 'cote', 'coté', 'côté'],
    ['x' => 'b', 'y' => 'a', 5 => 'c', 'z' => 'A'],
    ['Ab', 'aB', 'ab', 'AB'],
];
foreach (['en_US', 'fr_CA', 'de_DE'] as $loc) {
    $c = new Collator($loc);
    foreach ($arrays as $arr) {
        foreach ([0, 1, 2] as $flag) {
            $s = $arr; $c->sort($s, $flag);
            $a = $arr; $c->asort($a, $flag);
            row('sort', $loc, $arr, $flag, $s, array_keys($a));
        }
        $s = $arr; $c->sortWithSortKeys($s);
        row('sortkeys', $loc, $arr, $s);
    }
}
