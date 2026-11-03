<?php
// Float arithmetic plus cheap builtin calls (sqrt, sin) in a loop.
$f = 0.0;
for ($i = 1; $i <= 2_000_000; $i++) {
    $f += sqrt($i) * sin($i) / ($i + 1.5);
}
printf("%.6f\n", $f);
