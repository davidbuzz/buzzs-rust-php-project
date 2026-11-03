<?php
require __DIR__ . '/common.inc';
date_default_timezone_set('Europe/Lisbon');
ini_set('intl.default_locale', 'en_US');
$zones = ['UTC', 'Europe/Lisbon', 'America/New_York', 'Asia/Kolkata', 'GMT+03:00'];
$locales = ['en_US', 'pt_PT', 'de_DE', 'ar_SA', 'ja_JP@calendar=japanese', 'th_TH', 'he_IL@calendar=hebrew', 'zh_CN@calendar=chinese', 'fa_IR', 'en_US@calendar=islamic', 'en_GB', 'fr_FR@fw=wed'];
$times = [0.0, 1341100800000.0, 951782400000.0, -12219292800000.0, 1711846800000.0, 1700000000123.0];
foreach ($zones as $z) {
    foreach ($locales as $l) {
        $c = IntlCalendar::createInstance($z, $l);
        row('create', $z, $l, get_class($c), $c->getType(), $c->getLocale(0), $c->getLocale(1), $c->getFirstDayOfWeek(), $c->getMinimalDaysInFirstWeek(), $c->getTimeZone()->getID());
        foreach (range(1, 7) as $d) row('dow', $z, $l, $d, $c->getDayOfWeekType($d), $c->getWeekendTransition($d));
        foreach ($times as $t) {
            $c->setTime($t);
            $f = [];
            foreach (range(0, 23) as $fl) $f[] = $c->get($fl);
            row('fields', $z, $l, $t, $f, $c->inDaylightTime(), $c->isWeekend(), $c->isWeekend($t + 86400000 * 3));
            $lim = [];
            foreach (range(0, 23) as $fl) $lim[] = [$c->getActualMinimum($fl), $c->getActualMaximum($fl), $c->getMinimum($fl), $c->getMaximum($fl), $c->getGreatestMinimum($fl), $c->getLeastMaximum($fl)];
            row('limits', $z, $l, $t, $lim);
            $dt = attempt(fn() => $c->toDateTime());
            row('todt', $z, $l, $t, is_object($dt) ? $dt->format('U e') : $dt);
        }
    }
}
foreach (['en_US', 'ja_JP@calendar=japanese', 'he_IL@calendar=hebrew'] as $l) {
    foreach ([[2, 1], [2, 13], [5, 40], [1, -3], [3, 60], [11, 30], [7, 3], [6, 400], [0, 1], [12, -1000], [14, 1500]] as [$f, $v]) {
        $c = IntlCalendar::createInstance('UTC', $l);
        $c->setTime(1341100800000.0);
        $c->add($f, $v);
        $a = $c->getTime();
        $c->setTime(1341100800000.0);
        $c->roll($f, $v);
        row('addroll', $l, $f, $v, $a, $c->getTime());
    }
    foreach ([0.0, 2e12, -1e12, 1341100800000.0 + 3e9] as $when) {
        foreach ([0, 1, 2, 3, 5, 11, 12, 13] as $f) {
            $c = IntlCalendar::createInstance('UTC', $l);
            $c->setTime(1341100800000.0);
            row('fdiff', $l, $when, $f, attempt(fn() => $c->fieldDifference($when, $f)), $c->getTime());
        }
    }
}
// set / clear / isSet
$c = IntlCalendar::createInstance('UTC', 'en_US');
$c->clear();
row('cleared', $c->getTime(), $c->isSet(1), $c->isSet(5));
$c->set(1, 2010);
$c->set(2, 5);
row('set2', $c->getTime(), $c->isSet(1));
$c->setDate(2012, 1, 29);
row('setdate', $c->getTime());
$c->setDateTime(2012, 1, 29, 13, 14, 15);
row('setdt', $c->getTime());
$c->setDateTime(2012, 1, 29, 13, 14);
row('setdt5', $c->getTime());
$c->clear(11);
row('clearfield', $c->getTime(), $c->isSet(11));
// lenient / wall time
foreach ([[true, 0, 0], [false, 1, 1], [true, 1, 2]] as [$len, $rep, $skip]) {
    $c = IntlCalendar::createInstance('America/New_York', 'en_US');
    $c->setLenient($len);
    $c->setRepeatedWallTimeOption($rep);
    $c->setSkippedWallTimeOption($skip);
    $c->clear();
    $c->setDateTime(2013, 2, 10, 2, 30, 0);
    $t1 = attempt(fn() => $c->getTime());
    $c->clear();
    $c->setDateTime(2013, 10, 3, 1, 30, 0);
    $t2 = attempt(fn() => $c->getTime());
    $c->clear();
    $c->setDateTime(2013, 1, 31, 0, 0, 0);
    $t3 = attempt(fn() => $c->getTime());
    row('wall', $len, $rep, $skip, $c->isLenient(), $c->getRepeatedWallTimeOption(), $c->getSkippedWallTimeOption(), $t1, $t2, $t3);
}
// gregorian
foreach ([[2012, 1, 29], [2012, 1, 29, 13, 14], [2012, 1, 29, 13, 14, 15], [1582, 9, 10]] as $a) {
    $g = count($a) == 3 ? IntlGregorianCalendar::createFromDate(...$a) : IntlGregorianCalendar::createFromDateTime(...$a);
    row('greg', $a, $g->getTime(), $g->getTimeZone()->getID(), $g->getLocale(1), $g->getType());
}
$g = new IntlGregorianCalendar('UTC', 'en_US');
foreach ([1500, 1600, 1700, 1900, 2000, 2004, 2100, -4, 0, 1] as $y) row('leap', $y, $g->isLeapYear($y));
row('change', $g->getGregorianChange());
$g->setGregorianChange(-1e20);
row('change2', $g->getGregorianChange(), $g->isLeapYear(1500), $g->isLeapYear(1700));
$g->setGregorianChange(0.0);
row('change3', $g->getGregorianChange(), $g->isLeapYear(1900), $g->isLeapYear(1970), $g->isLeapYear(1972));
// equals/before/after/equivalent
$a = IntlCalendar::createInstance('UTC', 'en_US'); $a->setTime(1000.0);
$b = IntlCalendar::createInstance('UTC', 'en_US'); $b->setTime(2000.0);
$cc = IntlCalendar::createInstance('Europe/Lisbon', 'en_US'); $cc->setTime(1000.0);
row('cmp', $a->equals($b), $a->before($b), $a->after($b), $a->equals($cc), $a->isEquivalentTo($b), $a->isEquivalentTo($cc));
foreach (['calendar', 'collation', 'currency', 'hours'] as $k) {
    foreach (['en_US', 'ja_JP', 'th_TH', 'ar_SA'] as $l) {
        foreach ([true, false] as $cu) row('kw', $k, $l, $cu, attempt(fn() => iterator_to_array(IntlCalendar::getKeywordValuesForLocale($k, $l, $cu), false)));
    }
}
row('avail', IntlCalendar::getAvailableLocales());
$dt = new DateTime('2012-02-29 13:14:15.5', new DateTimeZone('America/Sao_Paulo'));
$c = IntlCalendar::fromDateTime($dt);
row('fromdt', $c->getTime(), $c->getTimeZone()->getID(), $c->getLocale(1), $c->getType());
$c = IntlCalendar::fromDateTime(new DateTime('@1234567890'), 'ja_JP@calendar=japanese');
row('fromdt2', $c->getTime(), $c->getTimeZone()->getID(), $c->getType());
$c = IntlCalendar::fromDateTime(new DateTime('2000-01-01', new DateTimeZone('+05:30')));
row('fromdt3', $c->getTime(), $c->getTimeZone()->getID());
