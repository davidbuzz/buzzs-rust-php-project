<?php
/*
 * Generates tests/data/oracle.tsv from the system PHP (the oracle):
 *   php -d error_reporting=-1 tools/gen_oracle.php > tests/data/oracle.tsv
 *
 * Line format (tab separated):
 *   func  arg1  arg2 ...  =>  result  diag
 * Values: s:<hex> string, i:<int>, n (null), F (false), T (true),
 *   p:<hex>,<hex>,<hex>,<int>,<hex> (mime prefs, '-' = absent),
 *   h:<name>=<v>[;...] header arrays (v = hex or [hex,hex]),
 *   c:<hex>/<hex>/... filter chunks.
 * diag: '-' or one or more 'N:msg' / 'W:msg' / 'D:msg' / 'V:msg' joined by '|'.
 */
mt_srand(20261103);
$DIAG = [];
set_error_handler(function ($no, $str) use (&$DIAG) {
    $kind = match ($no) { E_NOTICE => 'N', E_WARNING => 'W', E_DEPRECATED => 'D', default => 'E' };
    $DIAG[] = $kind . ':' . preg_replace('/^[a-z_]+\(\): /', '', $str);
    return true;
});

function enc($v) {
    if ($v === null) return 'n';
    if ($v === false) return 'F';
    if ($v === true) return 'T';
    if (is_int($v)) return 'i:' . $v;
    if (is_string($v)) return 's:' . bin2hex($v);
    if (is_array($v)) {
        $parts = [];
        foreach ($v as $k => $x) {
            $parts[] = bin2hex((string)$k) . '=' . (is_array($x) ? '[' . implode(',', array_map('bin2hex', $x)) . ']' : bin2hex($x));
        }
        return 'h:' . implode(';', $parts);
    }
    throw new Exception('bad');
}

function run($func, array $args, array $shown) {
    global $DIAG;
    $DIAG = [];
    try {
        $r = $func(...$args);
    } catch (ValueError $e) {
        $r = null;
        $DIAG[] = 'V:' . preg_replace('/^[a-z_]+\(\): /', '', $e->getMessage());
    }
    echo $func, "\t", implode("\t", $shown), "\t=>\t", enc($r), "\t", $DIAG ? implode('|', $DIAG) : '-', "\n";
}

function call($func, ...$args) {
    run($func, $args, array_map('enc', $args));
}

$utf8 = [
    '', 'a', 'abc', 'Hello, World!', "aéb€", "café crème", 'Größe', 'Привет мир', 'Καλημέρα',
    '日本語テキスト', 'ｶﾀｶﾅ', '中文字符', '한국어', "emoji 😀👍", "\u{2018}quoted\u{2019} \u{201C}x\u{201D}",
    "Œuvre æ ß ﬁ", "tab\tnl\ncr\r", "nul\0byte", str_repeat('x', 300), str_repeat('é', 150),
    "mixed ASCII and ünïcödé and 日本", "\u{00A0}nbsp\u{2026}", "™©®", "½ ¼ ¾", "→ ← ↑",
];
$bad = [
    "a\xc3", "\xe2\x82", "a\xffb", "\xc3\xc3\xc3\xb8aa", "aa\xC3\xC3\xC3", "\x80", "abc\xf0\x9f\x98",
    "\xed\xa0\x80", "\xc0\xaf", "ok\xfe\xff", "\xe9t\xe9", "\x82\xa0\x82\xa2", "\xa4\xa2\xa4\xa4",
    "\xff\xfeA\x00B\x00", "\xfe\xff\x00A\x00B", "A\x00", "\x00A\x00", random_bytes_det(7), random_bytes_det(13),
];
function random_bytes_det($n) { $s = ''; for ($i = 0; $i < $n; $i++) $s .= chr(mt_rand(0, 255)); return $s; }

$charsets = ['UTF-8', 'ISO-8859-1', 'ISO-8859-15', 'ASCII', 'UTF-16', 'UTF-16LE', 'UTF-16BE', 'UTF-32',
    'UCS-2', 'UCS-4LE', 'CP1252', 'WINDOWS-1251', 'KOI8-R', 'SJIS', 'EUC-JP', 'ISO-2022-JP', 'GB2312',
    'BIG5', 'UTF-7', 'CP437', 'MACINTOSH', 'ISO-8859-7', 'EUC-KR', 'utf8', 'latin1', 'bogus-charset'];
$suffixes = ['', '//TRANSLIT', '//IGNORE', '//TRANSLIT//IGNORE', '//IGNORE//TRANSLIT'];

// ---- iconv() ----
foreach ($utf8 as $s) {
    foreach ($charsets as $to) {
        foreach ($suffixes as $suf) {
            call('iconv', 'UTF-8', $to . $suf, $s);
        }
    }
}
foreach ($bad as $s) {
    foreach ($charsets as $from) {
        foreach (['UTF-8', 'UTF-8//IGNORE', 'ISO-8859-1', 'ASCII//TRANSLIT', 'UTF-16LE//IGNORE'] as $to) {
            call('iconv', $from, $to, $s);
        }
    }
}
// round trips from other charsets
foreach ($charsets as $from) {
    foreach ($utf8 as $s) {
        $src = @iconv('UTF-8', $from . '//TRANSLIT', $s);
        if ($src === false || $src === '') continue;
        foreach (['UTF-8', 'UTF-16BE', 'ASCII//TRANSLIT', 'ISO-8859-1//IGNORE', 'SJIS//TRANSLIT'] as $to) {
            call('iconv', $from, $to, $src);
        }
    }
}
call('iconv', str_repeat('x', 64), 'UTF-8', 'a');
call('iconv', 'UTF-8', str_repeat('x', 63), 'a');
call('iconv', '', '', 'abc');
call('iconv', 'UTF-8', '', 'abc');
call('iconv', 'ISO-8859-1', 'ASCII//TRANSLIT', "\xc9crit par %s.");

// ---- strlen / substr / strpos / strrpos ----
$encs = ['UTF-8', 'ISO-8859-1', 'UTF-16LE', 'UTF-16', 'SJIS', 'EUC-JP', 'UTF-7', 'ISO-2022-JP', 'WINDOWS-1258', 'bogus', null];
$strs = ['', 'abc', 'äöü', 'Hello, world', '日本語テキストです', 'aéb€xyz', 'test test', 'ababab', 'aaab', 'abcabcabd'];
foreach ($encs as $e) {
    $samples = [];
    foreach ($strs as $s) {
        $c = ($e === null || $e === 'bogus') ? $s : @iconv('UTF-8', $e, $s);
        if ($c !== false) $samples[] = $c;
    }
    $samples = array_merge($samples, ["a\xc3", "a\xffb", "\x82\xa0"]);
    foreach ($samples as $s) {
        $e === null ? call('iconv_strlen', $s) : call('iconv_strlen', $s, $e);
        foreach ([-7, -3, -1, 0, 1, 2, 4, 9] as $off) {
            foreach ([null, -4, -1, 0, 1, 3, 20] as $len) {
                $e === null ? call('iconv_substr', $s, $off, $len) : call('iconv_substr', $s, $off, $len, $e);
            }
        }
    }
    $hay = ['Hello, world', 'ababab', 'aaab', 'abcabcabd', '日本語テキストです日本', 'äöüäöü', 'test test', ''];
    $ndl = ['world', 'ab', 'aab', 'abd', 'abcabd', '日本', 'テ', 'üä', 'zzz', 'o', 'test test', ''];
    foreach ($hay as $h) {
        $hc = ($e === null || $e === 'bogus') ? $h : @iconv('UTF-8', $e, $h);
        if ($hc === false) continue;
        foreach ($ndl as $n) {
            $nc = ($e === null || $e === 'bogus') ? $n : @iconv('UTF-8', $e, $n);
            if ($nc === false) continue;
            foreach ([-20, -5, -1, 0, 3, 12, 13] as $off) {
                $e === null ? call('iconv_strpos', $hc, $nc, $off) : call('iconv_strpos', $hc, $nc, $off, $e);
            }
            $e === null ? call('iconv_strrpos', $hc, $nc) : call('iconv_strrpos', $hc, $nc, $e);
        }
    }
}
call('iconv_strlen', 'abc', str_repeat('x', 64));
call('iconv_substr', 'abc', 0, null, str_repeat('x', 64));
call('iconv_strpos', 'abc', 'b', 0, str_repeat('x', 64));
call('iconv_strrpos', 'abc', 'b', str_repeat('x', 64));
call('iconv_strrpos', 'abc', '', str_repeat('x', 64));
call('iconv_strpos', "a\xffb", 'b', -1);
call('iconv_strpos', "abc", 'b', PHP_INT_MIN);
call('iconv_substr', "abcdef", PHP_INT_MIN, PHP_INT_MAX);
call('iconv_substr', "abcdef", PHP_INT_MAX, PHP_INT_MIN);
call('iconv_substr', "abcdef", 2, PHP_INT_MIN);

// ---- iconv_mime_encode ----
function mime_enc($name, $value, $prefs) {
    $shown = [enc($name), enc($value)];
    $p = [];
    foreach (['scheme', 'input-charset', 'output-charset', 'line-length', 'line-break-chars'] as $k) {
        if (!array_key_exists($k, $prefs)) { $p[] = '-'; continue; }
        $p[] = $k === 'line-length' ? (string)$prefs[$k] : bin2hex($prefs[$k]);
    }
    $shown[] = 'p:' . implode(',', $p);
    run('iconv_mime_encode', [$name, $value, $prefs], $shown);
}
$names = ['Subject', 'X-Very-Long-Header-Name-Indeed', '', 'Q', str_repeat('N', 70)];
$values = ['', 'Hello', 'Prüfung Prüfung Prüfung Prüfung Prüfung Prüfung Prüfung Prüfung',
    '日本語のテキストです。長い件名をテストします。日本語のテキストです。', "a=b?c_d e\tf", str_repeat('é', 60),
    'ascii only but quite a long line that needs folding at some point for sure', "\x80", "bad\xffbyte"];
$prefsets = [];
foreach ([null, 'B', 'Q', 'q', 'x', ''] as $scheme) {
    foreach ([[], ['input-charset' => 'ISO-8859-1'], ['output-charset' => 'ISO-2022-JP'], ['output-charset' => 'UTF-16'], ['output-charset' => 'ISO-8859-1'], ['output-charset' => 'bogus']] as $cs) {
        foreach ([null, 5, 20, 30, 50, 76, 998] as $ll) {
            foreach ([null, "\n", ''] as $lf) {
                $p = $cs;
                if ($scheme !== null) $p['scheme'] = $scheme;
                if ($ll !== null) $p['line-length'] = $ll;
                if ($lf !== null) $p['line-break-chars'] = $lf;
                $prefsets[] = $p;
            }
        }
    }
}
foreach ($names as $ni => $n) {
    foreach ($values as $vi => $v) {
        foreach ($prefsets as $pi => $p) {
            if (($ni * 7 + $vi * 3 + $pi) % ($ni === 0 ? 4 : 17) !== 0) continue;
            mime_enc($n, $v, $p);
        }
    }
}
mime_enc('Subject', 'x', ['input-charset' => str_repeat('x', 64)]);
mime_enc('Subject', 'x', ['output-charset' => str_repeat('x', 64)]);
mime_enc('Subject', "\x80", ['input-charset' => 'UTF-8', 'output-charset' => 'UTF-8']);

// ---- iconv_mime_decode / headers ----
$tokens = ['=?UTF-8?B?SGVsbG8gV29ybGQ=?=', '=?ISO-8859-1?Q?caf=E9_cr=E8me?=', '=?utf-8?q?Pr=C3=BCfung?=',
    '=?bogus?Q?x?=', '=?utf-8*en?Q?lang?=', '=?UTF-8?X?a?=', '=?UTF-8?B?', '=?UTF-8?Q?a=ZZ?=', '=?UTF-8?Q?=A1?=',
    '=?ISO-2022-JP?B?GyRCRnxLXDhsGyhC?=', '=?UTF-8?B?5pel5pys6Kqe?=', ' ', '  ', "\t", "\r\n ", "\n\t", "\r\n",
    "\n", "\r", 'plain', 'text', '=', '?', '=?', '=?x', '?=', ':', 'Subject: ', 'To: ', '=?UTF-8?Q?a_b?=',
    '=?UTF-8?Q?soft=\r\n?=', '=?' . str_repeat('c', 85) . '?Q?x?=', '=?UTF-8?B?w6k=?=x', 'x=?UTF-8?B?w6k=?=',
    "\xff", 'é', '=?windows-1258?Q?DSI_Charg=E9?=', '=?UTF-8?Q?=?=', '=?UTF-8??a?=', '=?UTF-8?Q?a?b', "=?UTF-8?Q?a?=\n"];
$modes = [0, 1, 2, 3];
for ($i = 0; $i < 3000; $i++) {
    $n = mt_rand(1, 6);
    $s = '';
    for ($j = 0; $j < $n; $j++) $s .= $tokens[mt_rand(0, count($tokens) - 1)];
    $mode = $modes[mt_rand(0, 3)];
    $cs = ['UTF-8', 'ISO-8859-1', 'UTF-16BE', 'bogus', null][mt_rand(0, 4)];
    $cs === null ? call('iconv_mime_decode', $s, $mode) : call('iconv_mime_decode', $s, $mode, $cs);
}
foreach ($tokens as $t) foreach ($modes as $m) call('iconv_mime_decode', $t, $m, 'UTF-8');
call('iconv_mime_decode', 'x', 0, str_repeat('x', 64));
call('iconv_mime_decode', "=?utf-8?Q?\xA1?= .");

$hlines = ["Subject: =?UTF-8?B?SGVsbG8=?=", "To: a@b.c", "Received: one", "Received: two", "Received: three",
    "X-Folded: part1\r\n part2", "123: numeric", "123: again", "NoColon", "Empty:", "Spaced:   \t value",
    "Q: =?ISO-8859-1?Q?caf=E9?= =?ISO-8859-1?Q?_au_lait?=", "Bad: =?UTF-8?X?a?=", "Bin: \xff", "Lone: =?"];
for ($i = 0; $i < 1500; $i++) {
    $n = mt_rand(1, 5);
    $parts = [];
    for ($j = 0; $j < $n; $j++) $parts[] = $hlines[mt_rand(0, count($hlines) - 1)];
    $sep = ["\r\n", "\n", "\r\n\r\n"][mt_rand(0, 2)];
    $s = implode($sep, $parts) . (mt_rand(0, 1) ? $sep : '');
    $mode = $modes[mt_rand(0, 3)];
    $cs = ['UTF-8', 'ISO-8859-1', null][mt_rand(0, 2)];
    $cs === null ? call('iconv_mime_decode_headers', $s, $mode) : call('iconv_mime_decode_headers', $s, $mode, $cs);
}
call('iconv_mime_decode_headers', 'a: b', 0, str_repeat('x', 64));

// ---- stream filter ----
function filt($name, array $chunks) {
    global $DIAG;
    $DIAG = [];
    $fp = fopen('php://memory', 'w+');
    $f = @stream_filter_append($fp, $name, STREAM_FILTER_WRITE);
    if ($f === false) {
        $r = false;
    } else {
        foreach ($chunks as $c) fwrite($fp, $c);
        stream_filter_remove($f);
        rewind($fp);
        $r = stream_get_contents($fp);
    }
    echo "filter\ts:", bin2hex($name), "\tc:", implode('/', array_map('bin2hex', $chunks)), "\t=>\t", enc($r), "\t", $DIAG ? implode('|', $DIAG) : '-', "\n";
}
$fsamples = ['Hello', 'äöü日本語', "😀 emoji", "a\xffb", str_repeat('é', 80), "abc\xc3"];
foreach (['convert.iconv.UTF-8.UTF-16LE', 'convert.iconv.UTF-8/ISO-8859-1', 'convert.iconv.UTF-8.ISO-2022-JP',
          'convert.iconv.UTF-8/UTF-7', 'convert.iconv.UTF-8.ASCII//TRANSLIT', 'convert.iconv.UTF-8/UTF-8//IGNORE',
          'convert.iconv.bogus.UTF-8', 'convert.iconv.UTF-8', 'convert.iconv.UTF-16BE.UTF-8'] as $name) {
    foreach ($fsamples as $s) {
        filt($name, [$s]);
        for ($k = 1; $k < strlen($s) && $k < 12; $k++) {
            filt($name, [substr($s, 0, $k), substr($s, $k)]);
        }
        filt($name, str_split($s, 1));
    }
}
filt('convert.iconv.UTF-16BE.UTF-8', ['test']);
filt('convert.iconv.ucs-2/utf8//IGNORE', ['abc']);
