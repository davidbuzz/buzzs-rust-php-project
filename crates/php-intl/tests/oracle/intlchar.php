<?php
require __DIR__ . '/common.inc';
$cps = [0, 9, 10, 32, 48, 57, 65, 90, 97, 122, 0xA0, 0xC9, 0xE9, 0x130, 0x1C5, 0x2028, 0x300, 0x5D0, 0x627, 0x660, 0x669, 0x96F, 0x2160, 0x216B, 0x3007, 0x4E00, 0x4E09, 0xFF10, 0x1F600, 0xD800, 0x10FFFF, 0x28, 0x29, 0x5B, 0xAB, 0x00BD, 0xDF, 0x1E9E, 0xFB01, 0x110000, -1];
$bools = ['isUAlphabetic', 'isULowercase', 'isUUppercase', 'isUWhiteSpace', 'islower', 'isupper', 'istitle', 'isdigit', 'isalpha', 'isalnum', 'isxdigit', 'ispunct', 'isgraph', 'isblank', 'isdefined', 'isspace', 'isJavaSpaceChar', 'isWhitespace', 'iscntrl', 'isISOControl', 'isprint', 'isbase', 'isMirrored', 'isIDStart', 'isIDPart', 'isIDIgnorable', 'isJavaIDStart', 'isJavaIDPart'];
$ints = ['charDirection', 'charType', 'getCombiningClass', 'charDigitValue', 'getBlockCode'];
$chars = ['charMirror', 'tolower', 'toupper', 'totitle', 'getBidiPairedBracket'];
foreach ($cps as $cp) {
    $args = [$cp];
    if ($cp >= 0 && $cp <= 0x10FFFF && ($cp < 0xD800 || $cp > 0xDFFF)) $args[] = IntlChar::chr($cp);
    foreach ($args as $a) {
        $r = [];
        foreach ($bools as $m) $r[] = IntlChar::$m($a);
        foreach ($ints as $m) $r[] = IntlChar::$m($a);
        foreach ($chars as $m) $r[] = IntlChar::$m($a);
        $r[] = IntlChar::foldCase($a, 0);
        $r[] = IntlChar::foldCase($a, IntlChar::FOLD_CASE_EXCLUDE_SPECIAL_I);
        $r[] = IntlChar::digit($a); $r[] = IntlChar::digit($a, 16);
        $r[] = IntlChar::getNumericValue($a);
        $r[] = IntlChar::charAge($a);
        foreach ([0, 1, 2, 3, 4] as $nc) $r[] = IntlChar::charName($a, $nc);
        $r[] = IntlChar::getFC_NFKC_Closure($a);
        $r[] = IntlChar::chr($a); $r[] = IntlChar::ord($a);
        foreach ([0, 1, 6, 0x1000, 0x1002, 0x1005, 0x100A, 0x4000] as $p) { $r[] = IntlChar::hasBinaryProperty($a, $p); $r[] = IntlChar::getIntPropertyValue($a, $p); }
        row('cp', $a, $r, intl_get_error_message());
    }
}
foreach ([0, 1, 2, 0x1000, 0x1002, 0x4000, 0x7000, 99999, -1] as $p) {
    row('prop', $p, IntlChar::getIntPropertyMinValue($p), IntlChar::getIntPropertyMaxValue($p), IntlChar::getPropertyName($p), IntlChar::getPropertyName($p, 0), IntlChar::getPropertyValueName($p, 1), IntlChar::getPropertyValueName($p, 2, 0));
}
foreach (['Alphabetic', 'alpha', 'gc', 'General_Category', 'bogus', 'Block'] as $n) row('penum', $n, IntlChar::getPropertyEnum($n), IntlChar::getPropertyValueEnum(IntlChar::PROPERTY_GENERAL_CATEGORY, 'Lu'), IntlChar::getPropertyValueEnum(IntlChar::PROPERTY_BLOCK, $n));
foreach (['LATIN CAPITAL LETTER A', 'latin capital letter a', 'SNOWMAN', 'GRINNING FACE', 'NOT A NAME', ''] as $n) foreach ([0, 2, 3] as $c) row('fromname', $n, $c, IntlChar::charFromName($n, $c));
foreach ([[0, 2], [5, 10], [15, 36], [36, 16], [-1, 10], [3, 1]] as [$d, $r]) row('fordigit', $d, $r, IntlChar::forDigit($d, $r));
$types = []; IntlChar::enumCharTypes(function ($s, $l, $t) use (&$types) { if (count($types) < 40) $types[] = [$s, $l, $t]; });
row('types', $types);
$names = []; IntlChar::enumCharNames(0x1F600, 0x1F610, function ($c, $n, $name) use (&$names) { $names[] = [$c, $n, $name]; });
row('names', $names);
row('version', IntlChar::getUnicodeVersion());
