<?php
// Property access: declared, typed, static properties and $this-> in methods.
class C {
    public int $a = 0;
    public $b = 0;
    public static $s = 0;
    function run(int $n): int {
        for ($i = 0; $i < $n; $i++) {
            $this->a += 1;
            $this->b = $this->a + $i;
            self::$s++;
        }
        return $this->b;
    }
}
$c = new C();
$r = $c->run(1_000_000);
$o = new stdClass();
$o->v = 0;
for ($i = 0; $i < 500_000; $i++) {
    $o->v = $o->v + $c->a;
}
echo $r, ' ', C::$s, ' ', $o->v, "\n";
