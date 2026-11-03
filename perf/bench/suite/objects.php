<?php
// Object construction, property writes/reads, method calls.
class P {
    public $x;
    public $y;
    function __construct($x, $y) { $this->x = $x; $this->y = $y; }
    function len() { return sqrt($this->x * $this->x + $this->y * $this->y); }
}
$d = 0.0;
for ($i = 0; $i < 1_000_000; $i++) {
    $p = new P($i, $i + 1);
    $d += $p->len();
}
printf("%.3f\n", $d);
