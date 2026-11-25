<?php
$m = $_GET['m'] ?? '';
switch ($m) {
case 'cb_exit': header_register_callback(function () { header('X-Cb: 1'); exit("from cb\n"); }); echo "body\n"; echo "after\n"; break;
case 'cb_throw': header_register_callback(function () { throw new Exception('cb'); }); echo "body\n"; break;
case 'cb_twice': header_register_callback(fn() => header('X-1: 1')); header_register_callback(fn() => header('X-2: 2')); break;
case 'shutdown_hdr': register_shutdown_function(function () { header('X-Shutdown: 1'); http_response_code(202); }); break;
case 'shutdown_hdr_out': register_shutdown_function(function () { header('X-Shutdown: 1'); }); echo "out\n"; break;
case 'cookie_late': echo "x"; var_dump(setcookie('a', 'b')); break;
case 'cookie_bad': var_dump(setcookie("a=b", "v")); var_dump(setcookie("ok", "v", ['samesite' => "x;y"])); var_dump(setcookie("ok", "v", ['bogus' => 1])); break;
case 'destructor': $GLOBALS['o'] = new class { function __destruct() { header('X-Dtor: 1'); echo "dtor\n"; } }; break;
case 'ob_shutdown': ob_start(function ($b) { return "[" . $b . "]"; }); register_shutdown_function(function () { echo "sd"; }); echo "main"; break;
case 'exit_in_ob': ob_start(fn($b) => strrev($b)); echo "abc"; exit(0); break;
case 'status_then_exit': http_response_code(500); exit; break;
case 'loc_exit': header('Location: /x'); exit; break;
}
echo "end\n";
