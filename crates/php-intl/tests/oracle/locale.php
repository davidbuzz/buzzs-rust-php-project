<?php
require __DIR__ . '/common.inc';
ini_set('intl.default_locale', 'en_US');
$locs = ['uk-ua_CALIFORNIA@currency=;currency=GRN', 'root', 'uk@currency=EURO', 'Hindi', 'de', 'fr', 'ja', 'i-enochian', 'zh-Hant', 'zh-Hans', 'sr-Cyrl', 'sr-Latn', 'zh-Hans-CN', 'sr-Latn-CS', 'sl-rozaj', 'sl-nedis', 'de-CH-1901', 'sl-IT-nedis', 'sl-Latn-IT-nedis', 'de-DE', 'en-US', 'es-419', 'de-CH-x-phonebk', 'az-Arab-x-AZE-derbend', 'qaa-Qaaa-QM-x-southern', 'sr-Latn-QM', 'sr-Qaaa-CS', 'en-US-u-islamcal', 'zh-CN-a-myext-x-private', 'en-a-myext-b-another', 'de-419-DE', 'a-DE', 'ar-a-aaa-b-bbb-a-ccc', 'x-whatever', 'i-klingon', 'zh-min-nan', 'EN_us', 'en_US_POSIX', 'de_DE@collation=phonebook;currency=EUR', 'sl_IT_nedis_rozaj', 'es_ES@calendar=gregorian', 'sr_Latn_RS_REVISED@currency=USD', '', 'xx_YY_ZZ', '-', '@calendar=islamic', 'en-', 'sgn-BE-FR', 'art-lojban', 'en_GB_oed', 'zh_Hant_TW@collation=stroke'];
foreach ($locs as $l) {
    row('get', $l, attempt(fn() => Locale::getPrimaryLanguage($l)), attempt(fn() => Locale::getScript($l)), attempt(fn() => Locale::getRegion($l)), attempt(fn() => Locale::canonicalize($l)), attempt(fn() => Locale::getKeywords($l)), Locale::getAllVariants($l), Locale::parseLocale($l));
    foreach (['en', 'fr', 'de', 'ja', 'xx', 'ar'] as $d) {
        row('disp', $l, $d, attempt(fn() => Locale::getDisplayName($l, $d)), attempt(fn() => Locale::getDisplayLanguage($l, $d)), attempt(fn() => Locale::getDisplayScript($l, $d)), attempt(fn() => Locale::getDisplayRegion($l, $d)), attempt(fn() => Locale::getDisplayVariant($l, $d)));
    }
}
foreach (['calendar', 'collation', 'currency', 'numbers', 'xx'] as $k) {
    foreach (['en', 'de', 'zh'] as $d) {
        row('dispkw', $k, $d, attempt(fn() => Locale::getDisplayKeyword($k, $d)));
        foreach (['de_DE@collation=phonebook;currency=EUR', 'zh_TW@calendar=chinese;numbers=hanidec', 'en'] as $l) {
            row('dispkwv', $l, $k, $d, attempt(fn() => Locale::getDisplayKeywordValue($l, $k, $d)));
        }
    }
}
$compose = [
    ['language' => 'de'],
    ['language' => 'de', 'script' => 'Latn', 'region' => 'DE'],
    ['language' => 'zh', 'script' => 'Hant', 'region' => 'TW', 'variant0' => 'v1', 'variant1' => 'v2', 'private0' => 'p1', 'private1' => 'p2'],
    ['language' => 'en', 'variant' => ['a', 'b'], 'private' => ['x1'], 'extlang' => 'ext'],
    ['language' => 'en', 'extlang0' => 'e1', 'extlang2' => 'e3'],
    ['grandfathered' => 'i-klingon', 'language' => 'xx'],
    ['language' => 'en', 'region' => 12],
    ['language' => 'en', 'variant' => ['a', 3]],
    ['region' => 'DE'],
    [],
    ['language' => 'en', 'private' => 'priv'],
];
foreach ($compose as $c) row('compose', $c, attempt(fn() => Locale::composeLocale($c)));
$filters = [['de-DEVA', 'de-DE'], ['de-DE-1996', 'de-DE'], ['DE-de', 'de_DE'], ['de', 'de-DE'], ['sl_IT', 'sl'], ['sl-IT-nedis', 'sl_IT'], ['en_US@currency=USD', 'en_US'], ['zh-Hant', '*'], ['', 'en'], ['en', ''], ['de-DE', 'de-de']];
foreach ($filters as [$t, $r]) {
    row('filter', $t, $r, attempt(fn() => Locale::filterMatches($t, $r, false)), attempt(fn() => Locale::filterMatches($t, $r, true)));
}
$lookups = [
    [['de-DEVA', 'de-DE-1996', 'de', 'de-De'], 'de-DE-1996-x-prv1-prv2'],
    [['de-DEVA', 'de-DE-1996', 'de', 'de-De'], 'de-DE'],
    [['en', 'en_US', 'zh_Hans'], 'zh-Hans-CN'],
    [['EN_us', 'de'], 'en-US-u-cu-usd'],
    [['fr'], 'en'],
    [['sl_IT'], 'sl-IT-nedis'],
];
foreach ($lookups as [$tags, $r]) {
    foreach ([false, true] as $canon) {
        row('lookup', $tags, $r, $canon, attempt(fn() => Locale::lookup($tags, $r, $canon, 'xx_XX')), attempt(fn() => Locale::lookup($tags, $r, $canon)));
    }
}
foreach (['en-US,en;q=0.5', 'fr-FR;q=0.8,de', 'xx, yy', 'zh-Hant-TW', '*', '', 'en-us', 'pt_BR', 'sr-Latn-RS;q=0.1'] as $h) {
    row('accept', $h, attempt(fn() => Locale::acceptFromHttp($h)));
}
