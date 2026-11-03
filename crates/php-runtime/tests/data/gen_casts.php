<?php
// Generates tests/data/casts_table.txt: php tests/data/gen_casts.php > tests/data/casts_table.txt
$operands = ['null','true','false','0','1','-1','PHP_INT_MAX','PHP_INT_MIN','1.5','-1.5','-0.0','1e20','-1e20','1.9e19','0.1+0.2','1e15','1e14','123456789012345.678','1e-5','0.0001','""','"0"','"0.0"','"abc"','"1abc"','" 1"','"1 "','"1e3"','"0x1A"','"1e20"','"-1e20"','" 12.5e1xyz"','"9223372036854775808"','"-9223372036854775809"','".5"','"5."','[]','[0]'];
$casts = ['int', 'float', 'string', 'bool'];
function rep($v) {
    if (is_int($v)) return "i:$v";
    if (is_float($v)) return "d:" . var_export($v, true);
    if (is_string($v)) return "s:" . bin2hex($v);
    if (is_bool($v)) return "b:" . ($v ? 1 : 0);
    return "?";
}
$warn = [];
set_error_handler(function($no, $str) use (&$warn) { $warn[] = ($no == E_DEPRECATED ? "D:" : "W:") . $str; return true; });
foreach ($casts as $ci => $c) {
    foreach ($operands as $ai => $a) {
        $warn = [];
        try { $r = eval("return ($c)($a);"); $out = rep($r); }
        catch (Throwable $e) { $out = "E:" . get_class($e) . ":" . $e->getMessage(); }
        $out .= $warn ? "|" . implode("|", $warn) : "";
        echo "$ci $ai $out\n";
    }
}
