<?php
$operands = ['null','true','false','0','1','-1','2','7','PHP_INT_MAX','PHP_INT_MIN','1.5','-0.0','0.0','NAN','INF','-INF','""','"0"','"1"','"abc"','"1abc"','" 1"','"1 "','"1e3"','"0x1A"','"10"','"9"','"1.0"','"9223372036854775808"','"-5"','[]','[1]','[1,2]','["a"=>1]','[2]','"a"','"A"','"z"'];
$ops = ['+','-','*','/','%','**','<<','>>','|','&','^','.','<=>','==','===','<','<=','xor'];
function rep($v) {
    if (is_int($v)) return "i:$v";
    if (is_float($v)) { return "d:" . var_export($v, true); }
    if (is_string($v)) return "s:" . bin2hex($v);
    if (is_bool($v)) return "b:" . ($v ? 1 : 0);
    if (is_null($v)) return "n";
    if (is_array($v)) return "a:" . json_encode($v);
    return "?";
}
$warn = [];
set_error_handler(function($no, $str) use (&$warn) { $warn[] = ($no == E_DEPRECATED ? "D:" : "W:") . $str; return true; });
foreach ($ops as $oi => $op) {
  foreach ($operands as $ai => $a) {
    foreach ($operands as $bi => $b) {
      $isNanInf = fn($x) => in_array($x, ['NAN','INF','-INF']);
      if (in_array($op, ['%','<<','>>','|','&','^','.','xor']) && ($isNanInf($a) || $isNanInf($b))) continue;
      if (in_array($op, ['%','<<','>>','|','&','^']) && (str_contains($a, '"9223372036854775808"') || str_contains($b,'"9223372036854775808"'))) continue;
      $warn = [];
      try { $r = eval("return ($a) $op ($b);"); $out = rep($r); }
      catch (Throwable $e) { $out = "E:" . get_class($e) . ":" . $e->getMessage(); }
      $out .= $warn ? "|" . implode("|", $warn) : "";
      echo "$oi $ai $bi $out\n";
    }
  }
}
