<?php
// Logging wrappers around json_encode/json_decode/json_validate used to
// build the oracle fixture. Each call is executed for real and recorded as a
// tab-separated line of hex fields in the file named by $JSON_ORACLE_LOG.

function __jhex($s) { return bin2hex((string)$s); }

function __jlog(string $kind, array $fields) {
    $line = $kind . "\t" . implode("\t", array_map('__jhex', $fields)) . "\n";
    file_put_contents(getenv('JSON_ORACLE_LOG'), $line, FILE_APPEND);
}

// Whether a value can be rebuilt from serialize() by the Rust test host:
// scalars, arrays and objects of plain classes (no JsonSerializable, enums,
// closures, custom serialization, hooks, or non-public typed props).
function __jplain($v, int $level = 0): bool {
    if ($level > 600) return false;
    if (is_resource($v) || $v instanceof Closure) return false;
    if (is_array($v)) {
        foreach ($v as $x) if (!__jplain($x, $level + 1)) return false;
        return true;
    }
    if (is_object($v)) {
        if ($v instanceof JsonSerializable || $v instanceof UnitEnum || $v instanceof Serializable) return false;
        $rc = new ReflectionClass($v);
        if ($rc->isInternal() && get_class($v) !== 'stdClass') return false;
        if ($rc->hasMethod('__serialize') || $rc->hasMethod('__sleep')) return false;
        foreach ($rc->getProperties() as $p) {
            if ($p->isStatic()) continue;
            if (method_exists($p, 'hasHooks') && $p->hasHooks()) return false;
            if (!$p->isInitialized($v)) return false;
        }
        foreach ((array)$v as $x) if (!__jplain($x, $level + 1)) return false;
        return true;
    }
    return true;
}

function __jser($v) {
    try { return serialize($v); } catch (Throwable $e) { return null; }
}

function __je(mixed $value, int $flags = 0, int $depth = 512) {
    $plain = __jplain($value);
    $ser = $plain ? __jser($value) : null;
    try {
        $r = json_encode($value, $flags, $depth);
    } catch (Throwable $e) {
        if ($ser !== null) {
            __jlog('E', [ini_get('serialize_precision'), $flags, $depth, $ser, 'X',
                get_class($e) . ':' . $e->getMessage(), $e->getCode(), '']);
        }
        throw $e;
    }
    $code = json_last_error();
    $msg = json_last_error_msg();
    if ($ser !== null) {
        __jlog('E', [ini_get('serialize_precision'), $flags, $depth, $ser,
            $r === false ? 'F' : 'S', $r === false ? '' : $r, $code, $msg]);
    }
    return $r;
}

function __jd(string $json, ?bool $associative = null, int $depth = 512, int $flags = 0) {
    $a = $associative === null ? 'n' : ($associative ? 't' : 'f');
    try {
        $r = json_decode($json, $associative, $depth, $flags);
    } catch (Throwable $e) {
        __jlog('D', [ini_get('serialize_precision'), $flags, $depth, $a, $json, 'X',
            get_class($e) . ':' . $e->getMessage(), $e->getCode(), json_last_error()]);
        throw $e;
    }
    $code = json_last_error();
    $msg = json_last_error_msg();
    __jlog('D', [ini_get('serialize_precision'), $flags, $depth, $a, $json, 'V', serialize($r), $code, $msg]);
    return $r;
}

function __jv(string $json, int $depth = 512, int $flags = 0) {
    try {
        $r = json_validate($json, $depth, $flags);
    } catch (Throwable $e) {
        __jlog('V', [ini_get('serialize_precision'), $flags, $depth, $json, 'X',
            get_class($e) . ':' . $e->getMessage(), $e->getCode(), json_last_error()]);
        throw $e;
    }
    __jlog('V', [ini_get('serialize_precision'), $flags, $depth, $json, $r ? '1' : '0', '',
        json_last_error(), json_last_error_msg()]);
    return $r;
}
