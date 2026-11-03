<?php
// Generates oracle.tsv: a sequence of mb_* calls and their results under
// /usr/bin/php. Replayed by tests/strings_oracle.rs.
// Usage: php gen_oracle.php > oracle.tsv
mt_srand(12345);
$diag = [];
set_error_handler(function ($no, $str) use (&$diag) {
    if ($no === E_DEPRECATED || $no === E_USER_DEPRECATED) return true;
    $diag[] = preg_replace('/^[a-z_]+\(\): /', '', $str);
    return true;
});

function enc($v) {
    if (is_string($v)) return 's' . bin2hex($v);
    if (is_int($v)) return 'i' . $v;
    if ($v === true) return 'T';
    if ($v === false) return 'F';
    if ($v === null) return 'N';
    if (is_array($v)) return 'A' . implode(',', array_map('enc', array_values($v)));
    throw new Exception('bad');
}

function call($f, ...$args) {
    global $diag;
    $diag = [];
    try {
        $r = enc($f(...$args));
    } catch (Throwable $e) {
        $r = 'E' . get_class($e) . ':' . bin2hex($e->getMessage());
    }
    echo $f, "\t", implode("\t", array_map('enc', $args)), "\t=", $r, "\t", implode(',', array_map('bin2hex', $diag)), "\n";
}

$texts = [
    '', 'a', 'Hello World', 'héllo wörld', 'ΣΑΣ ΟΔΥΣΣΕΥΣ', 'ǅungla ǆ ǈ', 'straße ﬃ ŉ',
    '日本語テキストです。', 'ｱｲｳｴｵ ｶﾞｷﾞ', '한국어 텍스트', '中文简体字', '😀 emoji 𝔸𝕓',
    "tab\tnl\n", 'İstanbul ıi', 'ǰ̌ ΐ ﬀ', str_repeat('ab日', 50), str_repeat('Σα ', 40),
];
$encodings = ['UTF-8', 'UTF-16', 'UTF-16LE', 'UTF-32BE', 'UCS-2', 'ISO-8859-1', 'ISO-8859-7', 'ISO-8859-9',
    'Windows-1252', 'SJIS', 'EUC-JP', 'CP932', 'ISO-2022-JP', 'JIS', 'GB18030', 'BIG-5', 'EUC-KR', 'UHC',
    'UTF-7', 'UTF7-IMAP', 'ASCII', 'KOI8-R', 'CP936', 'HZ', 'SJIS-mac', 'EUC-TW', 'CP50220', 'ISO-2022-KR'];
$invalid = ["\xFF\xFEab", "a\xC3", "\xE3\x81", "abc\x80def", "\x1b\$Bx", "+AGE-x+", "\x81\x40\x81", "\xA4"];

// samples per encoding
$samples = [];
foreach ($encodings as $e) {
    foreach ($texts as $t) {
        $c = @mb_convert_encoding($t, $e, 'UTF-8');
        $samples[$e][] = $c;
    }
    foreach ($invalid as $b) $samples[$e][] = $b;
}

foreach ($encodings as $e) {
    foreach ($samples[$e] as $s) {
        call('mb_strlen', $s, $e);
        call('mb_strwidth', $s, $e);
        call('mb_check_encoding', $s, $e);
        call('mb_scrub', $s, $e);
        call('mb_substr', $s, 1, 3, $e);
        call('mb_substr', $s, -3, null, $e);
        call('mb_substr', $s, 2, -2, $e);
        call('mb_str_split', $s, 2, $e);
        call('mb_strtoupper', $s, $e);
        call('mb_strtolower', $s, $e);
        for ($m = 0; $m < 8; $m++) call('mb_convert_case', $s, $m, $e);
        call('mb_convert_encoding', $s, 'UTF-8', $e);
        if (strlen($s) > 0) {
            call('mb_ord', $s, $e);
            $l = strlen($s);
            foreach ([[0, 5], [1, 4], [3, 20], [$l >> 1, 7], [-5, null], [2, -3]] as [$f, $n]) {
                if ($n !== 0) call('mb_strcut', $s, $f, $n, $e);
            }
            call('mb_strimwidth', $s, 0, 6, '..', $e);
            call('mb_strimwidth', $s, 1, 4, '', $e);
        }
    }
}

// Search functions
$hay = ['Hello World hello world', 'ΣΑΣ σας ΣΑΣ', '日本語の日本語', 'aaa', 'İi ıI', "a\xFFb\xFFc"];
$needles = ['o', 'world', 'ΣΑΣ', 'σας', '日本', 'a', 'aa', 'i', 'I', "\xFF", 'zz', 'Hello World hello world!'];
foreach (['UTF-8', 'SJIS', 'UTF-16LE', 'ISO-8859-1', 'EUC-JP'] as $e) {
    foreach ($hay as $h0) {
        $h = @mb_convert_encoding($h0, $e, 'UTF-8');
        foreach ($needles as $n0) {
            $n = @mb_convert_encoding($n0, $e, 'UTF-8');
            foreach ([0, 2, -2, -1, 100, -100] as $off) {
                call('mb_strpos', $h, $n, $off, $e);
                call('mb_strrpos', $h, $n, $off, $e);
                call('mb_stripos', $h, $n, $off, $e);
                call('mb_strripos', $h, $n, $off, $e);
            }
            foreach ([false, true] as $b) {
                call('mb_strstr', $h, $n, $b, $e);
                call('mb_strrchr', $h, $n, $b, $e);
                call('mb_stristr', $h, $n, $b, $e);
                call('mb_strrichr', $h, $n, $b, $e);
            }
            call('mb_substr_count', $h, $n, $e);
        }
    }
}

// Conversion with substitution modes
$subst = ['none', 'long', 'entity', 0x3013, 63, 0x1F600];
$targets = ['ASCII', 'ISO-8859-1', 'SJIS', 'ISO-2022-JP', 'UTF-7', 'HZ', 'UTF-16BE', 'GB18030', 'CP1252', 'ArmSCII-8', 'UTF7-IMAP'];
foreach ($subst as $sc) {
    call('mb_substitute_character', $sc);
    foreach ($targets as $t) {
        foreach (['héllo wörld', '日本語 ok', '😀x', "bad\xFFutf8", 'ΣΑΣ'] as $s) {
            call('mb_convert_encoding', $s, $t, 'UTF-8');
        }
        call('mb_chr', 0x3042, $t);
        call('mb_chr', 0x41, $t);
    }
    call('mb_strtoupper', "x\xFFy", 'UTF-8');
    call('mb_str_split', "ab\xFFcd", 3, 'UTF-7');
}
call('mb_substitute_character', 63);
call('mb_substitute_character', 'bogus');
call('mb_substitute_character', 0xD800);
call('mb_substitute_character');

// Detection
$cands = [['ASCII', 'UTF-8'], ['UTF-8', 'SJIS', 'EUC-JP'], ['SJIS', 'EUC-JP', 'UTF-8', 'JIS'], ['ISO-8859-1', 'UTF-8'],
    ['UTF-8', 'ISO-8859-1'], ['UTF-8', 'UTF-16', 'UTF-16LE', 'UTF-16BE'], ['GB18030', 'BIG-5', 'UTF-8', 'EUC-KR'],
    ['Windows-1251', 'KOI8-R', 'CP866', 'UTF-8'], ['UTF-7', 'ASCII', 'UTF-8'], ['HZ', 'ASCII'], ['BASE64', 'UTF-8']];
foreach ($encodings as $e) {
    foreach (array_slice($samples[$e], 0, 18) as $s) {
        foreach ($cands as $c) {
            call('mb_detect_encoding', $s, $c, true);
            call('mb_detect_encoding', $s, $c, false);
            call('mb_detect_encoding', $s, implode(',', $c));
        }
    }
}
call('mb_detect_encoding', 'abc', 'UTF-8, bogus');
call('mb_detect_encoding', 'abc', []);
call('mb_detect_encoding', 'abc', ['BASE64']);
call('mb_detect_encoding', "\xEF\xBB\xBFabc", ['UTF-8', 'ASCII'], true);

// Numeric entities
$maps = [[0x0, 0x10FFFF, 0, 0x1FFFFF], [0x80, 0xFFFF, 0, 0xFFFF], [0x41, 0x5A, 1, 0xFF], [0x80, 0x10FFFF, 0x100, 0x1FFFFF]];
foreach ($maps as $m) {
    foreach (['UTF-8', 'SJIS', 'ISO-8859-1'] as $e) {
        foreach (['abcXYZ', 'héllo€', '日本😀', "&amp; &#65; &#x41; &#; &#x; &#99999999999; &#4294967295; &#x110000; tail&#66"] as $s0) {
            $s = @mb_convert_encoding($s0, $e, 'UTF-8');
            call('mb_encode_numericentity', $s, $m, $e, false);
            call('mb_encode_numericentity', $s, $m, $e, true);
            call('mb_decode_numericentity', $s, $m, $e);
        }
    }
}
call('mb_decode_numericentity', str_repeat('a', 120) . '&#x1F600;&#128512;' . str_repeat('&#65;', 40), [0, 0x10FFFF, 0, 0x1FFFFF], 'UTF-8');
call('mb_encode_numericentity', 'abc', [1, 2, 3], 'UTF-8');

// MIME headers
foreach (['UTF-8', 'ISO-2022-JP', 'SJIS', 'ISO-8859-1', 'UTF-16', 'GB18030'] as $cs) {
    foreach (['B', 'Q'] as $te) {
        foreach (['Subject: Hello', 'ascii only words that go on and on and on and on and on and on and on forever and ever amen', '日本語のテキストです。日本語のテキストです。日本語のテキストです。', 'Mixed 日本 text with words ' . str_repeat('xyz ', 20), "Ünïcödé=?_ x", '  leading spaces 日本'] as $s) {
            foreach ([0, 10, 70] as $indent) {
                call('mb_encode_mimeheader', $s, $cs, $te, "\r\n", $indent);
            }
            call('mb_encode_mimeheader', $s, $cs, $te, "\n", 0);
        }
    }
}
foreach (['=?UTF-8?B?5pel5pys6Kqe?=', '=?ISO-2022-JP?B?GyRCRnxLXDhsGyhC?= =?ISO-2022-JP?B?GyRCRnxLXBsoQg==?=', 'a =?UTF-8?Q?=E6=97=A5_x?= b', "x\r\n y\r\n", '=?bogus?B?xx?=', '=?UTF-8?X?abc?=', '=?UTF-8?B?5pel', '=?ISO-8859-1?q?caf=E9?= =?ISO-8859-1?q?_ok?=', '=?UTF-8?B?!!!!?='] as $s) {
    call('mb_decode_mimeheader', $s);
}

// str_pad
foreach (['', 'abc', '日本'] as $s) {
    foreach ([0, 5, 8] as $len) {
        foreach (['-', '日本語', 'ab'] as $pad) {
            foreach ([STR_PAD_LEFT, STR_PAD_RIGHT, STR_PAD_BOTH] as $t) {
                call('mb_str_pad', $s, $len, $pad, $t, 'UTF-8');
            }
        }
    }
}
call('mb_str_pad', 'a', 5, '', STR_PAD_LEFT, 'UTF-8');
call('mb_str_pad', 'a', 5, 'x', 7, 'UTF-8');

// Kana
foreach (['KV', 'k', 'h', 'A', 'a', 'R', 'r', 'N', 'n', 'S', 's', 'C', 'c', 'H', 'HV', 'KVA'] as $mode) {
    foreach (['UTF-8', 'SJIS', 'EUC-JP'] as $e) {
        $s = mb_convert_encoding('ｱｲｳｶﾞﾊﾟ あいう アイウ ＡＢＣ１２３ ABC123　', $e, 'UTF-8');
        call('mb_convert_kana', $s, $mode, $e);
    }
}
call('mb_convert_kana', 'x', 'Q', 'UTF-8');
call('mb_convert_kana', 'x', 'Rr', 'UTF-8');

// misc errors / edge cases
call('mb_strlen', 'abc', 'bogus');
call('mb_substr_count', 'abc', '', 'UTF-8');
call('mb_substr_count', 'abc', "\x1b(B", 'ISO-2022-JP');
call('mb_str_split', 'abc', 0, 'UTF-8');
call('mb_str_split', 'abc', 5, 'UTF-8');
call('mb_strimwidth', 'abc', 5, 1, '', 'UTF-8');
call('mb_strimwidth', 'abcdef', -2, 2, '', 'UTF-8');
call('mb_strimwidth', '日本語テキスト', 0, 7, '…', 'UTF-8');
call('mb_strimwidth', '日本語テキスト', 0, 1, '……', 'UTF-8');
call('mb_strpos', 'abc', 'b', 4, 'UTF-8');
call('mb_strpos', 'abc', 'b', -4, 'UTF-8');
call('mb_strrpos', 'abc', 'b', -4, 'UTF-8');
call('mb_ord', '', 'UTF-8');
call('mb_ord', 'abc', 'UTF-7');
call('mb_chr', 0xD800, 'UTF-8');
call('mb_chr', 0x110000, 'UTF-8');
call('mb_chr', 0x41, 'JIS');
call('mb_convert_case', 'abc', 8, 'UTF-8');
call('mb_preferred_mime_name', 'sjis-win');
call('mb_preferred_mime_name', 'UTF7-IMAP');
call('mb_preferred_mime_name', 'bogus');
call('mb_encoding_aliases', 'ASCII');
call('mb_encoding_aliases', 'UTF-16');
call('mb_internal_encoding', 'EUC-JP');
call('mb_internal_encoding');
call('mb_strlen', "\xA4\xA2\xA4\xA4");
call('mb_internal_encoding', 'bogus');
call('mb_internal_encoding', 'UTF-8');
call('mb_language', 'Japanese');
call('mb_detect_order', 'auto');
call('mb_detect_order');
call('mb_language', 'neutral');
call('mb_detect_order', ['ASCII', 'UTF-8']);
call('mb_detect_order', 'bogus');
call('mb_http_output');
call('mb_http_output', 'pass');
call('mb_http_output');
call('mb_http_input', 'L');
call('mb_http_input', 'X');
call('mb_regex_encoding');
call('mb_regex_encoding', 'EUC-JP');
call('mb_regex_encoding');
call('mb_regex_encoding', 'UTF-16');
call('mb_regex_encoding', 'CP936');
call('mb_language', 'bogus');
call('mb_list_encodings');
