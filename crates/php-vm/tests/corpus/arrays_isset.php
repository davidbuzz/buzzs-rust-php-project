<?php
class AA implements ArrayAccess {
  public $d = ['a' => ['b' => null, 'c' => 0], 'n' => null];
  function offsetExists($o): bool { echo "exists($o) "; return isset($this->d[$o]); }
  function offsetGet($o): mixed { echo "get($o) "; return $this->d[$o] ?? null; }
  function offsetSet($o, $v): void { echo "set($o) "; $this->d[$o] = $v; }
  function offsetUnset($o): void { echo "unset($o) "; unset($this->d[$o]); }
}
function probe($c) {
  $r = [];
  $r[] = isset($c['a']); $r[] = isset($c['a']['b']); $r[] = isset($c['a']['c']); $r[] = isset($c['a']['b']['x']);
  $r[] = empty($c['a']); $r[] = empty($c['a']['c']); $r[] = empty($c['z']['y']);
  $r[] = $c['a']['c'] ?? 'dflt'; $r[] = $c['n'] ?? 'dn'; $r[] = $c['a']['b'] ?? 'db'; $r[] = $c[0] ?? 'd0';
  $r[] = isset($c[0]); $r[] = isset($c[0][0]); $r[] = isset($c['0']); $r[] = isset($c[-1]); $r[] = isset($c['1x']);
  $r[] = isset($c->p); $r[] = isset($c->p->q);
  echo "\n";
  return $r;
}
function mutate(AA $o, $i) { $o['k'] = $i; $o[] = $i; unset($o['n']); $o['a']; return $o->d; }
$o = new AA;
for ($i = 0; $i < 3; $i++) {
  foreach ([['a' => ['b' => null, 'c' => 0], 'n' => null, 0 => 'z'], 'abc', '', null, 5, $o, ['a' => 'str']] as $k => $c) {
    echo "[$i.$k] "; var_export(probe($c)); echo "\n";
  }
  var_export(mutate($o, $i)); echo "\n";
}
