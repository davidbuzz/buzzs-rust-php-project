<?php
require __DIR__ . '/common.inc';
date_default_timezone_set('Europe/Lisbon');
ini_set('intl.default_locale', 'en_US');
$locales = ['en_US', 'de_DE', 'fr_FR', 'ja_JP', 'ar_EG', 'he_IL', 'th_TH', 'zh_CN', 'ru_RU', 'pt_BR', 'ja_JP@calendar=japanese', 'en_US@calendar=islamic'];
$styles = [-1, 0, 1, 2, 3, 128, 129, 130, 131];
$times = [0, 1341100800, -1000000000, 1711846800, 2000000000.5, 86400 * 365 * 30];
foreach ($locales as $loc) {
    foreach ($styles as $d) {
        foreach ($styles as $t) {
            if (($d + $t) % 2 && $d != $t && $d != -1) continue;
            $f = new IntlDateFormatter($loc, $d, $t, 'America/New_York');
            row('meta', $loc, $d, $t, $f->getPattern(), $f->getLocale(0), $f->getLocale(1), $f->getCalendar(), $f->getTimeZoneId(), $f->isLenient());
            foreach ($times as $ts) row('fmt', $loc, $d, $t, $ts, attempt(fn() => $f->format($ts)));
        }
    }
}
foreach (['UTC', 'Asia/Kolkata', 'Australia/Lord_Howe', 'GMT+03:00', 'Pacific/Chatham'] as $tz) {
    foreach ([0, 1] as $cal) {
        $f = new IntlDateFormatter('en_US', 0, 0, $tz, $cal);
        foreach ([-62135596800, -12219292800 - 86400, 0, 1341100800] as $ts) row('fmtcal', $tz, $cal, $ts, $f->format($ts));
    }
}
$patterns = ['yyyy-MM-dd HH:mm:ss zzzz', "EEEE, d 'de' MMMM 'de' y", 'QQQ qqqq w W D F E e c a h K k H m s S SS SSS A z Z ZZZZ v VVVV O X x', 'G GGGG GGGGG yy YYYY u U r', 'LLLL MMMMM ccc', "'quoted '' text'", 'B bbbb', ''];
foreach ($patterns as $p) {
    foreach (['en_US', 'pt_PT', 'zh_CN@calendar=chinese'] as $loc) {
        $f = new IntlDateFormatter($loc, IntlDateFormatter::FULL, IntlDateFormatter::FULL, 'Europe/Lisbon', null, $p);
        row('pattern', $p, $loc, $f->getPattern(), $f->format(1341100800), $f->format(-1000000000));
    }
}
// parse
$f = new IntlDateFormatter('en_US', -2, -2, 'UTC', null, 'yyyy-MM-dd HH:mm:ss');
foreach (['2012-07-01 12:00:00', '2012-07-01 12:00:00 trailing', 'garbage', '2012-13-45 99:99:99', '  2012-07-01 12:00:00', '1-1-1 1:1:1', '99999-01-01 00:00:00', ''] as $in) {
    foreach ([null, 0, 5, 100] as $off) {
        $o = $off;
        $r = attempt(function () use ($f, $in, &$o, $off) { return $off === null ? $f->parse($in) : $f->parse($in, $o); });
        row('parse', $in, $off, $r, $o);
        $o = $off;
        $g = clone $f;
        $r = attempt(function () use ($g, $in, &$o, $off) { return $off === null ? $g->localtime($in) : $g->localtime($in, $o); });
        row('localtime', $in, $off, $r, $o);
    }
}
$f->setLenient(false);
foreach (['2012-07-01 12:00:00', '2012-02-30 12:00:00', '2012-7-1 12:00:00'] as $in) row('strict', $in, attempt(fn() => $f->parse($in)));
foreach (['en_US' => 'MMMM d, y h:mm a', 'de_DE' => 'd. MMMM y HH:mm', 'ja_JP' => 'y年M月d日 H時m分'] as $loc => $p) {
    $g = new IntlDateFormatter($loc, -2, -2, 'Europe/Lisbon', null, $p);
    $s = $g->format(1341100800);
    $o = 0;
    row('roundtrip', $loc, $p, $s, $g->parse($s, $o), $o);
}
// array input
$f = new IntlDateFormatter('en_US', 0, 0, 'UTC');
foreach ([['tm_year' => 112, 'tm_mon' => 6, 'tm_mday' => 1, 'tm_hour' => 12, 'tm_min' => 30, 'tm_sec' => 15], ['tm_year' => 70], ['tm_mday' => 0], ['foo' => 1]] as $a) {
    row('arr', $a, attempt(fn() => $f->format($a)));
}
// DateTime / IntlCalendar / strings
foreach (['2012-02-29 13:14:15.5 America/Sao_Paulo', '2000-01-01 +05:30', '@1234567890'] as $s) {
    row('dtfmt', $s, $f->format(new DateTime($s)));
}
foreach (['12345', ' 12.5 ', '1e3', 'abc', '12abc'] as $s) row('strfmt', $s, attempt(fn() => $f->format($s)));
$c = IntlCalendar::createInstance('Asia/Tokyo', 'ja_JP@calendar=japanese'); $c->setTime(1341100800000.0);
row('calfmt', $f->format($c));
// setters
$f = new IntlDateFormatter('en_US', 0, 0, 'UTC');
foreach (['Europe/Lisbon', 'GMT-0530', 'America/Argentina/Buenos_Aires'] as $tz) {
    $f->setTimeZone($tz);
    row('settz', $tz, $f->getTimeZoneId(), $f->getTimeZone()->getID(), $f->format(1341100800));
}
foreach ([0, 1] as $cal) {
    $f->setCalendar($cal);
    row('setcal', $cal, $f->getCalendar(), $f->getCalendarObject()->getType(), $f->format(-12219292800 - 86400 * 20));
}
$f->setCalendar(IntlCalendar::createInstance('Asia/Tokyo', 'ja_JP@calendar=japanese'));
row('setcalobj', $f->getCalendar(), $f->getTimeZoneId(), $f->format(1341100800));
$f = new IntlDateFormatter('th_TH', 0, 0, 'UTC', IntlDateFormatter::TRADITIONAL);
row('trad', $f->getCalendarObject()->getType(), $f->format(1341100800));
$f->setCalendar(IntlDateFormatter::GREGORIAN);
row('trad2', $f->getCalendarObject()->getType(), $f->format(1341100800));
// formatObject
$dt = new DateTime('2012-07-01 12:00:00', new DateTimeZone('Europe/Lisbon'));
$cal = IntlCalendar::fromDateTime($dt, 'ja_JP@calendar=japanese');
foreach ([null, 0, 3, -1, 130, [0, -1], [-1, 3], [1, 129], 'yyyy-MM-dd zzzz', "EEEE 'q'"] as $fmt) {
    foreach (['en_US', 'pt_PT', 'ja_JP'] as $loc) {
        row('fmtobj', $fmt, $loc, attempt(fn() => IntlDateFormatter::formatObject($dt, $fmt, $loc)), attempt(fn() => IntlDateFormatter::formatObject($cal, $fmt, $loc)));
    }
}
