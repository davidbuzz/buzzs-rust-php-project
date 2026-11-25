<?php
class A { public $x = 'A.x'; public $y = 'A.y'; private $p = 'A.p'; protected $q = 'A.q';
  function get() { return [$this->x, $this->y, $this->p, $this->q]; }
  function who() { return 'A'; } function callWho() { return $this->who() . static::class; }
  function setP($v) { $this->p = $v; return $this->p; } }
class B extends A { public $y = 'B.y'; private $p = 'B.p'; public $z = 'B.z';
  function who() { return 'B'; } function getB() { return [$this->p, $this->x, $this->z]; } }
class C2 { public $y = 'C.y'; public $x = 'C.x'; function who() { return 'C'; } }
class G { private $data = []; function __get($n) { return "magic:$n"; } public $x = 'G.x'; }
#[AllowDynamicProperties] class Dy { }
function readX($o) { return $o->x; }
function readY($o) { return $o->y ?? 'none'; }
function writeX($o, $v) { $o->x = $v; return $o->x; }
function callWho($o) { return $o->who(); }
function run($round) {
  $objs = [new A, new B, new C2, new A, new G, new B];
  $d = new Dy; $d->x = 'dyn'; $objs[] = $d;
  $g2 = new G; unset($g2->x); $objs[] = $g2;
  $out = [];
  foreach ($objs as $k => $o) {
    $out[] = $k . ':' . readX($o) . '|' . readY($o) . '|' . (method_exists($o, 'who') ? callWho($o) : '-');
  }
  foreach ($objs as $k => $o) { $out[] = writeX($o, "w$k.$round"); }
  $a = new A; $b = new B;
  $out[] = $a->get(); $out[] = $b->get(); $out[] = $b->getB(); $out[] = $b->setP("set$round"); $out[] = $b->get(); $out[] = $b->getB();
  $out[] = $a->callWho(); $out[] = $b->callWho();
  unset($a->x); $out[] = @readX($a); $a->x = 'back'; $out[] = readX($a);
  return $out;
}
for ($i = 0; $i < 4; $i++) { var_export(run($i)); echo "\n"; }
function loopPoly($n) { $objs = [new A, new B, new C2]; $s = ''; for ($i = 0; $i < $n; $i++) { $o = $objs[$i % 3]; $s .= $o->who() . strlen($o->x) . ($i % 50 == 0 ? $o->y : ''); } return [strlen($s), md5($s)]; }
var_export(loopPoly(150)); var_export(loopPoly(301)); echo "\n";
function fns($n) { $fs = ['strtoupper', 'strrev', fn($s) => "<$s>", 'ucfirst', [new B, 'who'], 'A::who' ]; $r = [];
  for ($i = 0; $i < $n; $i++) { $f = $fs[$i % 5]; $x = $f("ab$i"); if ($i % 40 == 0) { $r[] = $x; } } return $r; }
var_export(fns(150)); echo "\n";
