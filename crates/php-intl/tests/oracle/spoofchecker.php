<?php
require __DIR__ . '/common.inc';
set_error_handler(function ($no, $str) { $GLOBALS['warn'][] = $str; return true; });
function warns() { $w = $GLOBALS['warn'] ?? []; $GLOBALS['warn'] = []; return implode('|', $w); }
$strs = ['', 'abc', 'hello, world', 'he11o, wor1d', "http://www.payp\u{0430}l.com", "\u{041F}aypal.com",
    'NAPKIN PEZ', 'napkin pez', "\u{91CE}\u{7403}\u{30FC}", "\u{91CE}\u{7403}\u{30FC} abc", '한국말', 'HELLO', "H\u{0415}LLO",
    "h\u{0435}llo", "a\u{0301}", "a\u{200B}b", "\x80abc", "1\u{0661}", 'Ελληνικά', 'Ελληνικά abc', "\u{0441}", 'c', 'xn--', "a\0b",
    "\u{05D0}A_1", 'ǉ', "e\u{0301}\u{0301}"];
$configs = [
    'default' => fn($x) => null,
    'locales_en' => fn($x) => $x->setAllowedLocales('en_US'),
    'locales_en_ko' => fn($x) => $x->setAllowedLocales('en_US, ko_KR'),
    'locales_gr' => fn($x) => $x->setAllowedLocales('gr_GR'),
    'locales_ru' => fn($x) => $x->setAllowedLocales('ru'),
    'locales_bad' => fn($x) => $x->setAllowedLocales('!!'),
    'checks_mixed' => fn($x) => $x->setChecks(Spoofchecker::MIXED_SCRIPT_CONFUSABLE | Spoofchecker::WHOLE_SCRIPT_CONFUSABLE | Spoofchecker::SINGLE_SCRIPT_CONFUSABLE),
    'checks_single' => fn($x) => $x->setChecks(Spoofchecker::SINGLE_SCRIPT),
    'checks_invisible' => fn($x) => $x->setChecks(Spoofchecker::INVISIBLE),
    'checks_numbers' => fn($x) => $x->setChecks(Spoofchecker::MIXED_NUMBERS),
    'checks_overlay' => fn($x) => $x->setChecks(Spoofchecker::HIDDEN_OVERLAY | Spoofchecker::INVISIBLE),
    'checks_anycase' => fn($x) => $x->setChecks(Spoofchecker::ANY_CASE | Spoofchecker::MIXED_SCRIPT_CONFUSABLE),
    'checks_charlimit' => fn($x) => $x->setChecks(Spoofchecker::CHAR_LIMIT),
    'checks_zero' => fn($x) => $x->setChecks(0),
    'checks_bad' => fn($x) => $x->setChecks(1 << 20),
    'level_ascii' => fn($x) => $x->setRestrictionLevel(Spoofchecker::ASCII),
    'level_single' => fn($x) => $x->setRestrictionLevel(Spoofchecker::SINGLE_SCRIPT_RESTRICTIVE),
    'level_moderate' => fn($x) => $x->setRestrictionLevel(Spoofchecker::MODERATELY_RESTRICTIVE),
    'level_minimal' => fn($x) => $x->setRestrictionLevel(Spoofchecker::MINIMALLY_RESTRICTIVE),
    'level_unrestricted' => fn($x) => $x->setRestrictionLevel(Spoofchecker::UNRESTRICTIVE),
];
foreach ($configs as $name => $cfg) {
    $x = new Spoofchecker();
    $cfg($x);
    row('config', $name, intl_get_error_code(), warns());
    foreach ($strs as $s) {
        $code = 'unset';
        $r = $x->isSuspicious($s, $code);
        row('susp', $name, $s, $r, $code, intl_get_error_code(), intl_get_error_message(), warns());
    }
    foreach ([['hello', "h\u{0435}llo"], ['HELLO', "H\u{0415}LLO"], ['hello, world', 'goodbye, world'], ['c', "\u{0441}"], ['abc', 'abc'], ['', ''], ["\x80", 'a'], ['paypal', "payp\u{0430}l"], ['rn', 'm'], ['0', 'O'], ["\u{05D0}A_1", "\u{05D0}1_A"], ['Ελληνικά', 'Eλληνικά']] as [$a, $b]) {
        $code = 'unset';
        $r = $x->areConfusable($a, $b, $code);
        row('conf', $name, $a, $b, $r, $code, intl_get_error_code(), warns());
    }
}
