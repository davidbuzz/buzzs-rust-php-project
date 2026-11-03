<?php
// Logging wrappers for every mbstring function (see gen_phpt_oracle.py).
function __mbw_enc($v) {
    if (is_string($v)) return 's' . bin2hex($v);
    if (is_int($v)) return 'i' . $v;
    if (is_float($v)) return 'd' . bin2hex(var_export($v, true));
    if ($v === true) return 'T';
    if ($v === false) return 'F';
    if ($v === null) return 'N';
    if (is_array($v)) {
        $out = [];
        foreach ($v as $k => $x) $out[] = __mbw_enc($k) . '=' . __mbw_enc($x);
        return 'A[' . implode(',', $out) . ']';
    }
    return 'O';
}
function __mbw_fh() {
    static $fh = null;
    if ($fh === null) $fh = fopen(getenv('MBW_LOG'), 'a');
    return $fh;
}
function __mbw_call($f, $args) {
    $fh = __mbw_fh();
    $diag = [];
    set_error_handler(function ($no, $str) use (&$diag) {
        $lvl = ($no === E_DEPRECATED) ? 'D' : (($no === E_NOTICE) ? 'N' : 'W');
        $diag[] = $lvl . bin2hex(preg_replace('/^[a-z_]+\(\): /', '', $str));
        return true;
    });
    $enc_args = implode("\t", array_map('__mbw_enc', $args));
    try {
        $r = $f(...$args);
        $res = '=' . __mbw_enc($r);
    } catch (Throwable $e) {
        restore_error_handler();
        fwrite($fh, $f . "\t" . $enc_args . "\t=E" . get_class($e) . ':' . bin2hex($e->getMessage()) . "\t" . implode(',', $diag) . "\n");
        throw $e;
    }
    restore_error_handler();
    fwrite($fh, $f . "\t" . $enc_args . "\t" . $res . "\t" . implode(',', $diag) . "\n");
    return $r;
}
foreach (get_extension_funcs('mbstring') as $__f) {
    if (preg_match('/^mb_(ereg|eregi|split|regex_set_options|convert_variables|parse_str|send_mail|output_handler)/', $__f)) continue;
    eval("function __mbw_$__f(...\$a) { return __mbw_call('$__f', \$a); }");
}
function __mbw_ini_set($k, $v) {
    $r = ini_set($k, $v);
    fwrite(__mbw_fh(), "#INISET\t" . $k . "\t" . bin2hex((string)$v) . "\n");
    return $r;
}
function __mbw_mb_convert_variables($to, $from, &...$vars) {
    $before = __mbw_enc($vars);
    $diag = [];
    set_error_handler(function ($no, $str) use (&$diag) {
        $lvl = ($no === E_DEPRECATED) ? 'D' : (($no === E_NOTICE) ? 'N' : 'W');
        $diag[] = $lvl . bin2hex(preg_replace('/^[a-z_]+\(\): /', '', $str));
        return true;
    });
    $head = "mb_convert_variables\t" . __mbw_enc($to) . "\t" . __mbw_enc($from) . "\t" . $before;
    try {
        $r = mb_convert_variables($to, $from, ...$vars);
    } catch (Throwable $e) {
        restore_error_handler();
        fwrite(__mbw_fh(), $head . "\t=E" . get_class($e) . ':' . bin2hex($e->getMessage()) . "\t" . implode(',', $diag) . "\n");
        throw $e;
    }
    restore_error_handler();
    fwrite(__mbw_fh(), $head . "\t=" . __mbw_enc($r) . "\t" . implode(',', $diag) . "\t" . __mbw_enc($vars) . "\n");
    return $r;
}
