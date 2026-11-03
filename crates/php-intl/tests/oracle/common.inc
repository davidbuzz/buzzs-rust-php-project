<?php
// Shared helpers for the oracle generators: one case per line, fields
// separated by TAB; fields are escaped (\\ \t \n \r) so they fit on a line.
function esc($v): string {
    if ($v === null) return '~N';
    if ($v === false) return '~F';
    if ($v === true) return '~T';
    if (is_int($v)) return 'i:' . $v;
    if (is_float($v)) return 'd:' . var_export($v, true);
    if (is_array($v)) return 'a:' . json_encode($v, JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES | JSON_PRESERVE_ZERO_FRACTION | JSON_INVALID_UTF8_SUBSTITUTE);
    return 's:' . strtr((string)$v, ["\\" => "\\\\", "\t" => "\\t", "\n" => "\\n", "\r" => "\\r"]);
}
function row(...$fields) {
    echo implode("\t", array_map('esc', $fields)), "\n";
}
// Run $f; return its result or "!<code>" on intl failure.
function attempt(callable $f) {
    try {
        $r = $f();
    } catch (Throwable $e) {
        return '!' . get_class($e);
    }
    if ($r === false || $r === null) {
        return '!' . intl_error_name(intl_get_error_code());
    }
    return $r;
}
