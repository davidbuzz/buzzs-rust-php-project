<?php
$m = $_GET['m'] ?? '';
if (isset($_GET['de'])) ini_set('display_errors', $_GET['de']);
if (isset($_GET['he'])) ini_set('html_errors', $_GET['he']);
echo "start\n";
switch ($m) {
case 'fatal': function f() { undefined_fn(); } f(); break;
case 'exc': throw new RuntimeException("boom <b>");
case 'exc_handler': set_exception_handler(function ($e) { echo "handled ", $e->getMessage(), "\n"; }); throw new Exception("x");
case 'exit_str': exit("bye\n");
case 'exit_int': exit(3);
case 'die_shutdown': register_shutdown_function(function () { echo "in shutdown\n"; die("died\n"); echo "never"; }); register_shutdown_function(function () { echo "second\n"; }); break;
case 'warn': echo $undefined; strlen(); break;
case 'notice_then_header': echo $undef2; header('X-Late: 1'); break;
case 'parse': include __DIR__ . '/bad.inc'; break;
case 'mem': ini_set('memory_limit', '4M'); $a = str_repeat('x', 8 << 20); break;
case 'trigger': trigger_error("user <warn>", E_USER_WARNING); trigger_error("user dep", E_USER_DEPRECATED); trigger_error("fatal user", E_USER_ERROR); break;
case 'handler': set_error_handler(function ($no, $str) { echo "H($no): $str\n"; return true; }); echo $undef3; break;
case 'docref': ini_set('docref_root', 'http://x/'); ini_set('docref_ext', '.html'); hex2bin('abc'); hex2bin('<z>'); break;
case 'ob_fatal': ob_start(); echo "buffered"; undefined_fn2(); break;
case 'ob_cb_fatal': ob_start(function ($b) { return strtoupper($b); }); echo "buffered "; throw new Exception('in ob'); break;
case 'error_log': ini_set('log_errors', '1'); ini_set('error_log', ''); error_log("logged"); echo $u; break;
case 'stack': function g($a) { throw new LogicException('deep'); } g([1,2,3]); break;
case 'gle': @strlen(); var_dump(error_get_last()); error_clear_last(); var_dump(error_get_last()); break;
}
echo "end\n";
