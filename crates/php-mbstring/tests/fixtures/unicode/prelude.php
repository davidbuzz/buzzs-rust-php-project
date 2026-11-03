<?php
// Logging wrappers substituted for mb_convert_encoding/mb_check_encoding/
// mb_strcut by gen_oracle.py; each call made by a .phpt is recorded with its
// (oracle-computed) result.
$GLOBALS['__L_fh'] = fopen(getenv('ORACLE_LOG'), 'a');
$GLOBALS['__L_char'] = is_int(mb_substitute_character()) ? mb_substitute_character() : 63;
function __L_subst($c = null) {
    if ($c === null) { return mb_substitute_character(); }
    $r = mb_substitute_character($c);
    if ($r && is_int($c)) { $GLOBALS['__L_char'] = $c; }
    return $r;
}
function __L_sub() {
    $s = mb_substitute_character();
    return (is_int($s) ? 'char' : strtolower($s)) . ':' . $GLOBALS['__L_char'];
}
function __L_w(...$f) { fwrite($GLOBALS['__L_fh'], implode("\t", $f) . "\n"); }
function __L_single($e) { return is_string($e) && strpos($e, ',') === false && strtolower($e) !== 'auto'; }
function __L_conv($str, $to, $from = null) {
    $r = mb_convert_encoding($str, $to, $from);
    if ($from === null) { $from = mb_internal_encoding(); }
    if (is_string($str) && is_string($r) && __L_single($to) && __L_single($from)) {
        __L_w('conv', $from, $to, __L_sub(), bin2hex($str), bin2hex($r));
    }
    return $r;
}
function __L_check($value = null, $encoding = null) {
    $r = mb_check_encoding($value, $encoding);
    if ($encoding === null) { $encoding = mb_internal_encoding(); }
    if (is_string($value) && __L_single($encoding)) {
        __L_w('check', $encoding, bin2hex($value), $r ? '1' : '0');
    }
    return $r;
}
function __L_cut($str, $start, $length = null, $encoding = null) {
    $r = mb_strcut($str, $start, $length, $encoding);
    if ($encoding === null) { $encoding = mb_internal_encoding(); }
    __L_w('cut', $encoding, (string)$start, $length === null ? 'null' : (string)$length, bin2hex($str), bin2hex($r));
    return $r;
}
