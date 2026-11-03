<?php
// Recorders for gen_oracle.php. Every filter_* call of a rewritten test goes
// through __filter_rec(), which calls the real function and appends one
// line to $FILTER_ORACLE_LOG:
//
//   <function> \t <args> \t <result> \t <diagnostics> \t <exception> \t <extras>
//
// All fields are hex-encoded. <args> and <result> are serialize()d after
// encoding objects as ["\0obj" => class, "\0str" => __toString() or null]
// and callables (FILTER_CALLBACK options) as ["\0callable" => n].
// <diagnostics> is a JSON list of "Level: message". <exception> is
// "Class: message" or empty. <extras> is JSON: the callback calls
// [[arg, ret], ...] (encoded), the regex results {pattern: {subject: r}}
// and, for filter_input*/filter_has_var, the raw input storages.

function __filter_enc($v, int $depth = 0) {
    if ($depth > 32) return null;
    if (is_object($v)) {
        if ($v instanceof Closure) return ["\0callable" => -1];
        return ["\0obj" => get_class($v),
                "\0str" => method_exists($v, '__toString') ? (string)$v : null];
    }
    if (is_resource($v)) return ["\0res" => get_resource_type($v)];
    if (is_array($v)) {
        $o = [];
        foreach ($v as $k => $x) $o[$k] = __filter_enc($x, $depth + 1);
        return $o;
    }
    return $v;
}

$GLOBALS['__filter_cb_log'] = [];
$GLOBALS['__filter_regex_log'] = [];

function __filter_wrap_callback(array &$args, string $func, &$skip) {
    // Locate the options argument and replace a FILTER_CALLBACK callable by
    // a logging proxy.
    $wrap = function (&$opts) use (&$skip) {
        if (!is_array($opts)) return;
        $isCb = isset($opts['filter']) && (int)$opts['filter'] === FILTER_CALLBACK;
        if ($isCb && array_key_exists('options', $opts) && is_callable($opts['options'])) {
            $orig = $opts['options'];
            $GLOBALS['__filter_callables'][] = __filter_enc($orig);
            $opts['options'] = function ($v) use ($orig) {
                $in = $v;
                $r = $orig($v);
                $GLOBALS['__filter_cb_log'][] = [__filter_enc($in), __filter_enc($r)];
                return $r;
            };
        }
    };
    if ($func === 'filter_var' || $func === 'filter_input') {
        $i = $func === 'filter_var' ? 1 : 2;
        $filter = $args[$i] ?? FILTER_DEFAULT;
        $oi = $i + 1;
        if (isset($args[$oi]) && is_array($args[$oi])) {
            $o = $args[$oi];
            $had = array_key_exists('filter', $o);
            if (!$had) $o['filter'] = $filter; // only for detection
            $wrap($o);
            if (!$had) unset($o['filter']);
            $args[$oi] = $o;
        }
    } elseif ($func === 'filter_var_array' || $func === 'filter_input_array') {
        if (isset($args[1]) && is_array($args[1])) {
            foreach ($args[1] as $k => $def) {
                if (is_array($def)) { $wrap($def); $args[1][$k] = $def; }
            }
        }
    }
}

function __filter_regex_probe(string $func, array $args) {
    // For FILTER_VALIDATE_REGEXP, record preg_match() of every leaf.
    $probe = function ($filter, $opts, $value) {
        if ((int)$filter !== FILTER_VALIDATE_REGEXP) return;
        $o = is_array($opts) ? ($opts['options'] ?? null) : null;
        if (!is_array($o) || !isset($o['regexp']) || !is_string($o['regexp'])) return;
        $re = $o['regexp'];
        $leaves = [];
        array_walk_recursive_safe($value, $leaves);
        foreach ($leaves as $leaf) {
            if (is_object($leaf) && !method_exists($leaf, '__toString')) continue;
            $s = (string)$leaf;
            $warn = [];
            set_error_handler(function ($no, $msg) use (&$warn) { $warn[] = $msg; return true; });
            $r = preg_match($re, $s);
            restore_error_handler();
            $GLOBALS['__filter_regex_log'][] = [bin2hex($re), bin2hex($s), $r === false ? -1 : $r,
                array_map('bin2hex', $warn)];
        }
    };
    if ($func === 'filter_var') {
        $f = $args[1] ?? FILTER_DEFAULT;
        $opts = $args[2] ?? 0;
        if (is_array($opts) && isset($opts['filter'])) $f = $opts['filter'];
        $probe($f, $opts, $args[0] ?? null);
    } elseif ($func === 'filter_var_array' && isset($args[0]) && is_array($args[0])) {
        $defs = $args[1] ?? FILTER_DEFAULT;
        if (is_array($defs)) {
            foreach ($defs as $k => $def) {
                if (is_array($def) && array_key_exists($k, $args[0])) {
                    $probe($def['filter'] ?? -1, $def, $args[0][$k]);
                }
            }
        }
    }
}

function array_walk_recursive_safe($v, array &$out, int $depth = 0) {
    if ($depth > 16) return;
    if (is_array($v)) { foreach ($v as $x) array_walk_recursive_safe($x, $out, $depth + 1); }
    elseif (!is_resource($v)) $out[] = $v;
}

function __filter_storages($type) {
    $s = [];
    foreach (['post' => INPUT_POST, 'get' => INPUT_GET, 'cookie' => INPUT_COOKIE,
              'env' => INPUT_ENV, 'server' => INPUT_SERVER] as $k => $t) {
        if ($t !== $type) continue;
        $a = filter_input_array($t, FILTER_UNSAFE_RAW);
        $s[$k] = $a === null ? null : bin2hex(serialize($a));
    }
    return $s;
}

function __filter_rec(string $func, ...$args) {
    $orig = $args;
    $skip = false;
    $GLOBALS['__filter_cb_log'] = [];
    $GLOBALS['__filter_regex_log'] = [];
    $GLOBALS['__filter_callables'] = [];
    __filter_wrap_callback($args, $func, $skip);
    __filter_regex_probe($func, $orig);
    $diag = [];
    $levels = [E_WARNING => 'Warning', E_NOTICE => 'Notice', E_DEPRECATED => 'Deprecated',
               E_USER_WARNING => 'Warning', E_USER_NOTICE => 'Notice', E_USER_DEPRECATED => 'Deprecated'];
    set_error_handler(function ($no, $msg) use (&$diag, $levels) {
        $diag[] = ($levels[$no] ?? "E$no") . ': ' . $msg;
        return true;
    });
    $exc = '';
    $result = null;
    $thrown = null;
    try {
        $result = $func(...$args);
    } catch (Throwable $e) {
        $exc = get_class($e) . ': ' . $e->getMessage();
        $thrown = $e;
    } finally {
        restore_error_handler();
    }
    $extras = ['cb' => $GLOBALS['__filter_cb_log'], 're' => $GLOBALS['__filter_regex_log'],
               'callables' => $GLOBALS['__filter_callables']];
    if (in_array($func, ['filter_input', 'filter_input_array', 'filter_has_var'])) {
        $extras['in'] = __filter_storages($orig[0] ?? null);
    }
    $encArgs = __filter_enc($orig);
    $line = implode("\t", [
        $func,
        bin2hex(serialize($encArgs)),
        bin2hex(serialize(__filter_enc($result))),
        bin2hex(json_encode($diag)),
        bin2hex($exc),
        ($extras['cb'] || $extras['re'] || $extras['callables'] || isset($extras['in']))
            ? bin2hex(serialize($extras)) : '',
    ]);
    file_put_contents(getenv('FILTER_ORACLE_LOG'), $line . "\n", FILE_APPEND);
    // Re-emit what the test would have seen.
    foreach ($diag as $d) {
        [$lvl, $msg] = explode(': ', $d, 2);
        trigger_error($msg, $lvl === 'Warning' ? E_USER_WARNING : ($lvl === 'Deprecated' ? E_USER_DEPRECATED : E_USER_NOTICE));
    }
    if ($thrown) throw $thrown;
    return $result;
}
