<?php
// Regenerates cases.expected from cases.txt using a real PHP binary:
//   php -n crates/php-pcre/tests/oracle/gen.php crates/php-pcre/tests/oracle/cases.txt > crates/php-pcre/tests/oracle/cases.expected
// The Rust test `tests/oracle.rs` replays the same cases against php-pcre.
// The harness itself must not call preg_* (it would reset preg_last_error()).
// Message differences between PHP 8.3 and php-src master are normalised to master.

function parse_args(string $s): array {
    $i = 0;
    $out = [];
    while (($v = parse_value($s, $i)) !== false) { $out[] = $v[0]; }
    return $out;
}

// Returns [value] or false at end of input / list.
function parse_value(string $s, int &$i) {
    $n = strlen($s);
    while ($i < $n && ($s[$i] === ' ' || $s[$i] === ',')) $i++;
    if ($i >= $n) return false;
    $c = $s[$i];
    if ($c === ']') { $i++; return false; }
    if ($c === '[') {
        $i++;
        $list = [];
        while (($v = parse_value($s, $i)) !== false) { $list[] = $v[0]; }
        return [$list];
    }
    if ($c === '"') {
        $j = $i + 1; $buf = '';
        while ($j < $n && $s[$j] !== '"') {
            if ($s[$j] === '\\') {
                $e = $s[$j + 1];
                switch ($e) {
                    case 'n': $buf .= "\n"; $j += 2; break;
                    case 'r': $buf .= "\r"; $j += 2; break;
                    case 't': $buf .= "\t"; $j += 2; break;
                    case '0': $buf .= "\0"; $j += 2; break;
                    case 'x': $buf .= chr(hexdec(substr($s, $j + 2, 2))); $j += 4; break;
                    default: $buf .= $e; $j += 2; break;
                }
            } else { $buf .= $s[$j]; $j++; }
        }
        $i = $j + 1;
        return [$buf];
    }
    if ($c === "'") {
        // PHP single-quote semantics: only \\ and \' are escapes.
        $j = $i + 1; $buf = '';
        while ($j < $n && $s[$j] !== "'") {
            if ($s[$j] === '\\' && $j + 1 < $n && ($s[$j + 1] === '\\' || $s[$j + 1] === "'")) {
                $buf .= $s[$j + 1]; $j += 2;
            } else { $buf .= $s[$j]; $j++; }
        }
        $i = $j + 1;
        return [$buf];
    }
    if ($c === '-' || ($c >= '0' && $c <= '9')) {
        $j = $i + 1;
        while ($j < $n && ($s[$j] >= '0' && $s[$j] <= '9')) $j++;
        $v = (int)substr($s, $i, $j - $i);
        $i = $j;
        return [$v];
    }
    if (substr($s, $i, 4) === 'null') { $i += 4; return [null]; }
    fwrite(STDERR, "bad arg syntax at $i: $s\n"); exit(1);
}

function normalise(string $msg): string {
    // No preg_* here: it would clobber preg_last_error().
    $p = strpos($msg, '(): ');
    if ($p !== false) $msg = substr($msg, $p + 4);
    $msg = str_replace('backslash, or NUL', 'backslash, or NUL byte', $msg);
    $msg = str_replace('byte byte', 'byte', $msg);
    $msg = str_replace('NUL is not a valid modifier', 'NUL byte is not a valid modifier', $msg);
    if ($msg === 'The /e modifier is no longer supported, use preg_replace_callback instead') {
        $msg = "Unknown modifier 'e'";
    }
    return $msg;
}

set_error_handler(function ($no, $str) {
    echo ($no === E_WARNING ? 'Warning: ' : ($no === E_NOTICE ? 'Notice: ' : 'Error: ')), normalise($str), "\n";
    return true;
});

$lines = file($argv[1], FILE_IGNORE_NEW_LINES);
foreach ($lines as $no => $line) {
    if ($line === '' || $line[0] === '#') continue;
    [$op, $rest] = array_pad(explode(' ', $line, 2), 2, '');
    $a = parse_args($rest);
    echo "== ", $no + 1, "\n";
    if ($op === 'ini') { ini_set($a[0], (string)$a[1]); echo "ok\n"; continue; }
    try {
        switch ($op) {
            case 'match':
                $m = 'untouched';
                var_dump(preg_match($a[0], $a[1], $m, $a[2] ?? 0, $a[3] ?? 0));
                var_dump($m);
                break;
            case 'match_nocap':
                var_dump(preg_match($a[0], $a[1]));
                break;
            case 'match_all':
                $m = 'untouched';
                var_dump(preg_match_all($a[0], $a[1], $m, $a[2] ?? 0, $a[3] ?? 0));
                var_dump($m);
                break;
            case 'match_all_nocap':
                var_dump(preg_match_all($a[0], $a[1]));
                break;
            case 'replace':
            case 'filter':
                $count = -1;
                $fn = $op === 'replace' ? 'preg_replace' : 'preg_filter';
                var_dump($fn($a[0], $a[1], $a[2], $a[3] ?? -1, $count));
                var_dump($count);
                break;
            case 'callback':
                $count = -1;
                var_dump(preg_replace_callback($a[0], function ($m) {
                    echo "callback:\n";
                    var_dump($m);
                    return '<' . count($m) . '>';
                }, $a[1], $a[2] ?? -1, $count, $a[3] ?? 0));
                var_dump($count);
                break;
            case 'split':
                var_dump(preg_split($a[0], $a[1], $a[2] ?? -1, $a[3] ?? 0));
                break;
            case 'grep':
                $r = preg_grep($a[0], $a[1], $a[2] ?? 0);
                // master returns false when matching failed part-way (8.3 returned the partial array).
                if (preg_last_error() !== PREG_NO_ERROR) $r = false;
                var_dump($r);
                break;
            case 'quote':
                var_dump(count($a) > 1 ? preg_quote($a[0], $a[1]) : preg_quote($a[0]));
                break;
            default:
                fwrite(STDERR, "unknown op $op\n"); exit(1);
        }
    } catch (Throwable $e) {
        echo get_class($e), ": ", $e->getMessage(), "\n";
    }
    echo "last_error=", preg_last_error(), "\n";
}
