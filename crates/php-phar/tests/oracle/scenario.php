<?php
// Runs a scenario script (see tests/scenarios.rs) and prints one line per
// result, warning or exception. usage: php scenario.php <file> <workdir>
$lines = file($argv[1], FILE_IGNORE_NEW_LINES | FILE_SKIP_EMPTY_LINES);
$W = rtrim($argv[2], '/');
set_error_handler(function ($no, $msg) use ($W) {
    if (!(error_reporting() & $no)) return true;
    // strip the "function(args): " prefix the VM adds
    if (preg_match('/^[A-Za-z_:]+\((.*?)\): (.*)$/s', $msg, $m)) {
        $msg = $m[2];
    } elseif (preg_match('/^[A-Za-z_:]+\(\): (.*)$/s', $msg, $m)) {
        $msg = $m[1];
    }
    echo ($no == E_WARNING ? 'W' : ($no == E_NOTICE ? 'N' : ($no == E_DEPRECATED ? 'D' : 'E'))), ": ", str_replace($W, '%W%', $msg), "\n";
    return true;
});
function tok($t, $W) {
    if ($t === 'n') return null;
    if (!str_contains($t, ':')) return $t;
    [$k, $v] = explode(':', $t, 2) + [1 => ''];
    switch ($k) {
        case 's': return $v;
        case 'x': return hex2bin($v);
        case 'i': return (int)$v;
        case 'b': return (bool)$v;
        case 'p': return "$W/$v";
        case 'u': return "phar://$W/$v";
        case 'a': return $v === '' ? [] : array_map(fn($x) => tok($x, $W), explode(',', $v));
    }
    return $t;
}
function dump($v, $W) {
    if (is_null($v)) return 'NULL';
    if (is_bool($v)) return $v ? 'bool(true)' : 'bool(false)';
    if (is_int($v)) return "int($v)";
    if (is_string($v)) {
        $v = str_replace($W, '%W%', $v);
        if (strlen($v) > 80 || preg_match('/[^\x20-\x7e]/', $v)) return 'string(' . strlen($v) . ') md5:' . md5($v);
        return 'string(' . strlen($v) . ') "' . $v . '"';
    }
    if (is_array($v)) {
        $o = [];
        foreach ($v as $k => $x) $o[] = dump($k, $W) . '=>' . dump($x, $W);
        return 'array(' . implode(', ', $o) . ')';
    }
    if ($v instanceof PharData || $v instanceof Phar) return get_class($v) . '(' . str_replace($W, '%W%', $v->getPath()) . ')';
    if ($v instanceof PharFileInfo) return 'PharFileInfo(' . str_replace($W, '%W%', $v->getPathname()) . ')';
    return gettype($v);
}
$p = null;
foreach ($lines as $line) {
    if ($line[0] === '#') continue;
    echo "> $line\n";
    $w = explode(' ', $line);
    $args = array_map(fn($t) => tok($t, $W), array_slice($w, 2));
    try {
        switch ($w[0]) {
            case 'ini': echo dump(ini_set($w[1], $w[2]), $W), "\n"; break;
            case 'phar': $p = new Phar(...array_map(fn($t) => tok($t, $W), array_slice($w, 1))); echo "ok\n"; break;
            case 'data': $p = new PharData(...array_map(fn($t) => tok($t, $W), array_slice($w, 1))); echo "ok\n"; break;
            case 'call': echo dump($p->{$w[1]}(...$args), $W), "\n"; break;
            case 'swap': $p = $p->{$w[1]}(...$args); echo dump($p, $W), "\n"; break;
            case 'static': echo dump(Phar::{$w[1]}(...$args), $W), "\n"; break;
            case 'info':
                $name = tok($w[1], $W);
                $o = new PharFileInfo("phar://" . $p->getPath() . "/" . $name);
                echo dump($o->{$w[2]}(...array_map(fn($t) => tok($t, $W), array_slice($w, 3))), $W), "\n";
                unset($o);
                break;
            case 'fn': echo dump(($w[1])(...$args), $W), "\n"; break;
            case 'release': $p = null; echo "ok\n"; break;
            case 'clearstatcache': clearstatcache(); echo "ok\n"; break;
            case 'copyfile': echo dump(copy("$W/{$w[1]}", "$W/{$w[2]}"), $W), "\n"; break;
            case 'shell': echo dump(file_exists("$W/{$w[1]}"), $W), "\n"; break;
            case 'mkfile':
                @mkdir(dirname("$W/{$w[1]}"), 0777, true);
                file_put_contents("$W/{$w[1]}", tok($w[2] ?? 's:', $W));
                echo "ok\n";
                break;
            case 'build':
                $r = isset($w[2]) ? $p->buildFromDirectory("$W/{$w[1]}", tok($w[2], $W)) : $p->buildFromDirectory("$W/{$w[1]}");
                ksort($r);
                echo dump($r, $W), "\n";
                break;
            case 'buildi':
                $items = [];
                foreach (array_slice($w, 2) as $kv) { [$k, $v] = explode('=', $kv, 2); $items[tok($k, $W)] = tok($v, $W); }
                $base = tok($w[1], $W);
                $r = $base === null ? $p->buildFromIterator(new ArrayIterator($items)) : $p->buildFromIterator(new ArrayIterator($items), $base);
                echo dump($r, $W), "\n";
                break;
            case 'fopen':
                $f = fopen(tok($w[1], $W), $w[2]);
                if ($f) { if (isset($w[3])) fwrite($f, tok($w[3], $W)); fclose($f); echo "ok\n"; } else echo "bool(false)\n";
                break;
            default: throw new Error("unknown op");
        }
    } catch (Throwable $e) {
        echo "EXC ", get_class($e), ": ", str_replace($W, '%W%', $e->getMessage()), "\n";
    }
}
