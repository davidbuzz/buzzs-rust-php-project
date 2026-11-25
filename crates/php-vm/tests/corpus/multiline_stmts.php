<?php
set_error_handler(function ($no, $msg, $f, $l) { echo "$msg @$l\n"; return true; });
function tr() { $t = (new Exception)->getTrace(); return array_map(fn($f) => ($f['function'] ?? '') . '@' . ($f['line'] ?? '-'), $t); }
class O { function m($a = null, $b = null) { return tr(); } static function s() { return tr(); } function __get($n) { return tr(); } }
function f($o) {
  if (
    $u1
  ) {}
  while (
    $u2
  ) {}
  switch (
    $u3
  ) { default: }
  $a = $u4
    ?? $u5;
  $b = isset(
    $u6['x']
  );
  $c = [
    ...($u7 ?? [])
  ];
  echo json_encode(tr(
  )), "\n";
  echo json_encode($o->m(
    1,
    2
  )), "\n";
  echo json_encode(O::s(
  )), "\n";
  echo json_encode($o
    ->m()), "\n";
  echo json_encode($o
    ->prop), "\n";
  echo json_encode(array_map(
    fn($x) => tr(),
    [1]
  )), "\n";
  $d = $u8 ?:
    $u9;
  $e = $u10 ==
    $u11;
  do {} while (
    $u12
  );
  for ($i = 0;
    $i < $u13;
    $i++) {}
}
for ($i = 0; $i < 3; $i++) { f(new O); }
