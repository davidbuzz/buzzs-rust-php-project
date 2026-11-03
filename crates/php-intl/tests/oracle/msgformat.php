<?php
require __DIR__ . '/common.inc';
date_default_timezone_set('America/New_York');
ini_set('intl.default_locale', 'en_US');
$patterns = [
    'Hello {0}, you are {1} years old.',
    '{0,number} | {0,number,integer} | {0,number,currency} | {0,number,percent} | {0,number,#,##0.00;(#)} | {0,number, ::compact-short}',
    '{0,date} | {0,date,short} | {0,date,medium} | {0,date,long} | {0,date,full} | {0,date,yyyy-MM-dd HH:mm zzzz} | {0,date,::yMMMEd}',
    '{0,time} | {0,time,short} | {0,time,full} | {0, time, h:mm a}',
    '{0,spellout} | {0,ordinal} | {0,duration} | {0,spellout,%spellout-ordinal}',
    '{0,plural,=0{no files} one{# file} other{# files}}',
    '{0,plural,offset:1 =0{nobody} =1{{1}} one{{1} and # other} other{{1} and # others}}',
    '{0,selectordinal,one{#st} two{#nd} few{#rd} other{#th}}',
    '{0,select,female{She} male{He} other{They}} said {1}',
    '{0,choice,-1#negative|0#zero|0<between|1#one|1<more than one: {0,number}}',
    "It''s '{0}' and '{'braces'}' and '' quote {0}",
    '{0} {1} {2} {3} {4} {5}',
    '{0,plural,one{# one} other{{0,number,#.00} other #}}',
    '{0,number,integer} {1,number,integer}',
    'no arguments at all',
    '',
    '{5} {0}',
    "{0,plural,other{'#' #}}",
];
$argsets = [
    [0, 'x'],
    [1, 'Alice', 3],
    [2, 'Bob'],
    [3.5, 'f'],
    [-1.25, 'male'],
    [21, 'female'],
    [1234567.891, 'Zed'],
    [1341100800, 'z'],
    ['42', '1.5abc'],
    [true, null],
    [1e20, -0.0],
    [PHP_INT_MAX, PHP_INT_MIN],
    [1.5, 2.5, 'three', false, true, 1e-7],
];
foreach (['en_US', 'de_DE', 'fr_FR', 'ar_EG', 'ru_RU', 'ja_JP', 'pl_PL'] as $loc) {
    foreach ($patterns as $p) {
        foreach ($argsets as $a) {
            row('fmt', $loc, $p, $a, attempt(fn() => MessageFormatter::formatMessage($loc, $p, $a)));
        }
    }
}
$named = [
    '{name} has {count, plural, =0{no cats} one{one cat} other{# cats}}',
    '{gender, select, female{{name} est allée} other{{name} est allé}} à {place}',
    '{n,number,integer} / {d,date,short} / {t,time,short}',
    '{a} {b} {a}',
    '{x, plural, offset:2 other{# (#)}}',
];
$namedargs = [
    ['name' => 'Ann', 'count' => 0, 'gender' => 'female', 'place' => 'Paris', 'n' => 12345, 'd' => 0, 't' => 3600, 'a' => 'A', 'b' => 'B', 'x' => 5],
    ['name' => 'Bob', 'count' => 1, 'gender' => 'male', 'place' => 'Rome', 'n' => 2.7, 'd' => 1341100800, 't' => 1341100800.5, 'a' => 1.5, 'b' => true, 'x' => 1.5],
    ['name' => 'Cid', 'count' => 7, 'gender' => 'x', 'n' => '9999999999999', 'd' => '86400', 't' => -1, 'x' => -3],
];
foreach (['en_US', 'fr_FR', 'ru_RU'] as $loc) {
    foreach ($named as $p) {
        foreach ($namedargs as $a) {
            row('named', $loc, $p, $a, attempt(fn() => MessageFormatter::formatMessage($loc, $p, $a)));
        }
    }
}
$bad = ['{0', '{0,foo}', '{0,choice}', '{0,plural,one{x}}', '{0,number} {0,date}', "{0,date,'unterminated}", '{a}{0}', '{0,select,a{b}', '{ 0 }', '{0,number,#,##,,0}', '}', '{0,plural,=x{a} other{b}}'];
foreach ($bad as $p) {
    row('bad', $p, attempt(fn() => MessageFormatter::formatMessage('en_US', $p, [1])), intl_get_error_message());
    row('badctor', $p, attempt(fn() => get_class(new MessageFormatter('en_US', $p))));
}
$parse = [
    ['{0} has {1,number} apples', ['Bob has 1,234.5 apples', 'Bob has apples', ' has 3 apples', 'Bob has 12 apples and more']],
    ['{0,number,integer}-{1,number}', ['12-3.5', '1,000-2', 'x-1', '9999999999999-1']],
    ['{0,date,short} at {0,time,short}', ['7/1/12 at 4:00 AM', 'garbage']],
    ['{0,choice,0#none|1#one|2#two}! {1}', ['one! x', 'two! ', 'none! end', 'zero!']],
    ['{0,spellout} items', ['forty-two items', 'many items']],
    ['{0,plural,other{#}}', ['5']],
    ['{2} and {0}', ['a and b', '{2} and {0}']],
    ['literal only', ['literal only', 'literal']],
    ['', ['', 'x']],
];
foreach ($parse as [$p, $inputs]) {
    foreach ($inputs as $in) {
        row('parse', $p, $in, attempt(fn() => MessageFormatter::parseMessage('en_US', $p, $in)));
    }
}
$mf = new MessageFormatter('de_DE', '{0,number} {1}');
row('obj', $mf->getLocale(), $mf->getPattern(), $mf->format([1234.5, 'x']));
$mf->setPattern('{1}: {0,date,yyyy}');
row('setpat', $mf->getPattern(), $mf->format([0, 'y']));
row('setbad', attempt(fn() => $mf->setPattern('{0,choice}')), $mf->getErrorCode(), $mf->getPattern(), attempt(fn() => $mf->format([1])));
foreach (['en_US', 'de', 'zh_Hant_TW', 'xx', 'EN-us', 'root', 'sr_Latn_RS@calendar=gregorian'] as $l) {
    row('locale', $l, (new MessageFormatter($l, '{0}'))->getLocale());
}
