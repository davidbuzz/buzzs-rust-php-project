<?php
class A {
  public function __invoke($x) { return "inv:$x"; }
  public function m($x) { return static::class . "::m:$x"; }
  public static function s($x) { return static::class . "::s:$x"; }
  public function __call($n, $a) { return "call:$n:" . implode(',', $a); }
  public static function __callStatic($n, $a) { return "cs:$n:" . implode(',', $a); }
  public function viaThis($x) { return [$this->m($x), static::s($x), self::s($x), $this->undefinedOne($x), static::nope($x)]; }
  public function closures() { return [fn($x) => $this->m($x), static fn($x) => static::class . $x, $this->m(...), self::s(...)]; }
}
class B extends A {
  public function m($x) { return 'B>' . parent::m($x); }
  public static function s($x) { return 'B>' . parent::s($x); }
}
function plain($x) { return "plain:$x"; }
function run($i) {
  $a = new A; $b = new B;
  $calls = ['plain', 'strtoupper', $a, [$a, 'm'], [$b, 'm'], ['B', 's'], 'A::s', 'B::s', [$b, 'parent::m'] ?? null,
            fn($x) => "arrow:$x", plain(...), strrev(...), $b->m(...), B::s(...), [$a, 'magic'], ['A', 'magicS'], 'A::magicS2'];
  $out = [];
  foreach ($calls as $k => $f) {
    if ($k === 8) { continue; }
    $out[] = $f("v$i");
    $out[] = call_user_func($f, "c$i");
    $out[] = call_user_func_array($f, ["ca$i"]);
    $out[] = is_callable($f);
  }
  $out[] = $a->viaThis($i); $out[] = $b->viaThis($i);
  foreach ([...$a->closures(), ...$b->closures()] as $c) { $out[] = $c($i); }
  $bound = Closure::bind(function ($x) { return get_class($this) . $x; }, $b, A::class);
  $out[] = $bound($i);
  $out[] = (function () { return func_get_args(); })->call($a, $i, $i + 1);
  $out[] = array_map([$b, 'm'], [1, 2]);
  $out[] = array_map('B::s', ['k' => $i]);
  $m = 'm'; $s = 's'; $cls = 'B';
  $out[] = $b->$m($i); $out[] = $cls::$s($i); $out[] = [$cls, $s]($i); $out[] = (new $cls)->m(...)($i);
  return $out;
}
for ($i = 0; $i < 4; $i++) { echo "-- $i\n"; var_export(run($i)); echo "\n"; }
