<?php
$names = ['precision', 'serialize_precision', 'error_reporting', 'display_errors', 'html_errors',
  'date.timezone', 'default_charset', 'memory_limit', 'include_path', 'session.name',
  'mbstring.language', 'mbstring.substitute_character',
  'zlib.output_compression', 'output_buffering', 'max_execution_time', 'pcre.backtrack_limit',
  'arg_separator.output', 'user_agent', 'default_socket_timeout', 'error_log', 'log_errors',
  'auto_detect_line_endings', 'unserialize_callback_func', 'assert.active', 'zend.assertions',
  'highlight.string', 'url_rewriter.tags', 'sys_temp_dir', 'open_basedir'];
foreach ($names as $n) { echo $n, '=', var_export(ini_get($n), true), "\n"; }
// Defaults that differ between 8.5 and master or by host: unchanged since startup?
foreach (['session.use_strict_mode', 'session.cookie_httponly', 'session.save_path'] as $n) {
    echo $n, ' unchanged=', var_export(ini_get_all('session')[$n]['local_value'] === ini_get_all('session')[$n]['global_value'], true), "\n";
}
echo 1/3, "\n";
echo date_default_timezone_get(), "\n";
echo mb_internal_encoding(), ' ', mb_substitute_character(), ' ', mb_language(), "\n";
echo setlocale(LC_ALL, 0), "\n";
echo decimal_point_test(), "\n";
function decimal_point_test() { return sprintf('%.2f', 1.5) . ' ' . (string)1.5; }
echo sprintf('%o', umask()), "\n";
echo basename(getcwd()), "\n";
var_dump(getenv('DIFFTEST_VAR'), (int) ignore_user_abort(), error_get_last(), http_response_code());
var_dump(libxml_use_internal_errors(), preg_last_error(), ob_get_level());
echo implode(' | ', headers_list()), "\n";
var_dump(set_error_handler(null), set_exception_handler(null), spl_autoload_functions());
var_dump(stream_get_wrappers() === stream_get_wrappers() ? count(stream_get_wrappers()) : 0);
var_dump(in_array('difftest', stream_get_wrappers()), in_array('difftest.*', stream_get_filters()));
var_dump(defined('DT_CONST'), function_exists('dt_fn'), class_exists('DtClass', false), isset($GLOBALS['dt_global']));
