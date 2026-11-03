<?php
// Generates euc_blocks.tsv: block-wise CRC32 oracle for the EUC-family and
// ISO-2022-KR codecs (exhaustive byte-pattern sweeps and BMP encode sweeps).
// Run: php euc_blocks.php > euc_blocks.tsv
// Line format (tab-separated):
//   blk <from> <to> <mode> <input-spec> <crc32-hex> <out-len> <errors>
// <input-spec>:
//   pairs:<prefix-hex>:<suffix-hex>  concat over b=0..255 of prefix.chr(b).suffix
//   cps:<start-hex>:<count>          UTF-8 of code points start..start+count-1
//                                    (surrogates skipped)
// Replayed by `replay_blocks` in src/codecs/cjk/euc.rs tests.

function spec_input(string $spec): string {
    $p = explode(':', $spec);
    if ($p[0] === 'pairs') {
        $pre = hex2bin($p[1]);
        $suf = hex2bin($p[2]);
        $s = '';
        for ($b = 0; $b < 256; $b++) {
            $s .= $pre . chr($b) . $suf;
        }
        return $s;
    }
    $start = hexdec($p[1]);
    $s = '';
    for ($cp = $start; $cp < $start + (int)$p[2]; $cp++) {
        if ($cp >= 0xD800 && $cp <= 0xDFFF) continue;
        $s .= mb_chr($cp, 'UTF-8');
    }
    return $s;
}

function blk(string $from, string $to, string $spec, $mode = 0x25): void {
    // Named modes keep the previous substitute character; use '?' for them.
    mb_substitute_character(0x3F);
    mb_substitute_character($mode);
    $m = is_int($mode) ? dechex($mode) : $mode;
    $before = mb_get_info('illegal_chars');
    $out = mb_convert_encoding(spec_input($spec), $to, $from);
    $errors = mb_get_info('illegal_chars') - $before;
    printf("blk\t%s\t%s\t%s\t%s\t%08x\t%d\t%d\n", $from, $to, $m, $spec, crc32($out), strlen($out), $errors);
}

$encs = ['EUC-JP', 'eucJP-win', 'CP51932', 'EUC-CN', 'EUC-TW', 'EUC-KR', 'UHC'];

foreach ($encs as $enc) {
    // Decode sweeps: every lead byte followed by every second byte
    for ($c1 = 0x80; $c1 <= 0xFF; $c1++) {
        blk($enc, 'UTF-8', sprintf('pairs:%02x:0a', $c1));
        blk($enc, 'UTF-8', sprintf('pairs:%02x:', $c1));
    }
    // Encode sweeps over the BMP and a few supplementary blocks
    for ($cp = 0; $cp < 0x10000; $cp += 0x100) {
        if ($cp >= 0xD800 && $cp < 0xE000) continue;
        blk('UTF-8', $enc, sprintf('cps:%x:256', $cp));
    }
    foreach ([0x10000, 0x1F300, 0x20000, 0x2A600, 0xF0000, 0x10FF00] as $cp) {
        blk('UTF-8', $enc, sprintf('cps:%x:256', $cp));
    }
    blk('UTF-8', $enc, 'cps:2000:256', 'long');
    blk('UTF-8', $enc, 'cps:2000:256', 'entity');
    blk('UTF-8', $enc, 'cps:2000:256', 'none');
}

// Three-byte forms
foreach (['EUC-JP', 'eucJP-win'] as $enc) {
    for ($c2 = 0x80; $c2 <= 0xFF; $c2++) {
        blk($enc, 'UTF-8', sprintf('pairs:8f%02x:0a', $c2));
    }
}
foreach (['a1', 'a2', 'ae', 'a3', 'b0'] as $c2) {
    for ($c3 = 0x80; $c3 <= 0xFF; $c3++) {
        blk('EUC-TW', 'UTF-8', sprintf('pairs:8e%s%02x:0a', $c2, $c3));
    }
}

// ISO-2022-KR: ASCII mode, KSC5601 mode, escape sequences
blk('ISO-2022-KR', 'UTF-8', 'pairs::0a');
blk('ISO-2022-KR', 'UTF-8', 'pairs:1b2429:0a');
blk('ISO-2022-KR', 'UTF-8', 'pairs:1b24:0a');
blk('ISO-2022-KR', 'UTF-8', 'pairs:1b:0a');
blk('ISO-2022-KR', 'UTF-8', 'pairs:1b:');
for ($c1 = 0x00; $c1 <= 0xFF; $c1++) {
    blk('ISO-2022-KR', 'UTF-8', sprintf('pairs:0e%02x:0f0a', $c1));
}
for ($cp = 0; $cp < 0x10000; $cp += 0x100) {
    if ($cp >= 0xD800 && $cp < 0xE000) continue;
    blk('UTF-8', 'ISO-2022-KR', sprintf('cps:%x:256', $cp));
}
blk('UTF-8', 'ISO-2022-KR', 'cps:2000:256', 'long');
