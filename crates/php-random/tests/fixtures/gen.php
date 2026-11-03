<?php
// Generates the oracle fixtures replayed by tests/oracle.rs.
// Usage: php gen.php > oracle.txt   (run with the reference PHP binary)
error_reporting(E_ALL & ~E_DEPRECATED);

use Random\Engine\Mt19937;
use Random\Engine\PcgOneseq128XslRr64;
use Random\Engine\Xoshiro256StarStar;
use Random\IntervalBoundary;
use Random\Randomizer;

$P = new Randomizer(new Xoshiro256StarStar(20240601)); // parameter chooser

function fbits(float $f): string { return bin2hex(pack('E', $f)); }
function bound_name(IntervalBoundary $b): string { return $b->name; }

$BOUNDS = [PHP_INT_MIN, PHP_INT_MIN + 1, -(2**62), -(2**32) - 1, -(2**32), -(2**31) - 1, -(2**31),
    -1000, -1, 0, 1, 2, 3, 7, 100, 255, 256, 1000, 2**31 - 1, 2**31, 2**32 - 1, 2**32, 2**32 + 1,
    2**53, 2**62, PHP_INT_MAX - 1, PHP_INT_MAX];

function pick_range(Randomizer $P, array $B): array {
    switch ($P->getInt(0, 3)) {
        case 0:
            $a = $B[$P->getInt(0, count($B) - 1)];
            $b = $B[$P->getInt(0, count($B) - 1)];
            return $a <= $b ? [$a, $b] : [$b, $a];
        case 1:
            $a = $P->getInt(-50, 50);
            return [$a, $a + $P->getInt(0, 40)];
        case 2:
            $a = $B[$P->getInt(0, count($B) - 1)];
            $d = $P->getInt(0, 1000);
            return $a <= PHP_INT_MAX - $d ? [$a, $a + $d] : [$a - $d, $a];
        default:
            $a = $P->getInt(PHP_INT_MIN, PHP_INT_MAX);
            $b = $P->getInt(PHP_INT_MIN, PHP_INT_MAX);
            return $a <= $b ? [$a, $b] : [$b, $a];
    }
}

function pick_float_range(Randomizer $P): array {
    $vals = [0.0, -0.0, 1.0, -1.0, 0.5, 1e-300, -1e-300, 1e300, -1e300, 1e17, 3.0, -3.0,
        PHP_FLOAT_EPSILON, 1.0 + PHP_FLOAT_EPSILON, 2.0, 1e-10, 123.456, -0.1, 0.1, 1e-320, PHP_FLOAT_MAX, -PHP_FLOAT_MAX];
    if ($P->getInt(0, 2) == 0) {
        $a = $P->getFloat(-1000, 1000);
        $b = $a + $P->getFloat(0, 10);
    } else {
        $a = $vals[$P->getInt(0, count($vals) - 1)];
        $b = $vals[$P->getInt(0, count($vals) - 1)];
    }
    return $a <= $b ? [$a, $b] : [$b, $a];
}

function mask_array(string $mask): array {
    $a = [];
    for ($i = 0; $i < strlen($mask); $i++) $a[$i] = $i;
    for ($i = 0; $i < strlen($mask); $i++) if ($mask[$i] === '0') unset($a[$i]);
    return $a;
}

function rand_mask(Randomizer $P): string {
    $n = $P->getInt(1, 40);
    $dense = $P->getInt(0, 2);
    $m = '';
    for ($i = 0; $i < $n; $i++) {
        $m .= ($dense == 0 || $P->getInt(0, 3) > 0) ? '1' : '0';
        if ($dense == 2 && $P->getInt(0, 1)) $m[$i] = '0';
    }
    if (strpos($m, '1') === false) $m[$P->getInt(0, $n - 1)] = '1';
    return $m;
}

function engine_ops(Randomizer $P, Randomizer $r, array $B, int $count, bool $is_mt) {
    $e = $r->engine;
    for ($k = 0; $k < $count; $k++) {
        $op = $P->getInt(0, 16);
        switch ($op) {
            case 0:
                echo "gen = ", bin2hex($e->generate()), "\n"; break;
            case 1: case 2: case 3:
                [$a, $b] = pick_range($P, $B);
                echo "int $a $b = ", $r->getInt($a, $b), "\n"; break;
            case 4:
                echo "nextint = ", $r->nextInt(), "\n"; break;
            case 5:
                $n = $P->getInt(1, 40);
                echo "bytes $n = ", bin2hex($r->getBytes($n)), "\n"; break;
            case 6:
                $choices = ['a', 'ab', 'abc', '0123456789abcdef', 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789',
                    str_repeat('x', 255) . 'y', str_repeat('q', 256) . 'z', str_repeat('m', 1000) . 'n', 'ab' . str_repeat('c', 127)];
                $s = $choices[$P->getInt(0, count($choices) - 1)];
                $n = $P->getInt(1, 30);
                echo "bfs ", bin2hex($s), " $n = ", bin2hex($r->getBytesFromString($s, $n)), "\n"; break;
            case 7:
                echo "nextfloat = ", fbits($r->nextFloat()), "\n"; break;
            case 8: case 9:
                [$a, $b] = pick_float_range($P);
                $bs = IntervalBoundary::cases();
                $bd = $bs[$P->getInt(0, 3)];
                try {
                    $v = fbits($r->getFloat($a, $b, $bd));
                } catch (ValueError $ex) {
                    $v = 'ERR';
                }
                echo "float ", fbits($a), " ", fbits($b), " ", $bd->name, " = $v\n"; break;
            case 10:
                $n = $P->getInt(0, 30);
                echo "shuffle $n = ", implode(',', $r->shuffleArray(range(0, max(0, $n - 1)))), "\n"; break;
            case 11:
                $s = ($l = $P->getInt(0, 30)) ? $P->getBytes($l) : "";
                echo "shufbytes ", bin2hex($s) ?: '-', " = ", bin2hex($r->shuffleBytes($s)) ?: '-', "\n"; break;
            case 12: case 13:
                $m = rand_mask($P);
                $avail = substr_count($m, '1');
                $num = $P->getInt(0, 3) == 0 ? 1 : $P->getInt(1, $avail);
                echo "pick $m $num = ", implode(',', $r->pickArrayKeys(mask_array($m), $num)), "\n"; break;
            case 14:
                $st = $e->__serialize()[1];
                if (count($st) > 100) $st = array_merge(array_slice($st, 0, 8), array_slice($st, -4));
                echo "ser = ", implode(',', $st), "\n"; break;
            case 15:
                if ($e instanceof PcgOneseq128XslRr64) {
                    $n = $P->getInt(0, 3) == 0 ? $P->getInt(0, PHP_INT_MAX) : $P->getInt(0, 1000);
                    $e->jump($n); echo "jump $n\n";
                } elseif ($e instanceof Xoshiro256StarStar) {
                    if ($P->getInt(0, 1)) { $e->jump(); echo "jump\n"; } else { $e->jumpLong(); echo "jumplong\n"; }
                } else {
                    $n = $P->getInt(1, 700);
                    for ($i = 0; $i < $n; $i++) $e->generate();
                    echo "skip $n\n";
                }
                break;
            default:
                $n = $P->getInt(1, 8);
                $o = [];
                for ($i = 0; $i < $n; $i++) $o[] = bin2hex($e->generate());
                echo "gens = ", implode(',', $o), "\n";
        }
    }
}

// Engine-based scenarios.
for ($case = 0; $case < 120; $case++) {
    $kind = $case % 6;
    switch ($kind) {
        case 0: case 1:
            $seed = $P->getInt(0, 3) == 0 ? $B = $BOUNDS[$P->getInt(0, count($BOUNDS) - 1)] : $P->getInt(0, 100000);
            $mode = $kind == 1 ? MT_RAND_PHP : MT_RAND_MT19937;
            echo "new mt $seed $mode\n";
            $e = new Mt19937($seed, $mode);
            break;
        case 2:
            $seed = $P->getInt(0, 1) ? $BOUNDS[$P->getInt(0, count($BOUNDS) - 1)] : $P->getInt(-1000, 1000);
            echo "new pcg int $seed\n";
            $e = new PcgOneseq128XslRr64($seed);
            break;
        case 3:
            $s = $P->getBytes(16);
            echo "new pcg str ", bin2hex($s), "\n";
            $e = new PcgOneseq128XslRr64($s);
            break;
        case 4:
            $seed = $P->getInt(0, 1) ? $BOUNDS[$P->getInt(0, count($BOUNDS) - 1)] : $P->getInt(-1000, 1000);
            echo "new xo int $seed\n";
            $e = new Xoshiro256StarStar($seed);
            break;
        default:
            $s = $P->getBytes(32);
            echo "new xo str ", bin2hex($s), "\n";
            $e = new Xoshiro256StarStar($s);
    }
    engine_ops($P, new Randomizer($e), $BOUNDS, 40, $kind < 2);
}

// Global-state scenarios.
for ($case = 0; $case < 60; $case++) {
    $seed = $P->getInt(0, 2) == 0 ? $BOUNDS[$P->getInt(0, count($BOUNDS) - 1)] : $P->getInt(0, 100000);
    $mode = $P->getInt(0, 2) == 0 ? MT_RAND_PHP : MT_RAND_MT19937;
    echo "mt_srand $seed $mode\n";
    mt_srand($seed, $mode);
    for ($k = 0; $k < 30; $k++) {
        switch ($P->getInt(0, 9)) {
            case 0: echo "mt_rand = ", mt_rand(), "\n"; break;
            case 1: case 2:
                [$a, $b] = pick_range($P, $BOUNDS);
                echo "mt_rand $a $b = ", mt_rand($a, $b), "\n"; break;
            case 3: echo "rand = ", rand(), "\n"; break;
            case 4:
                [$a, $b] = pick_range($P, $BOUNDS);
                if ($P->getInt(0, 1)) [$a, $b] = [$b, $a];
                echo "rand $a $b = ", rand($a, $b), "\n"; break;
            case 5:
                $n = $P->getInt(0, 30);
                $arr = range(0, max(0, $n - 1));
                if ($n == 0) $arr = [];
                shuffle($arr);
                echo "g_shuffle $n = ", implode(',', $arr), "\n"; break;
            case 6:
                $s = ($l = $P->getInt(0, 30)) ? $P->getBytes($l) : "";
                echo "str_shuffle ", bin2hex($s) ?: '-', " = ", bin2hex(str_shuffle($s)) ?: '-', "\n"; break;
            case 7: case 8:
                $m = rand_mask($P);
                $avail = substr_count($m, '1');
                $num = $P->getInt(0, 2) == 0 ? 1 : $P->getInt(1, $avail);
                echo "array_rand $m $num = ", implode(',', (array) array_rand(mask_array($m), $num)), "\n"; break;
            default:
                $n = $P->getInt(1, 700);
                for ($i = 0; $i < $n; $i++) mt_rand();
                echo "g_skip $n\n";
        }
    }
}
