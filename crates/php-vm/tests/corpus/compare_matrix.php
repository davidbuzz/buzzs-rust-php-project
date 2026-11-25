<?php
class Str { function __toString(): string { return '1'; } }
function vals() { return [null, false, true, 0, 1, -1, 1.0, NAN, '', '0', '1', 'a', '1e1', ' 1', '1 ', [], [0], new stdClass, new Str]; }
function cmp($a, $b) {
  $r = ($a == $b ? 'e' : '-') . ($a === $b ? 'i' : '-') . ($a < $b ? '<' : '-') . ($a <= $b ? 'l' : '-') . ($a > $b ? '>' : '-');
  try { $r .= $a <=> $b; } catch (Throwable $e) { $r .= 'X'; }
  if ($a == $b) { $r .= 'I'; } elseif ($a != $b) { $r .= 'N'; }
  switch ($a) { case $b: $r .= 's'; break; default: $r .= 'd'; }
  $r .= match (true) { $a === $b => 'm', $a == $b => 'w', default => '.' };
  return $r;
}
function row($i) {
  $v = vals(); $line = '';
  foreach ($v as $b) { $line .= cmp($v[$i], $b) . ' '; }
  return $line;
}
set_error_handler(function ($no, $msg, $f, $l) { echo "E[$no] $msg @$l\n"; return true; });
for ($rep = 0; $rep < 3; $rep++) {
  for ($i = 0; $i < count(vals()); $i++) { echo $rep, ':', $i, ' ', row($i), "\n"; }
}
function sw($x) { switch ($x) { case 0: return 'zero'; case '1': return 'one'; case 'a': return 'a'; case null: return 'null'; default: return 'other'; } }
function mt($x) { try { return match ($x) { 0 => 'z', '0' => 's0', 1.0 => 'f1', true => 't', default => 'd' }; } catch (\UnhandledMatchError $e) { return $e->getMessage(); } }
foreach ([0, '0', '', null, false, 'a', 1, 1.0, '1', true, [], 'A'] as $x) { echo sw($x), ' ', mt($x), ' ', sw($x), ' ', mt($x), "\n"; }
