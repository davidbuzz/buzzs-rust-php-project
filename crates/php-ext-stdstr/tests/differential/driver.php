<?php
// Differential driver: calls every internal function of one extension with a
// matrix of argument values and prints one line per call (the call, then the
// result or the exception, then the diagnostics an error handler collected).
// Run it under our php and under the C php with the same flags, then diff.
//
//   php -n -d memory_limit=1G -d error_reporting=-1 driver.php <ext> [letters] [arity]
//
// <ext> is an extension name as ReflectionFunction::getExtensionName() gives it
// (standard, Core, ctype, json, hash, date, pcre, mbstring, ...). [letters] is
// a set of first letters ("a-c", "s", "*" for all). [arity] is 0, 1, 2, 3 or
// "all" (default all); "sigs" prints the signature file the 4th argument reads,
// "reflect" prints each function's reflected signature.
//
// The deny-list below keeps the driver away from anything with side effects on
// the machine or the process: files, network, processes, environment, time,
// randomness, output and INI state, handlers, scope access.

ini_set('serialize_precision', '-1');
ini_set('precision', '14');

$ext = $argv[1] ?? 'standard';
$letters = $argv[2] ?? '*';
$arityArg = $argv[3] ?? 'all';
// Optional: a signature file written by `driver.php <ext> * sigs > file` under the C
// php. With it both implementations pick their "natural" values from the same types
// and make the same calls; without it each uses its own reflection.
$sigFile = $argv[4] ?? '';
$sigs = [];
if ($sigFile !== '' && is_readable($sigFile)) {
    foreach (explode("\n", (string) file_get_contents($sigFile)) as $row) {
        if ($row === '') {
            continue;
        }
        [$name, $json] = explode(' ', $row, 2);
        $sigs[$name] = json_decode($json, true);
    }
}

const DENY_RX = '/^(
    f[a-z_]*|file.*|.*dir.*|glob|is_(dir|file|link|executable|readable|writ(e)?able|uploaded_file)|
    (l)?stat|touch|unlink|rename|copy|tempnam|tmpfile|readfile|realpath.*|chmod|chown|chgrp|lch.*|
    link|linkinfo|readlink|symlink|umask|disk_.*|parse_ini_file|highlight_file|show_source|
    php_strip_whitespace|move_uploaded_file|clearstatcache|pclose|popen|proc_.*|exec|system|passthru|
    shell_exec|escapeshell.*|pcntl_.*|posix_.*|getmypid|getmyuid|getmygid|getmyinode|getlastmod|
    get_current_user|getrusage|gethostname|php_uname|sys_get_temp_dir|sys_getloadavg|getenv|putenv|
    get_cfg_var|ini_.*|set_include_path|get_include_path|set_time_limit|ignore_user_abort|
    connection_.*|header.*|headers_.*|http_response_code|setcookie|setrawcookie|
    ob_.*|output_.*|flush|print_r|var_dump|var_export|debug_zval_refcount|debug_.*|printf|vprintf|
    fprintf|vfprintf|print|echo|error_log|syslog|openlog|closelog|
    sleep|usleep|time_nanosleep|time_sleep_until|exit|die|register_.*|set_error_handler|
    set_exception_handler|restore_.*|spl_autoload.*|trigger_error|user_error|error_reporting|
    error_get_last|error_clear_last|assert.*|gc_.*|memory_.*|get_resources|
    time|microtime|hrtime|mktime|gmmktime|date|gmdate|idate|strtotime|getdate|localtime|checkdate_|
    date_create.*|date_default_timezone_.*|date_sun.*|uniqid|gettimeofday|strftime|gmstrftime|
    timezone_.*|date_.*|
    rand|mt_.*|random_.*|srand|lcg_value|shuffle|str_shuffle|array_rand|getrandmax|
    extract|compact|get_defined_vars|func_get_args?|func_num_args|get_defined_constants|
    get_defined_functions|get_declared_.*|get_loaded_extensions|get_extension_funcs|
    call_user_func.*|forward_static_call.*|func_overload|array_walk.*|iterator_apply|
    usort|uasort|uksort|array_map|array_filter|array_reduce|preg_replace_callback.*|
    define|constant|class_alias|create_function|
    dns_.*|gethostby.*|getmx.*|getprotobyn.*|getservby.*|checkdnsrr|net_get_interfaces|
    mail|socket_.*|stream_.*|set_file_buffer|set_socket_blocking|
    openssl_.*|session_.*|setlocale|localeconv|nl_langinfo|
    phpinfo|phpcredits|phpversion|php_ini_.*|php_sapi_name|zend_version|
    cli_.*|dl|apache_.*|getopt|readline.*|get_browser|sapi_.*|
    spl_object_(id|hash)|spl_classes|class_(implements|parents|uses)|
    unserialize|serialize|var_export|settype|
    mb_(http_.*|send_mail|parse_str|internal_encoding|regex_encoding|detect_order|substitute_character|language)|
    mb_ereg.*|iterator_.*|array_multisort|levenshtein|similar_text|str_repeat|str_pad|range|
    array_fill|array_fill_keys|array_pad|chunk_split|wordwrap|money_format|
    json_last_error.*|preg_last_error.*|libxml_.*|hash_file|hash_hmac_file|md5_file|sha1_file|hash_update_file|hash_update_stream|crc32_file|
    password_hash|crypt|password_needs_rehash|hash_pbkdf2|
    pcre_.*|preg_replace_callback_array|is_callable|
    image_type_to_.*|getimagesize.*|iptcembed|
    parse_str|import_request_variables|
    gethostbyaddr|inet_.*|ip2long|long2ip|
    convert_uu.*|quoted_printable_.*|
    get_class|get_parent_class|get_called_class|get_object_vars|get_class_vars|get_class_methods|method_exists|property_exists|class_exists|interface_exists|trait_exists|enum_exists|function_exists|is_subclass_of|is_a|
    set_.*|get_mangled_object_vars|array_key_.*_x
)$/xi';

$fns = [];
foreach (get_defined_functions()['internal'] as $f) {
    $rf = new ReflectionFunction($f);
    if (strcasecmp((string) $rf->getExtensionName(), $ext) !== 0) {
        continue;
    }
    if (preg_match(DENY_RX, $f)) {
        continue;
    }
    if ($rf->isDeprecated()) {
        continue;
    }
    if ($letters !== '*' && !preg_match('/^[' . $letters . ']/i', $f)) {
        continue;
    }
    $fns[] = $f;
}
sort($fns);

class Str
{
    public function __toString(): string
    {
        return 'str';
    }
}

$long = str_repeat('abcdefghij', 7);
$pool = [
    'null' => null, 'false' => false, 'true' => true,
    '0' => 0, '1' => 1, '-1' => -1, 'MAX' => PHP_INT_MAX, 'MIN' => PHP_INT_MIN,
    '0.0' => 0.0, '-0.0' => -0.0, '1.5' => 1.5, '-1.5' => -1.5, 'NAN' => NAN, 'INF' => INF,
    '1e100' => 1e100,
    "''" => '', "'0'" => '0', "'1'" => '1', "'abc'" => 'abc', "' 1'" => ' 1', "'1 '" => '1 ',
    "'1e3'" => '1e3', "'0x1A'" => '0x1A', "'a\\0b'" => "a\0b", 'long' => $long,
    '[]' => [], '[1,2]' => [1, 2], "['a'=>1]" => ['a' => 1], '[[1]]' => [[1]],
    'stdClass' => new stdClass(), 'Str' => new Str(),
];
// Values a closure would make nondeterministic across implementations are left out;
// the closure is tested only as a callable-free object.
$pool['closure'] = static fn () => 1;

/** A short, deterministic rendering of a value. */
function show(mixed $v, int $depth = 0): string
{
    if (is_null($v)) {
        return 'NULL';
    }
    if (is_bool($v)) {
        return $v ? 'true' : 'false';
    }
    if (is_int($v)) {
        return (string) $v;
    }
    if (is_float($v)) {
        $s = var_export($v, true);
        return 'f:' . $s;
    }
    if (is_string($v)) {
        if (strlen($v) > 60) {
            return 's(' . strlen($v) . ')#' . sprintf('%08x', crc32($v));
        }
        return '"' . addcslashes($v, "\0..\37\"\\\177..\377") . '"';
    }
    if (is_array($v)) {
        $s = serialize($v);
        if (count($v) > 8 || strlen($s) > 200 || $depth > 1) {
            return 'array(' . count($v) . ')#' . sprintf('%08x', crc32($s));
        }
        $parts = [];
        foreach ($v as $k => $x) {
            $parts[] = (is_int($k) ? $k : '"' . addcslashes($k, "\0..\37\"\\\177..\377") . '"') . '=>' . show($x, $depth + 1);
        }
        return '[' . implode(',', $parts) . ']';
    }
    if (is_object($v)) {
        return 'object(' . get_class($v) . ')';
    }
    if (is_resource($v)) {
        return 'resource(' . get_resource_type($v) . ')';
    }
    return 'closed-resource';
}

$diags = [];
set_error_handler(static function (int $no, string $msg) use (&$diags): bool {
    $diags[] = $no . ':' . $msg;
    return true;
});

/** The signature the driver works from: [max arity, [param type names], required, variadic]. */
function signature(ReflectionFunction $rf): array
{
    $types = [];
    foreach ($rf->getParameters() as $p) {
        $t = $p->getType();
        $types[] = $t instanceof ReflectionNamedType ? $t->getName() : ($t === null ? 'mixed' : 'union');
    }
    return [count($types), $types, $rf->getNumberOfRequiredParameters(), $rf->isVariadic()];
}

/** Three "natural" values for a parameter's declared type name. */
function natural(string $n): array
{
    return match ($n) {
        'int' => [0, 3, -2],
        'float' => [0.0, 2.5, -1.0],
        'bool' => [true, false, true],
        'string' => ['abc', '', 'a,b'],
        'array' => [[1, 2], [], ['x' => 'y', 3 => 'z']],
        'callable' => ['strlen', 'strtoupper', 'trim'],
        default => ['abc', 3, [1, 2]],
    };
}

function call_one(string $f, array $args, string $label): void
{
    global $diags;
    $diags = [];
    $line = $f . '(' . $label . ') = ';
    try {
        $r = $f(...$args);
        $line .= show($r);
    } catch (Throwable $e) {
        $line .= get_class($e) . ': ' . $e->getMessage();
    }
    if ($diags) {
        $line .= ' | ' . implode(' | ', $diags);
    }
    echo str_replace("\n", '\n', $line), "\n";
}

$labels = array_keys($pool);
foreach ($fns as $f) {
    $rf = new ReflectionFunction($f);
    if ($arityArg === 'sigs') {
        echo $f, ' ', json_encode(signature($rf)), "\n";
        continue;
    }
    if ($arityArg === 'reflect') {
        // Reflection itself is user-visible: print the signature for diffing.
        $ps = [];
        foreach ($rf->getParameters() as $p) {
            $ps[] = ($p->getType() ?? '') . ' ' . ($p->isPassedByReference() ? '&' : '')
                . ($p->isVariadic() ? '...' : '') . '$' . $p->getName()
                . ($p->isOptional() && !$p->isVariadic() ? ' = ' . ($p->isDefaultValueAvailable()
                    ? ($p->isDefaultValueConstant() ? $p->getDefaultValueConstantName() : show($p->getDefaultValue()))
                    : '?') : '');
        }
        echo $f, '(', implode(', ', $ps), '): ', $rf->getReturnType() ?? '', "\n";
        continue;
    }
    [$count, $types, $required, $variadic] = $sigs[$f] ?? signature($rf);
    $max = $variadic ? max(3, $count) : $count;
    $t0 = $types[0] ?? 'mixed';
    $t1 = $types[1] ?? $t0;
    if (($arityArg === 'all' || $arityArg === '0') && $required === 0) {
        call_one($f, [], '');
    }
    if (($arityArg === 'all' || $arityArg === '1') && $max >= 1) {
        foreach ($labels as $l) {
            call_one($f, [$pool[$l]], $l);
        }
    }
    if (($arityArg === 'all' || $arityArg === '2') && $max >= 2) {
        foreach (natural($t0) as $i => $nat) {
            foreach ($labels as $l) {
                call_one($f, [$nat, $pool[$l]], 'N' . $i . ',' . $l);
            }
        }
        foreach (natural($t1) as $i => $nat) {
            foreach ($labels as $l) {
                call_one($f, [$pool[$l], $nat], $l . ',N' . $i);
            }
        }
    }
    if (($arityArg === 'all' || $arityArg === '3') && $max >= 3) {
        $n0 = natural($t0)[0];
        $n1 = natural($t1)[0];
        foreach ($labels as $l) {
            call_one($f, [$n0, $n1, $pool[$l]], 'N,N,' . $l);
        }
    }
}
