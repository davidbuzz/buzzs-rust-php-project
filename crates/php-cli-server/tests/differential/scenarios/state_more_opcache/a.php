<?php
function counter() { static $n = 0; return ++$n; }
class K { public static $s = 0; const C = 1; static function m() { static $x = 0; return ++$x; } }
enum E: string { case A = 'a'; }
echo counter(), counter(), ' ', ++K::$s, ' ', K::m(), K::m(), ' ', E::A->value, "\n";
$o = new stdClass; $p = new stdClass;
echo spl_object_id($o), ' ', spl_object_id($p), "\n";
$f = fopen("php://memory", "r"); var_dump($f); fclose($f); unset($f);
echo count(get_declared_classes()) > 100 ? 'many' : 'few', ' ', in_array('K', get_declared_classes()), "\n";
echo json_encode(array_keys(get_defined_vars())), "\n";
echo count(get_defined_functions()['user']), ' ', count(get_defined_constants(true)['user'] ?? []), "\n";
var_dump(error_reporting(), ini_get('error_reporting'));
echo implode(',', array_map(fn($x) => gettype($x), array_slice($GLOBALS, 0, 3))), ' ', count($GLOBALS), "\n";
$seed = $_GET['seed'] ?? null;
if ($seed !== null) { mt_srand((int)$seed); srand((int)$seed); }
$r = [mt_rand(), rand(), mt_getrandmax()];
echo $seed !== null ? json_encode($r) : 'unseeded', "\n";
echo setlocale(LC_NUMERIC, 0), ' ', 1.5, ' ', sprintf('%.1f', 2.5), "\n";
if (isset($_GET['loc'])) { var_dump(setlocale(LC_ALL, $_GET['loc'])); echo 3.5, "\n"; }
spl_autoload_register(function ($c) { echo "autoload $c\n"; });
class_exists('NotThere');
if (isset($_GET['er'])) error_reporting((int)$_GET['er']);
if (isset($_GET['gc'])) { gc_disable(); }
var_dump(gc_enabled());
var_dump(preg_match('/(?<n>a)/', 'a', $m), $m, preg_last_error_msg());
if (isset($_GET['bt'])) { ini_set('pcre.backtrack_limit', '1'); var_dump(preg_match('/(a+)+b/', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaac'), preg_last_error_msg()); }
echo umask(), ' ', getcwd() === __DIR__ ? 'cwd ok' : getcwd(), "\n";
if (isset($_GET['ch'])) { umask(0); chdir('/tmp'); }
var_dump(libxml_get_errors());
if (isset($_GET['xml'])) { libxml_use_internal_errors(true); simplexml_load_string('<bad'); }
var_dump(count(libxml_get_errors()));
var_dump(mb_detect_order(), iconv_get_encoding('internal_encoding'));
if (isset($_GET['mb'])) { mb_detect_order(['ASCII']); iconv_set_encoding('internal_encoding', 'ISO-8859-1'); mb_http_output('ISO-8859-1'); }
var_dump(mb_http_output(), date_default_timezone_get(), ini_get('date.timezone'));
if (isset($_GET['tz'])) date_default_timezone_set('Asia/Kolkata');
echo date('e T', 0), "\n";
var_dump(assert_options(ASSERT_ACTIVE), ini_get('zend.assertions'));
var_dump(serialize_precision_check());
function serialize_precision_check() { return var_export(0.1 + 0.2, true) . ' ' . json_encode(0.1 + 0.2); }
var_dump(class_exists('Attr1', false));
#[Attribute] class Attr1 {}
var_dump(array_map(fn($a) => $a->getName(), (new ReflectionClass('K'))->getAttributes()));
var_dump(stream_context_get_options(stream_context_get_default()));
if (isset($_GET['ctx'])) stream_context_set_default(['http' => ['user_agent' => 'leak']]);
var_dump(ini_get('user_agent'), headers_sent());
