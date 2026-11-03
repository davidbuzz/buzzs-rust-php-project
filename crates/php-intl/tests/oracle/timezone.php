<?php
require __DIR__ . '/common.inc';
date_default_timezone_set('UTC');
ini_set('intl.default_locale', 'en_US');
$ids = ['Europe/Lisbon', 'America/New_York', 'Asia/Kolkata', 'Australia/Lord_Howe', 'Asia/Tokyo', 'UTC', 'GMT', 'Etc/GMT+5', 'GMT+05:30', 'GMT-3', 'gmt+1', 'Europe/Kiev', 'Europe/Kyiv', 'US/Pacific', 'EST', 'CET', 'nonsense', '', 'Africa/Casablanca', 'America/Sao_Paulo', 'Pacific/Chatham', 'Antarctica/Troll', 'Etc/Unknown', 'Asia/Calcutta', 'America/Argentina/Buenos_Aires'];
$dates = [0.0, 1341100800000.0, 1325376000000.0, -2208988800000.0, 1711846800000.0, 1711843200000.0, 1730595600000.0];
foreach ($ids as $id) {
    $tz = IntlTimeZone::createTimeZone($id);
    row('id', $id, $tz->getID());
    row('dst', $id, $tz->getDSTSavings());
    foreach ($dates as $d) {
        foreach ([false, true] as $local) {
            $raw = $dst = null;
            $ok = $tz->getOffset($d, $local, $raw, $dst);
            row('offset', $id, $d, $local, $ok, $raw, $dst);
        }
    }
    foreach (['en_US', 'pt_PT', 'ja_JP', 'de_DE', 'fr_FR'] as $loc) {
        foreach ([1, 2, 5, 6, 7, 8] as $type) {
            foreach ([false, true] as $dl) {
                row('display', $id, $loc, $type, $dl, attempt(fn() => $tz->getDisplayName($dl, $type, $loc)));
            }
        }
    }
    row('canonical', $id, attempt(function () use ($id) { $sys = null; $r = IntlTimeZone::getCanonicalID($id, $sys); return $r === false ? false : [$r, $sys]; }));
    row('region', $id, attempt(fn() => IntlTimeZone::getRegion($id)));
    row('count_equiv', $id, IntlTimeZone::countEquivalentIDs($id));
    foreach ([0, 1, 2, 5, -1, 100] as $i) row('equiv', $id, $i, IntlTimeZone::getEquivalentID($id, $i));
    row('windows', $id, attempt(fn() => IntlTimeZone::getWindowsID($id)));
    row('iana', $id, attempt(fn() => IntlTimeZone::getIanaID($id)));
    $dtz = attempt(fn() => $tz->toDateTimeZone());
    row('to_dtz', $id, is_object($dtz) ? $dtz->getName() : $dtz);
    foreach (['Europe/Lisbon', 'Europe/London', 'WET', 'Atlantic/Madeira', 'UTC', 'Etc/UTC', 'Asia/Calcutta', 'Asia/Kolkata', 'GMT+05:30'] as $o) {
        row('same_rules', $id, $o, $tz->hasSameRules(IntlTimeZone::createTimeZone($o)));
    }
}
foreach (['Pacific Standard Time', 'W. Europe Standard Time', 'Romance Standard Time', 'bogus', 'India Standard Time'] as $w) {
    foreach ([null, 'US', 'PT', 'DE', 'XX'] as $r) {
        row('for_windows', $w, $r, attempt(fn() => IntlTimeZone::getIDForWindowsID($w, $r)));
    }
}
foreach ([null, 'PT', 'US', 'XX', '001'] as $c) {
    $e = $c === null ? IntlTimeZone::createEnumeration() : IntlTimeZone::createEnumeration($c);
    row('enum', $c, iterator_to_array($e, false));
}
foreach ([0, 3600000, 19800000, -18000000, 1234] as $o) {
    row('enum_off', $o, iterator_to_array(IntlTimeZone::createEnumeration($o), false));
}
foreach ([0, 1, 2] as $t) {
    foreach ([null, 'PT', 'US'] as $r) {
        foreach ([null, 0, -18000000] as $o) {
            row('idenum', $t, $r, $o, attempt(fn() => iterator_to_array(IntlTimeZone::createTimeZoneIDEnumeration($t, $r, $o), false)));
        }
    }
}
foreach ([new DateTimeZone('Europe/Lisbon'), new DateTimeZone('+03:00'), new DateTimeZone('-03:30'), new DateTimeZone('EST'), new DateTimeZone('CEST'), new DateTimeZone('-00:30'), new DateTimeZone('+14:00')] as $d) {
    row('from_dtz', $d->getName(), attempt(fn() => IntlTimeZone::fromDateTimeZone($d)->getID()));
}
row('tzdata', IntlTimeZone::getTZDataVersion());
