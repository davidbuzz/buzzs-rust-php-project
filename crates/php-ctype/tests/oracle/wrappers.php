<?php
// Recorder for gen_oracle.php: __ctype('ctype_xxx', $arg) calls the real
// function and appends one line to $CTYPE_ORACLE_LOG.
function __ctype_arg($a): string {
    if (is_string($a)) return 's:' . bin2hex($a);
    if (is_int($a)) return "i:$a";
    if (is_float($a)) return 'd:' . var_export($a, true);
    if (is_bool($a)) return 'b:' . (int)$a;
    if ($a === null) return 'n';
    if (is_array($a)) return 'a';
    if (is_object($a)) return 'o:' . get_class($a);
    return 'r';
}
function __ctype(string $f, $a = null, ...$rest) {
    $dep = '-';
    set_error_handler(function ($no, $msg) use (&$dep) {
        if ($no === E_DEPRECATED) { $dep = $msg; return true; }
        return false;
    });
    try {
        $r = $f($a, ...$rest);
    } finally {
        restore_error_handler();
    }
    if ($rest === [] && func_num_args() == 2) {
        file_put_contents(getenv('CTYPE_ORACLE_LOG'),
            implode("\t", [$f, __ctype_arg($a), (int)$r, $dep]) . "\n", FILE_APPEND);
    }
    return $r;
}
