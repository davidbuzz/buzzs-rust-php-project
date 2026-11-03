<?php
// Oracle fixtures for ISO-2022-JP-2004, EUC-JP-2004 and SJIS-2004.
// Usage: php jis2004_oracle.php > jis2004_oracle.tsv
//        php jis2004_oracle.php digest > jis2004_digest.tsv
require __DIR__ . '/oracle.php';

$encs = ['ISO-2022-JP-2004', 'EUC-JP-2004', 'SJIS-2004'];

if (($argv[1] ?? '') === 'digest') {
    // FNV-1a (32 bit) over the results of converting every 1- and 2-byte
    // string (and EUC-JP-2004 3-byte 0x8F strings), and every code point.
    function fnv(int $h, string $s): int {
        $n = strlen($s);
        for ($i = 0; $i < $n; $i++) {
            $h = (($h ^ ord($s[$i])) * 16777619) & 0xFFFFFFFF;
        }
        return $h;
    }
    function rec(int $h, string $in, string $from, string $to): int {
        $before = mb_get_info('illegal_chars');
        $out = mb_convert_encoding($in, $to, $from);
        $err = mb_get_info('illegal_chars') - $before;
        $chk = mb_check_encoding($in, $from) ? '1' : '0';
        return fnv($h, $out . '|' . $err . '|' . $chk . "\n");
    }
    mb_substitute_character(0x25);
    foreach ($encs as $enc) {
        $h = 0x811C9DC5;
        for ($a = 0; $a < 256; $a++) {
            $h = rec($h, chr($a), $enc, 'UTF-8');
        }
        for ($a = 0; $a < 256; $a++) {
            for ($b = 0; $b < 256; $b++) {
                $h = rec($h, chr($a) . chr($b), $enc, 'UTF-8');
            }
        }
        echo "decode2\t$enc\t" . sprintf('%08x', $h) . "\n";
    }
    $h = 0x811C9DC5;
    for ($b = 0xA1; $b < 0x100; $b++) {
        for ($c = 0; $c < 256; $c++) {
            $h = rec($h, "\x8F" . chr($b) . chr($c), 'EUC-JP-2004', 'UTF-8');
        }
    }
    echo "decode3\tEUC-JP-2004\t" . sprintf('%08x', $h) . "\n";
    foreach (['ISO-2022-JP-2004' => ["\x1B\$B", "\x1B\$(Q", "\x1B\$(P"]] as $enc => $escs) {
        foreach ($escs as $esc) {
            $h = 0x811C9DC5;
            for ($a = 0; $a < 256; $a++) {
                for ($b = 0; $b < 256; $b++) {
                    $h = rec($h, $esc . chr($a) . chr($b), $enc, 'UTF-8');
                }
            }
            echo "decode_esc\t$enc\t" . bin2hex($esc) . "\t" . sprintf('%08x', $h) . "\n";
        }
    }
    foreach ($encs as $enc) {
        $h = 0x811C9DC5;
        for ($cp = 0; $cp < 0x10000; $cp++) {
            if ($cp >= 0xD800 && $cp <= 0xDFFF) continue;
            $h = rec($h, mb_chr($cp, 'UTF-8'), 'UTF-8', $enc);
        }
        for ($cp = 0x20000; $cp < 0x2A700; $cp++) {
            $h = rec($h, mb_chr($cp, 'UTF-8'), 'UTF-8', $enc);
        }
        echo "encode1\t$enc\t" . sprintf('%08x', $h) . "\n";
        // Every pair (first code point of a combining sequence, any of a
        // set of interesting second code points)
        $firsts = [0xE6, 0x31F7];
        for ($cp = 0x254; $cp <= 0x2E9; $cp++) $firsts[] = $cp;
        for ($cp = 0x304B; $cp <= 0x3053; $cp++) $firsts[] = $cp;
        for ($cp = 0x30AB; $cp <= 0x30C8; $cp++) $firsts[] = $cp;
        $seconds = [0x41, 0x300, 0x301, 0x309A, 0x2E5, 0x2E9, 0x304B, 0x254, 0x4E00, 0x20B9F, 0xFF95, 0x3000];
        $h = 0x811C9DC5;
        foreach ($firsts as $f) {
            foreach ($seconds as $s) {
                $h = rec($h, mb_chr($f, 'UTF-8') . mb_chr($s, 'UTF-8'), 'UTF-8', $enc);
            }
        }
        echo "encode2\t$enc\t" . sprintf('%08x', $h) . "\n";
    }
    exit;
}

// --- Targeted cases ---
$modes = [0x3F, 0x25, 'none', 'long', 'entity'];

// In "long"/"entity" mode, bad input is replaced by the substitute character
// set before; the fixture format assumes '?', so make sure it is.
function dec_m(string $enc, string $in, $mode): void {
    if (is_string($mode)) mb_substitute_character(0x3F);
    dec($enc, $in, $mode);
}
function enc_cps_m(string $enc, array $cps, $mode): void {
    if (is_string($mode)) mb_substitute_character(0x3F);
    enc_cps($enc, $cps, $mode);
}

// Invalid/truncated input
$bad = [
    'ISO-2022-JP-2004' => ["\x1B", "\x1B$", "\x1B(", "\x1B$(", "\x1B$(X", "\x1B\$Bx", "\x1B\$B!", "\x1B$(Q!",
        "\x1B$(P!", "\x1B\$B!\x0A", "\x1BXY", "\x1B(J", "\xE0", "\x80\x9F", "\x1B$(Q\x24\x77", "\x1B$(Q\x2B\x44",
        "\x1B$(P\x22\x21", "\x1B$(P\x21\x22", "\x1B$(P\x7E\x7E", "\x1B$(Q\x7E\x7E\x1B(Babc", "\x1B\$B\x22\x2F",
        "\x1B$(Pa\x1B$(Qa", "\x1B$(Q\x21\x21\x1BXabc", "\x1B$(Q\x21"],
    'EUC-JP-2004' => ["\x80", "\xFE\xFF", "\xA1", "\x8E", "\x8E\xE0", "\x8F", "\x8F\xA1", "\x8F\xA2\xA1",
        "\x8F\xA1\xA1", "\x8F\xA1\x21", "\x8F\xFE\xFE", "\xA4\xF7", "\xA4\xF8", "\xAB\xC4", "\xA1\xA0abc",
        "\xFE\xFE", "\xF9\xA1", "\x8F\xA1", "\x8Fab", "\x8E\x41"],
    'SJIS-2004' => ["\x80", "\x81\x20", "\xFC\xF5", "\x81", "\xA0\x41", "\xFD\x41", "\xFE", "\xFF\x41",
        "\x82\xF5", "\x82\xF6", "\x86\x63", "\x81\x7F", "\xFC\xFC", "\xF0\x40", "\x5C\x7E", "\xE0", "\x9F\xFD"],
];
foreach ($bad as $enc => $list) {
    foreach ($list as $s) {
        foreach ($modes as $m) {
            dec_m($enc, $s, $m);
        }
        dec($enc, $s . "xyz");
        check($enc, $s);
    }
}

// Valid strings with combining sequences and charset switches
$texts = [
    "か゚き゚く゚け゚こ゚カ゚キ゚ク゚ケ゚コ゚セ゚ツ゚ト゚ㇷ゚æ̀ɔ̀ɔ́ʌ̀ʌ́ə̀ə́ɚ̀ɚ́˩˥˥˩",
    "かかかabc", "かa", "æ", "ɔ", "ɔ́", "ɚ", "˥", "˩", "˩˥x", "ㇷ", "ㇷ゚",
    "漢字テスト𠀋𡈽丂丽", "ｱｲｳ", "\\~¥‾", "，", "\u{FE45}\u{FE46}\u{F91D}\u{F928}\u{F9DC}",
    "仝〆〇ー‐／＼～∥｜…‥‘’“”（）〔〕［］｛｝〈〉《》「」『』【】＋－±×÷＝≠＜＞≦≧∞∴♂♀°′″℃￥＄￠￡％＃＆＊＠§☆★○●◎◇",
    "\u{4E02}\u{3000}\u{4E02}", "\u{20089}\u{2000B}",
    "a\u{0}b",
];
foreach ($encs as $enc) {
    foreach ($texts as $t) {
        conv('UTF-8', $enc, $t);
        $enc_bytes = mb_convert_encoding($t, $enc, 'UTF-8');
        dec($enc, $enc_bytes);
        check($enc, $enc_bytes);
    }
    // Unencodable characters in all error modes
    foreach ($modes as $m) {
        enc_cps_m($enc, [0x41, 0x80, 0x42, 0xFF95, 0x1F600, 0x304B, 0x10000, 0x43], $m);
        enc_cps_m($enc, [0x304B, 0x80], $m);
        enc_cps_m($enc, [0x254, 0x1F600], $m);
    }
    // Combining base characters at chunk boundaries (decoder buffers hold
    // 128 code points)
    foreach ([124, 125, 126, 127, 128, 129, 130, 254, 255, 256] as $n) {
        conv('UTF-8', $enc, str_repeat('a', $n) . "か゚き");
        conv('UTF-8', $enc, str_repeat('a', $n) . "か");
        conv('UTF-8', $enc, str_repeat('a', $n) . "ɔ́x");
        conv('UTF-8', $enc, str_repeat('a', $n) . "ɔ\u{300}");
        conv('UTF-8', $enc, str_repeat('漢', $n) . "か゚");
        $b = mb_convert_encoding(str_repeat('a', $n) . "か゚き" . str_repeat('漢', 3) . "ɔ́", $enc, 'UTF-8');
        dec($enc, $b);
        dec($enc, substr($b, 0, -1));
    }
}
