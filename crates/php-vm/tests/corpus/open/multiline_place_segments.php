<?php
// Each fetch of a chain split over lines reports its own line in C (one
// FETCH opline per segment, each on the line of its name or offset). Ours
// compiles the chain into one place operation with one line: the line of
// the last segment. Expected: missing @6, deeper @7, "a" @9, null offset @10.
set_error_handler(function ($no, $msg, $f, $l) { echo "$msg @$l\n"; return true; });
function f($o, $arr) {
  $v = $o->missing
      ->deeper;
  $z = $arr['a']
     ['b'];
}
f(new stdClass, []); f(new stdClass, []);
