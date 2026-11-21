<?php
// Helpers for generating CJK oracle fixtures with the C implementation of
// mbstring (`/usr/bin/php8.5`). Fixture scripts `require` this file and call
// conv()/check(); output goes to stdout, redirect it into a `.tsv` file.
// Replayed by `codecs::cjk::testutil::replay_oracle`.
//
// Line formats (tab-separated):
//   conv  <from> <to> <mode> <in-hex> <out-hex> <errors>
//   check <enc> <in-hex> <0|1>
// <mode>: "none", "long", "entity", or the hex code point of the
// substitute character.

function conv(string $from, string $to, string $in, $mode = 0x3F): void {
    if ($mode === 'none' || $mode === 'long' || $mode === 'entity') {
        // These modes still use the previously set substitute character for
        // bad input; replay_oracle assumes '?', so reset it first.
        mb_substitute_character(0x3F);
        mb_substitute_character($mode);
        $m = $mode;
    } else {
        mb_substitute_character($mode);
        $m = dechex($mode);
    }
    $before = mb_get_info('illegal_chars');
    $out = mb_convert_encoding($in, $to, $from);
    $errors = mb_get_info('illegal_chars') - $before;
    echo "conv\t$from\t$to\t$m\t" . bin2hex($in) . "\t" . bin2hex($out) . "\t$errors\n";
}

function check(string $enc, string $in): void {
    echo "check\t$enc\t" . bin2hex($in) . "\t" . (mb_check_encoding($in, $enc) ? 1 : 0) . "\n";
}

// The pivot encoding is UTF-8 (UTF-16/32 may not be ported yet in phprust).

// Decode $in from $enc (to UTF-8).
function dec(string $enc, string $in, $mode = 0x3F): void {
    conv($enc, 'UTF-8', $in, $mode);
}

// Encode a list of valid Unicode scalar values (ints) into $enc.
function enc_cps(string $enc, array $cps, $mode = 0x3F): void {
    $in = '';
    foreach ($cps as $cp) {
        $in .= mb_chr($cp, 'UTF-8');
    }
    conv('UTF-8', $enc, $in, $mode);
}
