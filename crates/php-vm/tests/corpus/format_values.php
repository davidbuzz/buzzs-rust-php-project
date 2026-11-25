<?php
class Pt implements JsonSerializable { public $x = 1.0; protected $y = [0.1]; private $z = 'z';
  function jsonSerialize(): mixed { return ['x' => $this->x, 'y' => $this->y]; } }
class Ser { public $a = 1; public $b; function __sleep() { return ['a']; } function __wakeup() { $this->b = 'woke'; } }
class Ser2 { public $d = []; function __serialize(): array { return ['d' => $this->d, 'n' => count($this->d)]; } function __unserialize(array $a): void { $this->d = $a; } }
function fmt($v) {
  $r = [];
  $r[] = sprintf('%d|%5.2f|%-6s|%06.1f|%x|%o|%b|%e|%E|%g|%G|%u|%c|%%|%+d|%\'*8s', $v, $v, $v, $v, $v, $v, $v, $v, $v, $v, $v, $v, 65, $v, $v);
  $r[] = sprintf('%2$s-%1$s', $v, 'two'); $r[] = sprintf('%.3e', $v); $r[] = sprintf('%10.4F', $v);
  $r[] = number_format((float) $v); $r[] = number_format((float) $v, 2); $r[] = number_format((float) $v, 3, ',', '.');
  $r[] = var_export($v, true); $r[] = serialize($v); $r[] = json_encode($v); $r[] = print_r($v, true); $r[] = strval($v); $r[] = (string) $v;
  ob_start(); var_dump($v); $r[] = ob_get_clean(); ob_start(); echo $v; $r[] = ob_get_clean();
  return $r;
}
function objs($i) {
  $p = new Pt; $p->x = $i / 3; $s = new Ser; $s->b = 'lost'; $s2 = new Ser2; $s2->d = range(1, $i);
  $out = [json_encode($p), json_encode([$p, 'k' => $i], JSON_PRETTY_PRINT), serialize($s), serialize($s2), var_export($p, true), print_r($p, true)];
  $u = unserialize(serialize($s)); $out[] = $u->b; $u2 = unserialize(serialize($s2)); $out[] = $u2->d;
  $out[] = json_decode('{"a":{"b":[1,2.0,"3"]},"c":1e2}', true); $out[] = json_decode('[1,{"x":null}]'); $out[] = json_encode(1.0); $out[] = json_encode([1 => 'a', 2 => 'b']); $out[] = json_encode(['a', 'b']);
  $out[] = json_encode("é\u{1F600}/", JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES); $out[] = json_encode("é/"); $out[] = json_last_error_msg();
  return $out;
}
foreach ([0, 1, -1, 1.5, -0.5, 0.1 + 0.2, 1e20, 1e-7, 123456789.123, -0.0, PHP_INT_MAX, 7E-10, 1.0, 100.0, 2.5e15, '12abc'] as $v) {
  for ($k = 0; $k < 2; $k++) { echo json_encode(@fmt($v)), "\n"; }
}
for ($i = 0; $i < 3; $i++) { var_export(objs($i)); echo "\n"; }
