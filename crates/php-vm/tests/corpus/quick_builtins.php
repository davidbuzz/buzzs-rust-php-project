<?php
set_error_handler(function ($no, $msg, $f, $l) { echo "E[$no] $msg @$l\n"; return true; });
function maths($v) {
  $r = [];
  try { $r[] = abs($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = ceil($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = floor($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = sqrt($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = exp($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = log($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = log10($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = sin($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = cos($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = tan($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = acos($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = asinh($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = is_nan($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = is_finite($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = is_infinite($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = deg2rad($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = rad2deg($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = is_numeric($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = dechex($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = chr($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = ord($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = expm1($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = log1p($v); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = round($v); $r[] = round($v, 2); $r[] = round($v, -1); } catch (Throwable $e) { $r[] = $e->getMessage(); }
  try { $r[] = intdiv($v, 3); $r[] = fmod($v, 3); $r[] = fdiv($v, 0); $r[] = pow($v, 2); $r[] = $v ** 0.5; $r[] = atan2($v, 1); $r[] = hypot($v, 4); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = max($v, 2); $r[] = min($v, 2); $r[] = max([$v, '10', 9.5]); $r[] = min('abc', $v); } catch (Throwable $e) { $r[] = $e->getMessage(); }
  return $r;
}
function strs($s) {
  $r = [];
  try {
    $r[] = trim($s); $r[] = trim($s, 'a..c'); $r[] = rtrim($s, "x\n"); $r[] = ltrim($s, ' 0'); $r[] = chop($s);
    $r[] = strpos($s, 'a'); $r[] = strpos($s, 'a', -2); $r[] = strstr($s, 'b'); $r[] = strstr($s, 'b', true);
    $r[] = substr($s, 1); $r[] = substr($s, -2, 1); $r[] = substr($s, 10); $r[] = substr($s, 1, -10);
    $r[] = str_contains($s, ''); $r[] = str_starts_with($s, ' '); $r[] = str_ends_with($s, 'c');
    $r[] = strtolower($s); $r[] = strtoupper($s); $r[] = strtr($s, 'ab', 'xy'); $r[] = strtr($s, ['a' => 'AA', 'ab' => 'Z']);
    $r[] = str_replace(['a', 'b'], ['b', 'c'], $s); $r[] = implode('-', str_split($s . 'x')); $r[] = dirname("/p/$s/q");
    $r[] = in_array($s, ['0', 0, null]); $r[] = in_array($s, ['0', 0, null], true); $r[] = in_array(0, [$s]);
    $r[] = preg_match('/^\s*(\w)/', $s, $m); $r[] = $m;
  } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  try { $r[] = strpos($s, 'a', 99); } catch (Throwable $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
  return $r;
}
$vals = [4, -4.5, '9', '1e2', ' 16', '16 ', 'abc', true, null, 0, -0.0, PHP_INT_MIN, 2.5, NAN, INF, '0x10', 3.5];
for ($rep = 0; $rep < 3; $rep++) {
  foreach ($vals as $v) { echo var_export($v, true), ': ', json_encode(maths($v), JSON_PARTIAL_OUTPUT_ON_ERROR | JSON_PRESERVE_ZERO_FRACTION), "\n"; }
}
foreach (['  abc  ', "abcx\n", '00a1b', '', 'ABCabc', 'a', 0, 1.5, null, true] as $s) {
  for ($rep = 0; $rep < 3; $rep++) { var_export(strs($s)); echo "\n"; }
}
