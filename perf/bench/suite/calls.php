<?php
// Call overhead: plain, typed, static, instance and closure calls with arguments.
function add(int $a, int $b): int { return $a + $b; }
function untyped($a, $b) { return $a; }
final class M {
    private int $acc = 0;
    public static function twice(int $x): int { return $x * 2; }
    public function bump(int $by): void { $this->acc += $by; }
    public function acc(): int { return $this->acc; }
}
$m = new M();
$c = function ($x) { return $x + 1; };
$s = 0;
for ($i = 0; $i < 300_000; $i++) {
    $s = add($s, 1);
    $s = untyped($s, $i);
    $s += M::twice(1);
    $m->bump(1);
    $s = $c($s);
}
echo $s + $m->acc(), "\n";
