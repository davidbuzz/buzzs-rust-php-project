<?php
require __DIR__ . '/common.inc';
ini_set('intl.default_locale', 'en_US');
$texts = ["Hello, world! How are you? I'm fine.", "Ação é boa. Ünïcödé!", "日本語のテキストです。次の文。", "สวัสดีครับ ประเทศไทย", "a\u{0301}e\u{0308} 👍🏽 🇵🇹 👨‍👩‍👧", "", "x", "Mr. Smith went to Washington. He arrived at 3 p.m. today.", "line one\nline two\r\nthree\u{2028}four", "abc\xffdef\xc3", "12,345.67 + 3.14 = $12,348.81"];
$kinds = ['createCharacterInstance', 'createWordInstance', 'createLineInstance', 'createSentenceInstance', 'createTitleInstance', 'createCodePointInstance'];
foreach ($kinds as $k) {
    foreach (['en', 'ja', 'th', 'de'] as $loc) {
        if ($k === 'createCodePointInstance' && $loc !== 'en') continue;
        $bi = $k === 'createCodePointInstance' ? IntlBreakIterator::createCodePointInstance() : IntlBreakIterator::$k($loc);
        row('meta', $k, $loc, get_class($bi), $k === 'createCodePointInstance' ? '' : $bi->getLocale(0), $k === 'createCodePointInstance' ? '' : $bi->getLocale(1));
        foreach ($texts as $t) {
            $bi->setText($t);
            $fwd = [$bi->first()]; $st = [];
            if ($bi instanceof IntlRuleBasedBreakIterator) $st[] = $bi->getRuleStatusVec();
            while (($n = $bi->next()) !== IntlBreakIterator::DONE) { $fwd[] = $n; if ($bi instanceof IntlRuleBasedBreakIterator) $st[] = [$bi->getRuleStatus(), $bi->getRuleStatusVec()]; }
            $back = [$bi->last()];
            while (($n = $bi->previous()) !== IntlBreakIterator::DONE) $back[] = $n;
            $fol = []; $pre = []; $isb = [];
            for ($i = -1; $i <= strlen($t) + 1; $i++) { $fol[] = $bi->following($i); $pre[] = $bi->preceding($i); $isb[] = $bi->isBoundary($i) ? 1 : 0; $isb[] = $bi->current(); }
            $nx = [];
            foreach ([0, 1, 2, -1, 5, -3] as $n) { $bi->first(); $bi->next(2); $nx[] = $bi->next($n); $nx[] = $bi->current(); }
            $cp = $bi instanceof IntlCodePointBreakIterator ? $bi->getLastCodePoint() : null;
            row('walk', $k, $loc, $t, $fwd, $back, $fol, $pre, $isb, $nx, $st, $cp);
            foreach ([0, 1, 2] as $kt) {
                $parts = [];
                foreach ($bi->getPartsIterator($kt) as $key => $v) $parts[] = [$key, $v];
                row('parts', $k, $loc, $t, $kt, $parts);
            }
        }
    }
}
$cp = IntlBreakIterator::createCodePointInstance();
$cp->setText("aé😀");
$seq = [];
foreach ([['first'], ['next'], ['next'], ['previous'], ['last'], ['previous'], ['following', 2], ['preceding', 2], ['isBoundary', 2], ['next', 1], ['next', -2]] as $op) {
    $r = isset($op[1]) ? $cp->{$op[0]}($op[1]) : $cp->{$op[0]}();
    $seq[] = [$op[0], $r, $cp->current(), $cp->getLastCodePoint()];
}
row('cpseq', $seq);
$w = IntlBreakIterator::createWordInstance('en');
row('rules', md5($w->getRules()), strlen($w->getRules()), substr($w->getRules(), 0, 300));
$rbbi = new IntlRuleBasedBreakIterator('$LN = [[:letter:] [:number:]];$S = [.;,:];!!forward;$LN+ {1};$S+ {42};!!reverse;$LN+ {1};$S+ {42};!!safe_forward;!!safe_reverse;');
row('rbbirules', $rbbi->getRules());
foreach (['[[', '$x = ;', 'a+ {9'] as $r) row('badrules', $r, attempt(function () use ($r) { new IntlRuleBasedBreakIterator($r); return 'ok'; }));
try { new IntlRuleBasedBreakIterator('[['); } catch (IntlException $e) { row('badmsg', $e->getMessage()); }
