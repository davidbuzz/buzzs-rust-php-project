<?php
class A { public static $shared = 0; public static $own = 'A'; const C = 'A.C'; const D = self::C . '+' . 'd';
  static function create() { return new static; } static function name() { return static::class; }
  static function viaParent() { return static::$own . '/' . self::$own . '/' . static::C . '/' . self::C . '/' . static::D; }
  function inst() { return [static::class, self::class, get_class($this), get_called_class(), static::C]; }
  static function bump() { static::$shared++; self::$shared++; return [A::$shared, static::$own]; } }
class B extends A { public static $own = 'B'; const C = 'B.C';
  static function viaParent() { return 'B>' . parent::viaParent(); }
  static function fwd() { return [parent::name(), self::name(), A::name(), forward_static_call(['A', 'name']), call_user_func('parent::name')]; } }
class C3 extends B { public static $shared = 100; const C = 'C.C'; }
interface I { const IC = 'I.C'; } class D2 extends C3 implements I { const C = self::IC . '!'; }
function run($i) {
  $out = [];
  foreach (['A', 'B', 'C3', 'D2'] as $cls) {
    $out[] = get_class($cls::create()); $out[] = $cls::name(); $out[] = $cls::viaParent(); $out[] = (new $cls)->inst();
    $out[] = $cls::bump(); $out[] = [$cls::$shared, $cls::$own, $cls::C, constant("$cls::C")];
    $obj = new $cls; $out[] = [$obj instanceof A, $obj instanceof B, $obj instanceof I, is_a($obj, 'C3'), is_subclass_of($obj, $cls)];
  }
  $out[] = B::fwd(); $out[] = C3::fwd();
  B::$own = "B$i"; $out[] = [A::$own, B::$own, C3::$own, D2::$own];
  $ref = &A::$shared; $ref += 1000; $out[] = [A::$shared, B::$shared, C3::$shared];
  $name = 'own'; $out[] = B::$$name; $cn = 'C3'; $out[] = $cn::$shared;
  return $out;
}
for ($i = 0; $i < 4; $i++) { var_export(run($i)); echo "\n"; }
