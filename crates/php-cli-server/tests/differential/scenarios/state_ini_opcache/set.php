<?php
foreach (['precision' => '5', 'serialize_precision' => '3', 'error_reporting' => '0', 'display_errors' => '0',
  'html_errors' => '1', 'date.timezone' => 'Asia/Tokyo', 'default_charset' => 'ISO-8859-1', 'memory_limit' => '256M',
  'include_path' => '/nonexistent', 'session.name' => 'OTHER', 'session.use_strict_mode' => '1',
  'session.cookie_httponly' => '1', 'mbstring.language' => 'Japanese', 'mbstring.substitute_character' => '42',
  'max_execution_time' => '7', 'pcre.backtrack_limit' => '10', 'arg_separator.output' => ';',
  'user_agent' => 'x', 'default_socket_timeout' => '3', 'error_log' => '/tmp/x', 'log_errors' => '1',
  'unserialize_callback_func' => 'foo', 'zend.assertions' => '0', 'highlight.string' => '#000',
  'url_rewriter.tags' => 'a=href', 'session.save_path' => '/tmp'] as $k => $v) {
    if (ini_set($k, $v) === false) echo "ini_set($k) failed\n";
}
date_default_timezone_set('America/New_York');
mb_internal_encoding('ISO-8859-1');
mb_substitute_character(63);
mb_language('uni');
setlocale(LC_ALL, 'C.UTF-8');
umask(077);
chdir('/');
putenv('DIFFTEST_VAR=1');
ignore_user_abort(true);
set_time_limit(9);
libxml_use_internal_errors(true);
set_error_handler(fn() => true);
set_exception_handler(fn() => true);
spl_autoload_register(fn($c) => null);
class DtFilter extends php_user_filter {}
class DtWrapper { public $context; }
stream_wrapper_register('difftest', 'DtWrapper');
stream_filter_register('difftest.*', 'DtFilter');
stream_wrapper_unregister('glob');
define('DT_CONST', 1);
function dt_fn() {}
class DtClass {}
$GLOBALS['dt_global'] = 1;
@$undefined_var_notice;
preg_match('/(a+)+b/', str_repeat('a', 30));
http_response_code(201);
header('X-Left: 1');
header_register_callback(function () { header('X-Callback: yes'); });
register_shutdown_function(function () { echo "shutdown\n"; });
ob_start();
echo "left open\n";
