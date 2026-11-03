<?php
require __DIR__ . '/common.inc';
$locales = ['en_US', 'de_DE', 'fr_FR', 'ja_JP', 'ar_EG', 'hi_IN', 'de_CH', 'fa_IR'];
$styles = [0 => '#,##0.###', 1 => null, 2 => null, 3 => null, 4 => null, 5 => null, 6 => null, 7 => null, 8 => null, 10 => null, 11 => null, 12 => null, 13 => null, 14 => null, 15 => null, 16 => null];
$values = [0, 1, -1, 2, 21, 1234, -1234.5678, 1234567.891, 0.5, 1e-7, 123456789012345, PHP_INT_MAX, PHP_INT_MIN, 1.5e300, -0.0, INF, NAN, 3600];
foreach ($locales as $loc) {
    foreach ($styles as $st => $pat) {
        $f = new NumberFormatter($loc, $st, $pat);
        foreach ($values as $v) {
            if (is_float($v) && !is_finite($v) && in_array($st, [5, 6, 7, 8])) continue;
            foreach ([0, 1, 3] as $t) {
                row('format', $loc, $st, $pat, $v, $t, attempt(fn() => $f->format($v, $t)));
            }
        }
        row('pattern', $loc, $st, $pat, attempt(fn() => $f->getPattern()));
        row('locale', $loc, $st, $pat, $f->getLocale(0), $f->getLocale(1));
        if (!in_array($st, [0, 1, 2, 5])) continue;
        foreach (range(0, 19) as $a) row('getattr', $loc, $st, $pat, $a, attempt(fn() => $f->getAttribute($a)));
        foreach (range(0, 7) as $a) row('gettext', $loc, $st, $pat, $a, attempt(fn() => $f->getTextAttribute($a)));
        foreach (range(0, 28) as $s) row('symbol', $loc, $st, $pat, $s, attempt(fn() => $f->getSymbol($s)));
        foreach (['USD', 'EUR', 'JPY', 'XXX', 'ab'] as $cur) {
            foreach ([1234.5, -0.5] as $v) {
                row('fmtcur', $loc, $st, $pat, $v, $cur, attempt(fn() => $f->formatCurrency($v, $cur)));
            }
        }
    }
}
// parsing
$parse_inputs = ['123', '123.45', '-1,234.5', '1.234,5', '1 234,5', '12abc', 'abc', '', '9223372036854775807', '9223372036854775808', '2147483648', '1e5', '50%', '$1,234.56', '€12', '١٢٣', 'one hundred', '1,2,3', '  42', "\u{a0}42", '-', '0.0000001', '१२३'];
foreach (['en_US', 'de_DE', 'fr_FR', 'ar_EG', 'hi_IN'] as $loc) {
    foreach ([1, 2, 3, 5] as $st) {
        $f = new NumberFormatter($loc, $st);
        foreach ($parse_inputs as $in) {
            foreach ([1, 2, 3] as $t) {
                $pos = 0;
                $r = attempt(function () use ($f, $in, $t, &$pos) { return $f->parse($in, $t, $pos); });
                row('parse', $loc, $st, $in, $t, $r, $pos);
            }
            $r = attempt(fn() => $f->parse($in));
            row('parse_nopos', $loc, $st, $in, $r);
            $pos = 1;
            $r = attempt(function () use ($f, $in, &$pos) { return $f->parse($in, 3, $pos); });
            row('parse_pos1', $loc, $st, $in, $r, $pos);
        }
    }
    $f = new NumberFormatter($loc, 2);
    foreach (['$1,234.56', '1.234,56 €', '€1,234.56', 'USD 12', '12 USD', 'CHF 1', '¥123', 'abc', '1234,56 €', 'US$12.00'] as $in) {
        $pos = 0; $cur = '';
        $r = attempt(function () use ($f, $in, &$cur, &$pos) { return $f->parseCurrency($in, $cur, $pos); });
        row('parsecur', $loc, $in, $r, $cur, $pos);
    }
}
// attributes / patterns / symbols round trips
$f = new NumberFormatter('en_US', 1);
foreach ([[6, 2], [7, 3], [8, 1], [1, 0], [2, 1], [3, 2], [4, 5], [9, 100], [10, 4], [11, 0], [11, 6], [12, 0.5], [13, 12], [14, 2], [15, 2], [16, 1], [17, 2], [18, 3], [19, 1], [0, 1], [99, 1], [12, 0.05], [9, 2.7]] as [$a, $v]) {
    $g = clone $f;
    $ok = attempt(fn() => $g->setAttribute($a, $v));
    row('setattr', $a, $v, $ok, attempt(fn() => $g->getAttribute($a)), $g->format(12345.678), $g->format(-1234567), $g->format(0.000123));
}
foreach ([[0, '+'], [1, 'p'], [2, '<'], [3, '>'], [4, '*'], [5, 'EUR'], [6, '%spellout-numbering'], [99, 'x']] as [$a, $v]) {
    foreach ([1, 2, 5] as $st) {
        $g = new NumberFormatter('en_US', $st);
        $ok = attempt(fn() => $g->setTextAttribute($a, $v));
        row('settext', $st, $a, $v, $ok, attempt(fn() => $g->getTextAttribute($a)), $g->format(-12345.678), $g->format(42));
    }
}
foreach (range(0, 30) as $s) {
    $g = new NumberFormatter('en_US', 1);
    $ok = attempt(fn() => $g->setSymbol($s, '_X_'));
    row('setsym', $s, $ok, attempt(fn() => $g->getSymbol($s)), $g->format(-12345.678), $g->format(1.5e20));
}
foreach (['#,##0.00', '0.###E0', '#,##0.00;(#,##0.00)', '00000', '¤#,##0.00', "'#'#", '#,##,##0.0#', '@@@', '*x#,##0', '0.00%', '#,##0.0#‰', '[', '#.#.#', ''] as $p) {
    $g = new NumberFormatter('en_US', 1);
    $ok = attempt(fn() => $g->setPattern($p));
    row('setpattern', $p, $ok, attempt(fn() => $g->getPattern()), $g->format(-12345.678), $g->format(0.5), $g->format(7));
}
foreach (['@numbers=arab', 'en@numbers=hanidec', 'th_TH@numbers=thai', 'de@currency=CHF', 'en_US_POSIX', 'root', 'und', 'uk', 'en-US', 'zh_Hant_TW', 'sr_Latn', 'xx', 'EN_us', 'de_DE@collation=phonebook'] as $loc) {
    foreach ([1, 2, 8, 5] as $st) {
        $r = attempt(fn() => (new NumberFormatter($loc, $st))->format(1234.5));
        row('locfmt', $loc, $st, $r, attempt(fn() => (new NumberFormatter($loc, $st))->getLocale(1)));
    }
}
foreach (['%spellout-ordinal', '%spellout-cardinal', '%digits-ordinal', '%spellout-numbering-year', '%bogus'] as $rs) {
    $g = new NumberFormatter('en_US', 5);
    $ok = attempt(fn() => $g->setTextAttribute(NumberFormatter::DEFAULT_RULESET, $rs));
    row('ruleset', $rs, $ok, $g->format(1999), $g->format(21.5));
}
$rbnf = "%main: 0: zero; 1: one; 2: two; 3: >>; 10: ten>>;";
row('rbnf', attempt(fn() => (new NumberFormatter('en_US', NumberFormatter::PATTERN_RULEBASED, $rbnf))->format(2)));
row('rbnf', attempt(fn() => (new NumberFormatter('en_US', NumberFormatter::PATTERN_RULEBASED, "%foo: 0: nothing; 1: =#,##0=;"))->format(12345)));
