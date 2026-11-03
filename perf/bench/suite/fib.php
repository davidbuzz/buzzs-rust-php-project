<?php
// Recursive user function calls: INIT_FCALL/SEND/DO_UCALL/RECV/RETURN.
function fib($n) { return $n < 2 ? $n : fib($n - 1) + fib($n - 2); }
echo fib(30), "\n";
