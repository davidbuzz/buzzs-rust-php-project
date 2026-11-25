<?php
class D { static $n = 0; public $id; function __construct(public $name) { $this->id = ++self::$n; echo "ctor $name\n"; }
  function __destruct() { echo "dtor {$this->name}\n"; }
  function __clone() { $this->name .= "'"; echo "clone {$this->name}\n"; } }
function scope($i) { $a = new D("a$i"); $b = new D("b$i"); echo "in scope\n"; return $b->name; }
function overwrite($i) { $x = new D("x$i"); $x = new D("y$i"); echo "overwritten\n"; unset($x); echo "unset\n"; $arr = [new D("e1"), new D("e2")]; $arr = null; echo "arr null\n"; }
function unwind($i) { $u = new D("u$i"); if ($i % 2) { throw new Exception("boom$i"); } return 'ok'; }
function cl($i) { $o = new D("o$i"); $c = clone $o; $c2 = clone $c; return [$o->name, $c->name, $c2->name]; }
function ids() { $a = new stdClass; $b = new stdClass; $c = new stdClass; $ia = spl_object_id($a); $ib = spl_object_id($b);
  unset($b); $d = new stdClass; unset($a); $e = new stdClass; $f = new stdClass;
  return [spl_object_id($d) - $ib, spl_object_id($e) - $ia, spl_object_id($f) - $ia, spl_object_id($c) - $ia]; }
function ret($i) { return new D("r$i"); }
for ($i = 0; $i < 3; $i++) {
  echo "-- $i\n";
  echo scope($i), "\n";
  overwrite($i);
  try { echo unwind($i), "\n"; } catch (Exception $e) { echo "caught ", $e->getMessage(), "\n"; }
  var_export(cl($i)); echo "\n";
  var_export(ids()); echo "\n";
  ret($i); echo "after discarded ret\n";
  $keep = ret($i); echo "kept\n"; $keep = null;
  echo "end $i\n";
}
