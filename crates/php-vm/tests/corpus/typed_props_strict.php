<?php declare(strict_types=1);
enum E: string { case A = 'a'; case B = 'b'; }
class T { public int $i = 0; public ?int $ni = null; public float $f = 0.0; public string $s = '';
  public bool $b = false; public array $a = []; public ?T $o = null; public int|string $u = 0; public ?E $e = null;
  public function __construct(public readonly int $ro = 7) {}
  function put($v) {
    $out = [];
    foreach (['i', 'ni', 'f', 's', 'b', 'u'] as $p) {
      try { $this->$p = $v; $out[] = [$p, $this->$p]; } catch (TypeError $e) { $out[] = [$p, $e->getMessage()]; }
    }
    return $out;
  }
  function ops($v) {
    $r = [];
    try { if (is_numeric($v) || is_bool($v) || $v === null) { $this->i += $v; } $r[] = $this->i; } catch (TypeError $e) { $r[] = $e->getMessage(); }
    try { $this->s .= $v; $r[] = $this->s; } catch (TypeError $e) { $r[] = $e->getMessage(); }
    try { $this->f++; $this->i--; $r[] = [$this->f, $this->i]; } catch (Error $e) { $r[] = $e->getMessage(); }
    $this->ni ??= 5; $r[] = $this->ni;
    $this->a[] = $v; $this->a['k'] = $v; $r[] = count($this->a);
    try { $this->e = E::from($v); } catch (Throwable $e) { $r[] = get_class($e); }
    $r[] = $this->e;
    try { $this->ro = 1; } catch (Error $e) { $r[] = $e->getMessage(); }
    try { $this->o = $v; } catch (TypeError $e) { $r[] = $e->getMessage(); }
    $ref = &$this->u; try { $ref = [$v]; } catch (TypeError $e) { $r[] = $e->getMessage(); } unset($ref);
    return $r;
  }
}
function viaVar(T $t, $v) { $t->i = $v; $x = $t; $x->f = $v; return [$t->i, $t->f]; }
set_error_handler(function ($no, $msg, $f, $l) { echo "E[$no] $msg @$l\n"; return true; });
$t = new T;
foreach (['5', 1, 1.0, 1.5, 'a', true, null, ' 7', '7 ', 'a', 'b', 2**63] as $k => $v) {
  echo "-- $k\n";
  var_export($t->put($v)); echo "\n";
  var_export($t->ops($v)); echo "\n";
  try { var_export(viaVar($t, $v)); } catch (TypeError $e) { echo $e->getMessage(); } echo "\n";
}
var_dump($t);
