<?php
set_error_handler(function ($no, $msg, $f, $l) { echo "E[$no] $msg @$l\n"; return true; });
class M { private $data = ['a' => 1]; public $real = 'r';
  function __get($n) { echo "get($n) "; return $this->$n ?? ($this->data[$n] ?? null); }
  function __set($n, $v) { echo "set($n) "; $this->$n = $v; }
  function __isset($n) { echo "isset($n) "; return isset($this->data[$n]); }
  function __unset($n) { echo "unset($n) "; unset($this->data[$n]); }
}
class Plain { public $d = 1; }
#[AllowDynamicProperties] class Dyn {}
class RO { public function __construct(public readonly array $a, public readonly int $n = 1) {}
  function bump() { try { $this->n++; } catch (Error $e) { return $e->getMessage(); } }
  function app() { try { $this->a[] = 1; } catch (Error $e) { return $e->getMessage(); } } }
class Hk { public int $x = 0 { set => $value * 2; get => $this->x + 1; } public $log = []; public string $v { get => 'virt' . $this->x; } }
function run($i) {
  $out = [];
  $m = new M; $out[] = $m->a; $out[] = $m->zz; $m->q = $i; $out[] = $m->q; $out[] = isset($m->a); $out[] = isset($m->real);
  $out[] = empty($m->a); unset($m->a); $out[] = isset($m->a); $m->real = 'w'; $out[] = $m->real; $m->data = 'priv'; $out[] = $m->a ?? 'dd';
  echo "\n";
  $p = new Plain; $p->undecl = $i; $out[] = $p->undecl; $out[] = $p->nope ?? 'n'; $out[] = @$p->nope2;
  $d = new Dyn; $d->x = $i; $d->x .= 'y'; $out[] = $d;
  $n = null; $out[] = $n?->x; $out[] = @$n->x;
  try { $n->x = 1; } catch (Error $e) { $out[] = $e->getMessage(); }
  try { $n->m(); } catch (Error $e) { $out[] = $e->getMessage(); }
  $r = new RO([$i]); $out[] = $r->bump(); $out[] = $r->app(); $out[] = $r;
  try { $r->n = 5; } catch (Error $e) { $out[] = $e->getMessage(); }
  $c = clone $r; $out[] = $c == $r; $out[] = $c === $r;
  $h = new Hk; $h->x = $i; $out[] = $h->x; $h->x += 3; $out[] = $h->x; $out[] = $h->v;
  return $out;
}
for ($i = 0; $i < 4; $i++) { echo "-- $i\n"; var_export(run($i)); echo "\n"; }
