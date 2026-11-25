<?php
class T { public $u; public array $a = [3, 1, 2]; public ?int $ni = null; public string $s = 'abc'; public int|array $ia = 5;
  public static $su; public static array $sa = [9, 8]; public static ?string $ss = null; public $arr = ['e' => [2, 1]];
  public $obj; }
$g = [5, 4];
function ops(&$t, $label) {
  $r = [];
  $r[] = @settype($t, 'array');
  $r[] = array_push($t, 'p'); $r[] = array_pop($t); $r[] = array_unshift($t, 'u'); $r[] = array_shift($t);
  $r[] = end($t); $r[] = key($t); $r[] = reset($t); $r[] = next($t); $r[] = current($t); $r[] = prev($t);
  $r[] = array_splice($t, 0, 1, ['s1', 's2']); $r[] = array_walk($t, function (&$v, $k) { $v = is_int($v) ? $v * 2 : "w$v"; });
  $r[] = sort($t); $r[] = $t;
  return $r;
}
function scalars($i) {
  $t = new T; $t->obj = new stdClass; $out = [];
  $out[] = similar_text('World', 'Word', $t->u); $out[] = $t->u;
  $out[] = sscanf("12 apples $i", "%d %s %d", $n1, $t->obj->w, T::$su); $out[] = [$n1, $t->obj->w, T::$su];
  $out[] = sscanf("x7", "x%d", $t->arr['n']); $out[] = $t->arr;
  $out[] = settype($t->u, 'string'); $out[] = $t->u;
  try { $out[] = settype($t->ni, 'string'); } catch (TypeError $e) { $out[] = $e->getMessage(); } $out[] = $t->ni;
  $x = '12abc'; $out[] = settype($x, 'integer'); $out[] = $x; $y = 1.5; settype($y, 'bool'); $out[] = $y;
  return $out;
}
function targets($i) {
  global $g; static $st = ['b', 'a'];
  $t = new T; $loc = [2, 1]; $other = [7, 6]; $al = &$other; $out = [];
  $out['local'] = ops($loc, 'l');
  $out['undef'] = ops($undef, 'u');
  $out['global'] = ops($g, 'g');
  $out['static'] = ops($st, 's');
  $out['alias'] = ops($al, 'a'); $out['aliased'] = $other;
  $out['prop'] = ops($t->u, 'p');
  $out['tprop'] = ops($t->a, 'tp');
  $out['sprop'] = ops(T::$su, 'sp');
  $out['tsprop'] = ops(T::$sa, 'tsp');
  $out['elem'] = ops($loc[5], 'e');
  $out['nested'] = ops($loc['x']['y'], 'n');
  $out['propelem'] = ops($t->arr['e'], 'pe');
  $objs = [new T]; $out['elemprop'] = ops($objs[0]->u, 'ep');
  $out['state'] = [$t, $loc, $objs];
  return $out;
}
for ($i = 0; $i < 3; $i++) { echo "-- $i\n"; var_export(scalars($i)); echo "\n"; var_export(targets($i)); echo "\n"; }
var_export([$g, T::$su, T::$sa]); echo "\n";
