<?php
// Oracle side of the php-zip scenario tests.
//
// usage: php -d memory_limit=1G runner.php <scenario.jsonl> <workdir> <fixtures-dir>
//
// Each scenario line is a JSON array `[op, ...args]`; the Rust harness in
// tests/oracle.rs interprets the same lines against the php-zip API and
// must print exactly the same transcript.

[, $scenario, $DIR, $FIX] = $argv;
$vars = [];
$streams = [];

set_error_handler(function (int $no, string $str) {
    if (!(error_reporting() & $no)) return true;
    $lvl = [E_WARNING => 'Warning', E_DEPRECATED => 'Deprecated', E_NOTICE => 'Notice'][$no] ?? (string)$no;
    echo "[$lvl] $str\n";
    return true;
});

function ex($v): string {
    if (is_null($v)) return 'null';
    if (is_bool($v)) return $v ? 'true' : 'false';
    if (is_int($v)) return (string)$v;
    if (is_float($v)) return var_export($v, true);
    if (is_string($v)) {
        if (preg_match('/^[\x20-\x7e]*$/D', $v)) return '"' . $v . '"';
        return 'hex:' . bin2hex($v);
    }
    if (is_array($v)) {
        $p = [];
        foreach ($v as $k => $x) {
            if ($k === 'mtime' && is_int($x) && $x > 1600000000) {
                $p[] = ex($k) . '=>NOW';
            } else {
                $p[] = ex($k) . '=>' . ex($x);
            }
        }
        return '[' . implode(',', $p) . ']';
    }
    if (is_resource($v)) return 'resource(' . get_resource_type($v) . ')';
    if (is_object($v)) return 'object(' . get_class($v) . ')';
    return gettype($v);
}

function arg($a) {
    global $DIR, $FIX;
    if (is_string($a)) {
        if (str_starts_with($a, 'hex:')) return hex2bin(substr($a, 4));
        if (str_starts_with($a, 'rep:')) {
            [, $n, $s] = explode(':', $a, 3);
            return str_repeat($s, (int)$n);
        }
        if (str_starts_with($a, 'const:')) return constant(substr($a, 6));
        if (str_starts_with($a, 'cb:')) return make_cb(substr($a, 3));
        return str_replace(['{DIR}', '{FIX}'], [$DIR, $FIX], $a);
    }
    if (is_array($a)) return array_map('arg', $a);
    return $a;
}

function make_cb(string $spec) {
    global $vars;
    $parts = explode(':', $spec);
    switch ($parts[0]) {
        case 'progress':
            return function ($r) {
                if ($r == 0.0) echo "progress start\n";
                if ($r == 1.0) echo "progress end\n";
            };
        case 'progress_all':
            return function ($r) { echo "progress ", ex($r > 0.0 && $r < 1.0 ? 'mid' : $r), "\n"; };
        case 'cancel':
            $v = (int)$parts[1];
            return function () use ($v) { echo "cancel?\n"; return $v; };
        case 'cancel_array':
            return function () { return []; };
        case 'cancel_string':
            return function () { return '123abc'; };
        case 'throw':
            return function () { throw new Exception('boom'); };
        case 'reenter':
            // reenter:<var>:<method>:<json args>
            $var = $parts[1];
            $method = $parts[2];
            $args = json_decode(implode(':', array_slice($parts, 3)), true) ?? [];
            return function ($r = null) use ($var, $method, $args) {
                static $done = false;
                if ($done || ($r !== null && $r <= 0)) return 0;
                $done = true;
                global $vars;
                try {
                    $res = $vars[$var]->$method(...array_map('arg', $args));
                    echo "reenter $method = ", ex($res), "\n";
                } catch (Throwable $e) {
                    echo "reenter $method ! ", get_class($e), ': ', $e->getMessage(), "\n";
                }
                return 0;
            };
    }
    throw new Error("unknown callback $spec");
}

function rel(string $p): string {
    global $DIR;
    return $DIR . '/' . $p;
}

function ls(string $base, string $prefix = ''): array {
    $out = [];
    $items = scandir($base . ($prefix === '' ? '' : '/' . $prefix));
    sort($items);
    foreach ($items as $it) {
        if ($it === '.' || $it === '..') continue;
        $p = $prefix === '' ? $it : "$prefix/$it";
        $full = "$base/$p";
        if (is_dir($full)) {
            $out[] = "$p/";
            $out = array_merge($out, ls($base, $p));
        } else {
            $c = file_get_contents($full);
            $m = filemtime($full);
            $out[] = $p . ' ' . strlen($c) . ' ' . substr(md5($c), 0, 8) . ($m < 1600000000 ? " mtime=$m" : '');
        }
    }
    return $out;
}

foreach (file($scenario, FILE_IGNORE_NEW_LINES) as $line) {
    $line = trim($line);
    if ($line === '' || $line[0] === '#') continue;
    $cmd = json_decode($line, true);
    if (!is_array($cmd)) { echo "BAD LINE $line\n"; continue; }
    $op = array_shift($cmd);
    if (in_array($op, ['sread', 'sstat', 'sclose'], true) && $cmd[0] === -1) {
        $cmd[0] = count($streams) - 1;
    }
    try {
        switch ($op) {
            case 'echo':
                echo "# ", $cmd[0], "\n";
                break;
            case 'new':
                $vars[$cmd[0]] = new ZipArchive();
                break;
            case 'call':
                $v = array_shift($cmd);
                $m = array_shift($cmd);
                $args = array_map('arg', $cmd);
                $r = $vars[$v]->$m(...$args);
                echo "$v->$m = ", ex($r), "\n";
                break;
            case 'extattr':
                // getExternalAttributes{Name,Index}(name, flags) by reference
                $v = array_shift($cmd);
                $m = array_shift($cmd);
                $opsys = $attr = null;
                $r = $vars[$v]->$m(arg($cmd[0]), $opsys, $attr, ...array_map('arg', array_slice($cmd, 1)));
                echo "$v->$m = ", ex($r), ' opsys=', ex($opsys), ' attr=', ex($attr), "\n";
                break;
            case 'static':
                $m = array_shift($cmd);
                $r = ZipArchive::$m(...array_map('arg', $cmd));
                echo "ZipArchive::$m = ", ex($r), "\n";
                break;
            case 'prop':
                $r = $vars[$cmd[0]]->{$cmd[1]};
                echo "$cmd[0]->$cmd[1] = ", ex($r), "\n";
                break;
            case 'props':
                $p = [];
                foreach (['lastId', 'status', 'statusSys', 'numFiles', 'filename', 'comment'] as $n) {
                    $p[$n] = $vars[$cmd[0]]->$n;
                }
                echo "$cmd[0] props = ", ex($p), "\n";
                break;
            case 'setprop':
                $vars[$cmd[0]]->{$cmd[1]} = arg($cmd[2]);
                echo "set ok\n";
                break;
            case 'unset':
                unset($vars[$cmd[0]]);
                echo "unset $cmd[0]\n";
                break;
            case 'stream':
                $v = array_shift($cmd);
                $m = array_shift($cmd);
                $s = $vars[$v]->$m(...array_map('arg', $cmd));
                if (!is_resource($s)) { echo "$v->$m = ", ex($s), "\n"; break; }
                $streams[] = $s;
                echo "$v->$m = stream #", count($streams) - 1, "\n";
                break;
            case 'fopen':
                $ctx = isset($cmd[1]) ? stream_context_create(arg($cmd[1])) : null;
                $s = $ctx ? fopen(arg($cmd[0]), 'r', false, $ctx) : fopen(arg($cmd[0]), 'r');
                if (!is_resource($s)) { echo "fopen = ", ex($s), "\n"; break; }
                $streams[] = $s;
                echo "fopen = stream #", count($streams) - 1, "\n";
                break;
            case 'sread':
                $s = $streams[$cmd[0]];
                $r = isset($cmd[1]) ? fread($s, $cmd[1]) : stream_get_contents($s);
                $eof = feof($s);
                echo "read #$cmd[0] = ", ex($r), " eof=", ex($eof), "\n";
                break;
            case 'sstat':
                $st = fstat($streams[$cmd[0]]);
                if ($st === false) { echo "fstat #$cmd[0] = false\n"; break; }
                echo "fstat #$cmd[0] = size=$st[size] mode=", decoct($st['mode']), " mtime=", ($st['mtime'] > 1600000000 ? 'NOW' : $st['mtime']), " nlink=$st[nlink] ino=$st[ino]\n";
                break;
            case 'sclose':
                $r = fclose($streams[$cmd[0]]);
                echo "fclose #$cmd[0] = ", ex($r), "\n";
                break;
            case 'write':
                @mkdir(dirname(rel($cmd[0])), 0777, true);
                file_put_contents(rel($cmd[0]), arg($cmd[1]));
                break;
            case 'copy':
                @mkdir(dirname(rel($cmd[1])), 0777, true);
                copy($FIX . '/' . $cmd[0], rel($cmd[1]));
                break;
            case 'poke':
                $raw = file_get_contents(rel($cmd[0]));
                $sig = arg($cmd[1]);
                $bytes = arg($cmd[3]);
                $pos = 0;
                while (($pos = strpos($raw, $sig, $pos)) !== false) {
                    $raw = substr_replace($raw, $bytes, $pos + $cmd[2], strlen($bytes));
                    $pos += strlen($sig);
                }
                file_put_contents(rel($cmd[0]), $raw);
                break;
            case 'sub':
                $raw = file_get_contents(rel($cmd[0]));
                $pos = strpos($raw, arg($cmd[1]));
                $raw = substr_replace($raw, arg($cmd[2]), $pos, strlen(arg($cmd[1])));
                file_put_contents(rel($cmd[0]), $raw);
                break;
            case 'entries':
                $z = $vars[$cmd[0]];
                for ($i = 0; $i < $z->numFiles; $i++) {
                    echo "entry $i = ", ex($z->getNameIndex($i)), "\n";
                }
                break;
            case 'mkdir':
                mkdir(rel($cmd[0]), 0777, true);
                break;
            case 'chmod':
                chmod(rel($cmd[0]), octdec($cmd[1]));
                break;
            case 'unlink':
                unlink(rel($cmd[0]));
                break;
            case 'ls':
                foreach (ls(rel($cmd[0])) as $l) echo "ls $l\n";
                break;
            case 'cat':
                $r = file_get_contents(rel($cmd[0]));
                echo "cat $cmd[0] = ", ex($r), "\n";
                break;
            case 'size':
                clearstatcache();
                $r = @filesize(rel($cmd[0]));
                echo "size $cmd[0] = ", ex($r), "\n";
                break;
            case 'exists':
                clearstatcache();
                echo "exists $cmd[0] = ", ex(file_exists(rel($cmd[0]))), "\n";
                break;
            case 'zip_open':
                $r = zip_open(arg($cmd[0]));
                if (is_resource($r)) { $vars[$cmd[1]] = $r; echo "zip_open = resource\n"; }
                else echo "zip_open = ", ex($r), "\n";
                break;
            case 'zip_read':
                $r = zip_read($vars[$cmd[0]]);
                if (is_resource($r)) { $vars[$cmd[1]] = $r; echo "zip_read = resource\n"; }
                else echo "zip_read = ", ex($r), "\n";
                break;
            case 'zipfn':
                $f = array_shift($cmd);
                $args = array_map(fn($a) => is_string($a) && str_starts_with($a, 'var:') ? $vars[substr($a, 4)] : arg($a), $cmd);
                $r = $f(...$args);
                echo "$f = ", ex($r), "\n";
                break;
            default:
                echo "UNKNOWN OP $op\n";
        }
    } catch (Throwable $e) {
        echo "! ", get_class($e), ': ', $e->getMessage(), "\n";
    }
}
foreach (array_keys($vars) as $k) {
    unset($vars[$k]);
}
foreach ($streams as $s) {
    if (is_resource($s)) fclose($s);
}
echo "END\n";
