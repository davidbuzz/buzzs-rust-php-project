<?php
// Oracle counterpart of examples/probe.rs: run with
//   php -d date.timezone=UTC crates/php-date/tools/probe.php < cmds.txt
error_reporting(0);
function unesc($s) {
    return preg_replace_callback('/\\\\(.)/s', function ($m) {
        return match ($m[1]) { 't' => "\t", 'n' => "\n", '0' => "\0", default => $m[1] };
    }, $s);
}
function dump($v) {
    if (is_null($v)) return 'N';
    if (is_bool($v)) return $v ? 'T' : 'F';
    if (is_int($v)) return (string)$v;
    if (is_float($v)) return sprintf('%.6F', $v);
    if (is_string($v)) return '"' . $v . '"';
    $o = '{';
    foreach ($v as $k => $x) $o .= $k . ':' . dump($x) . ',';
    return $o . '}';
}
function mk($tz, $str) {
    try { return new DateTime($str, null); } catch (Throwable $e) { return null; }
}
while (($line = fgets(STDIN)) !== false) {
    $line = rtrim($line, "\n");
    $f = array_map('unesc', explode("\t", $line));
    ob_start();
    $cmd = $f[0];
    $tz = $f[1] ?? 'UTC';
    if (in_array($cmd, ['strtotime', 'date', 'create', 'cff', 'modify', 'diff'])) {
        date_default_timezone_set($tz);
    }
    switch ($cmd) {
        case 'parse': echo dump(date_parse($f[1])), "\n"; break;
        case 'pff': echo dump(date_parse_from_format($f[1], $f[2])), "\n"; break;
        case 'strtotime': $r = strtotime($f[3], (int)$f[2]); echo $r === false ? 'F' : $r, "\n"; break;
        case 'date': echo date($f[3], (int)$f[2]), "\n"; break;
        case 'create':
            // "now" is pinned via the same base as the Rust probe.
            $d = date_create('@1136073600'); $d->setTimezone(new DateTimeZone($tz));
            $r = $f[2] === '' ? $d : (function() use ($f, $d) { try { $x = new DateTime($f[2]); return $x; } catch (Throwable $e) { return null; } })();
            echo $r ? $r->format($f[3]) : 'ERR', "\n"; break;
        case 'cff':
            $r = DateTime::createFromFormat($f[2], $f[3]);
            echo $r ? $r->format($f[4]) : 'ERR', "\n"; break;
        case 'modify':
            try { $x = new DateTime($f[2]); $r = @$x->modify($f[3]); echo $r ? $x->format($f[4]) : 'ERR', "\n"; }
            catch (Throwable $e) { echo "ERR\n"; }
            break;
        case 'diff':
            try { $a = new DateTime($f[2]); $b = new DateTime($f[3]); echo $a->diff($b)->format($f[4]), "\n"; }
            catch (Throwable $e) { echo "ERR\n"; }
            break;
        case 'interval':
            try { echo (new DateInterval($f[1]))->format($f[2]), "\n"; } catch (Throwable $e) { echo "ERR ", $e->getMessage(), "\n"; }
            break;
        case 'datestr':
            try { $i = @DateInterval::createFromDateString($f[1]); echo $i ? $i->format($f[2]) : 'ERR', "\n"; } catch (Throwable $e) { echo "ERR\n"; }
            break;
        case 'add': case 'sub':
            date_default_timezone_set($f[1]);
            try {
                $d = new DateTime($f[2]);
                $i = $f[3][0] === 'P' ? new DateInterval($f[3]) : DateInterval::createFromDateString($f[3]);
                $r = $cmd === 'add' ? $d->add($i) : @$d->sub($i);
                echo $r ? $d->format($f[4]) : 'ERR', "\n";
            } catch (Throwable $e) { echo "ERR\n"; }
            break;
        case 'period':
            try {
                $p = new DatePeriod($f[1], (int)$f[2]); $o = [];
                foreach ($p as $d) { $o[] = $d->format($f[3]); if (count($o) >= 50) break; }
                echo implode('|', $o), "\n";
            } catch (Throwable $e) { echo "ERR ", $e->getMessage(), "\n"; }
            break;
        case 'sun':
            date_default_timezone_set($f[1]);
            echo dump(date_sun_info((int)$f[2], (float)$f[3], (float)$f[4])), "\n"; break;
        case 'abbr':
            $r = timezone_name_from_abbr($f[1], (int)$f[2], (int)$f[3]); echo $r === false ? 'F' : $r, "\n"; break;
        case 'tzinit':
            try { $z = new DateTimeZone($f[1]); $a = (array)$z; echo $a['timezone_type'], ' ', $z->getName(), "\n"; }
            catch (Throwable $e) { echo "ERR ", preg_replace('/^DateTimeZone::__construct\(\): /', '', $e->getMessage()), "\n"; }
            break;
        case 'strftime': case 'gmstrftime':
            date_default_timezone_set($f[1]);
            $r = @$cmd($f[3], (int)$f[2]); echo $r === false ? 'F' : $r, "\n"; break;
        case 'idate':
            date_default_timezone_set($f[1]);
            try { $r = @idate($f[3], (int)$f[2]); echo $r === false ? 'F' : $r, "\n"; } catch (Throwable $e) { echo "F\n"; }
            break;
        case 'getdate':
            date_default_timezone_set($f[1]);
            echo dump(getdate((int)$f[2])), '|', dump(localtime((int)$f[2], true)), "\n"; break;
        case 'mktime': case 'gmmktime':
            date_default_timezone_set($f[1]);
            // Pin "now" by passing every argument we have; missing ones come from 2006-01-01 00:00:00.
            $a = [];
            for ($i = 2; $i < 8; $i++) $a[] = $f[$i] === '' ? null : (int)$f[$i];
            $now = 1136073600;
            $tz = $cmd === 'mktime' ? new DateTimeZone($f[1]) : new DateTimeZone('UTC');
            $d = new DateTime('@' . $now); $d->setTimezone($tz);
            $defs = [(int)$d->format('G'), (int)$d->format('i'), (int)$d->format('s'), (int)$d->format('n'), (int)$d->format('j'), (int)$d->format('Y')];
            foreach ($a as $k => $v) if ($v === null) $a[$k] = $defs[$k];
            echo $cmd(...$a), "\n"; break;
        default: echo "?\n";
    }
    $r = ob_get_clean();
    if (substr($r, -1) === "\n") $r = substr($r, 0, -1);
    echo strtr($r, ["\\" => "\\\\", "\t" => "\\t", "\n" => "\\n", "\0" => "\\0"]), "\n";
}
