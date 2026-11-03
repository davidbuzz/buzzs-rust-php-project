<?php
// Generates tests/data/oracle.txt: one base64(serialize(case)) per line.
// Run with: php crates/php-stdarr/tests/data/gen_oracle.php > crates/php-stdarr/tests/data/oracle.txt
// (or `cargo test -p php-stdarr -- --ignored regenerate_oracle`).
//
// Every case is ['func' => name, 'args' => serialized args, 'result' =>
// serialized result or null, 'byref' => serialized args after the call (for
// functions taking references), 'msgs' => [diagnostics], 'exc' => "Class:
// message" or null]. Arguments go through unserialize(serialize()) first so
// that both implementations see identically built arrays.
//
// Callbacks are written as "@name" strings and resolved from $CB (the Rust
// harness implements the same set).
//
// Where php-src master (the spec) differs from the installed oracle, the
// expected text is rewritten by $MASTER_FIXUPS below.

error_reporting(E_ALL);
ini_set('precision', '14');
ini_set('serialize_precision', '-1');

$calls = 0;
$CB = [
    'cmp' => fn($a, $b) => $a <=> $b,
    'rcmp' => fn($a, $b) => $b <=> $a,
    'cmp_len' => fn($a, $b) => strlen($a) <=> strlen($b),
    'cmp_case' => fn($a, $b) => strcasecmp($a, $b),
    'cmp_zero' => fn($a, $b) => 0,
    'cmp_float' => fn($a, $b) => ($a - $b) * 0.1,
    'cmp_str' => fn($a, $b) => strcmp((string)$a, (string)$b),
    'is_odd' => fn($v) => $v % 2 == 1,
    'truthy' => fn($v) => (bool)$v,
    'double' => fn($v) => $v * 2,
    'is_str' => fn($v) => is_string($v),
    'both' => fn($v, $k) => is_int($k) && $v > 1,
    'sum2' => fn($c, $v) => $c + $v,
    'concat' => fn(...$a) => implode(',', array_map(fn($x) => var_export($x, true), $a)),
    'pair' => fn($a, $b) => [$a, $b],
    'gt2' => fn($v) => $v > 2,
    'walk_append' => function (&$v, $k, $p = null) { $v = $v . '_' . $k . ($p ?? ''); },
    'walk_double' => function (&$v) { $v = $v * 2; },
    'throw' => function () { throw new Error('boom'); },
    'v_k' => fn($v, $k) => "$k=$v",
];

if (!function_exists('array_find')) {
    function array_find(array $array, callable $callback): mixed {
        foreach ($array as $k => $v) { if ($callback($v, $k)) return $v; }
        return null;
    }
    function array_find_key(array $array, callable $callback): mixed {
        foreach ($array as $k => $v) { if ($callback($v, $k)) return $k; }
        return null;
    }
    function array_any(array $array, callable $callback): bool {
        foreach ($array as $k => $v) { if ($callback($v, $k)) return true; }
        return false;
    }
    function array_all(array $array, callable $callback): bool {
        foreach ($array as $k => $v) { if (!$callback($v, $k)) return false; }
        return true;
    }
}
if (!function_exists('array_first')) {
    function array_first(array $array): mixed { foreach ($array as $v) return $v; return null; }
    function array_last(array $array): mixed { return $array ? $array[array_key_last($array)] : null; }
}

// Texts that changed between the installed PHP and php-src master.
$MASTER_FIXUPS = [
    'Argument #1 ($array) cannot be empty' => 'Argument #1 ($array) must not be empty',
];

$msgs = [];
set_error_handler(function ($no, $str) use (&$msgs) {
    $label = match ($no) {
        E_WARNING, E_USER_WARNING => 'Warning',
        E_NOTICE, E_USER_NOTICE => 'Notice',
        E_DEPRECATED, E_USER_DEPRECATED => 'Deprecated',
        default => "E$no",
    };
    $msgs[] = "$label: $str";
    return true;
});

function resolve($v) {
    global $CB;
    if (is_string($v) && strlen($v) > 1 && $v[0] === '@' && isset($CB[substr($v, 1)])) {
        return $CB[substr($v, 1)];
    }
    return $v;
}

const BYREF = ['sort', 'rsort', 'asort', 'arsort', 'ksort', 'krsort', 'natsort', 'natcasesort',
    'usort', 'uasort', 'uksort', 'array_multisort', 'array_walk', 'array_walk_recursive',
    'array_push', 'array_pop', 'array_shift', 'array_unshift', 'array_splice', 'shuffle'];

function fixup($s) {
    global $MASTER_FIXUPS;
    return strtr($s, $MASTER_FIXUPS);
}

$out = [];
function run(string $func, array $args) {
    global $msgs, $out;
    $args = unserialize(serialize($args));
    $sargs = serialize($args);
    $msgs = [];
    $exc = null;
    $result = null;
    $call = array_map('resolve', $args);
    if ($func === 'extract' || $func === 'compact') {
        $fn = "do_$func";
    } elseif (str_starts_with($func, 'seed:')) {
        $fn = substr($func, 5);
        mt_srand(array_shift($call));
    } else {
        $fn = $func;
    }
    try {
        $r = $fn(...$call);
        $result = serialize($r);
    } catch (Throwable $e) {
        $exc = get_class($e) . ': ' . fixup($e->getMessage());
    }
    $byref = null;
    $base = str_starts_with($func, 'seed:') ? substr($func, 5) : $func;
    if (in_array($base, BYREF, true)) {
        $after = array_map(fn($v) => $v instanceof Closure ? null : $v, $call);
        $byref = serialize(array_values($after));
    }
    $out[] = base64_encode(serialize([
        'func' => $func,
        'args' => $sargs,
        'result' => $result,
        'byref' => $byref,
        'msgs' => array_map('fixup', $msgs),
        'exc' => $exc,
    ]));
}

function do_extract($__vars, $__arr, ...$__rest) {
    extract($__vars);
    unset($__vars);
    $__count = extract($__arr, ...$__rest);
    unset($__arr, $__rest);
    $__defined = get_defined_vars();
    unset($__defined['__count']);
    ksort($__defined);
    return [$__count, $__defined];
}

function do_compact($__vars, ...$__names) {
    extract($__vars);
    unset($__vars);
    return compact(...$__names);
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------
$holes = [0, 1, 2, 3, 4, 5];
unset($holes[1], $holes[4]);
$holes_hash = ['a' => 1, 'b' => 2, 'c' => 3, 5 => 4, 6 => 5];
unset($holes_hash['b'], $holes_hash[5]);
$inputs = [
    'empty' => [],
    'one' => [42],
    'ints' => [3, 1, 2, 10, -5, 0, 2],
    'mixed' => [3, "10", "9", "abc", 1.5, true, null, "1e1", "", "a", 0, -1, " 5", "5 ", "0.0"],
    'assoc' => ['b' => 2, 'a' => 1, 'c' => 3, 10 => 'x', '9' => 'y', '-3' => 'z', 'B' => 0],
    'strs' => ["img12.png", "img10.png", "IMG2.png", "img1.png", "Img3.png", "img2.png", "img02.png", " img4.png"],
    'floats' => [1.5, -0.0, 0.0, 2.25, 1e100, -1e-10, 1.0, 0.1 + 0.2],
    'dups' => [1, "1", 1.0, "01", true, "a", "A", "a", null, "", 0, false, "1", 2, "2"],
    'nested' => [[1, 2], [1], [2, 1], 'x' => [3], [], [[1]], [1, 2]],
    'holes' => $holes,
    'holes_hash' => $holes_hash,
    'negkeys' => [-5 => 'a', 'b', 'c'],
    'bigint' => [PHP_INT_MAX, PHP_INT_MIN, PHP_INT_MAX - 1, 9007199254740993, 9.007199254740992E15],
    'numkeys' => ["1" => 'a', "01" => 'b', "1.5" => 'c', " 1" => 'd', "-0" => 'e', 2 => 'f'],
    'bools' => [true, false, true, null],
    'cases' => ['Apple', 'apple', 'Banana', 'banana', 'cherry', 'Cherry', 'apple'],
    'numstr' => ["10", "9", "2", "1", "1.5", "1e1", "0x1A", "011", " 3", "3 "],
    'keyorder' => [10 => 'a', 9 => 'b', 'x' => 'c', '-1' => 'd', 'X' => 'e', '' => 'f', 0 => 'g'],
];

$sortflags = [0, 1, 2, 2 | 8, 5, 6, 6 | 8, 3, 8];

// ---------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------
foreach (['sort', 'rsort', 'asort', 'arsort', 'ksort', 'krsort'] as $f) {
    foreach ($inputs as $in) {
        foreach ($sortflags as $fl) {
            run($f, [$in, $fl]);
        }
    }
}
foreach (['natsort', 'natcasesort'] as $f) {
    foreach ($inputs as $name => $in) {
        if ($name === 'nested') continue;
        run($f, [$in]);
    }
}
foreach (['usort', 'uasort', 'uksort'] as $f) {
    foreach (['ints', 'assoc', 'strs', 'mixed', 'holes', 'empty', 'one', 'cases', 'keyorder'] as $name) {
        foreach (['@cmp', '@rcmp', '@cmp_zero', '@cmp_float', '@cmp_str'] as $cb) {
            run($f, [$inputs[$name], $cb]);
        }
    }
    run($f, [[3, 1, 2], '@throw']);
}
run('usort', [['bb', 'a', 'ccc', 'dd'], '@cmp_len']);
run('uasort', [['x' => 'bb', 'y' => 'a', 'z' => 'ccc'], '@cmp_len']);

// array_multisort
run('array_multisort', [[3, 1, 2]]);
run('array_multisort', [[3, 1, 2], [1, 2, 3]]);
run('array_multisort', [[1, 1, 2], 3, [3, 2, 1]]);
run('array_multisort', [[1, 1, 2], 4, 2, ['b', 'a', 'c']]);
run('array_multisort', [['a' => 3, 'b' => 1, 5 => 2, 7 => 0]]);
run('array_multisort', [['10', 9, 2, '1'], 2, 3]);
run('array_multisort', [['10', 9, 2, '1'], 1]);
run('array_multisort', [['B', 'a', 'C', 'b'], 2 | 8]);
run('array_multisort', [['img12', 'img10', 'img2'], 6]);
run('array_multisort', [[1, 2], [1]]);
run('array_multisort', [[1, 2], 3, 3]);
run('array_multisort', [[1, 2], 0, 1]);
run('array_multisort', [[1, 2], 99]);
run('array_multisort', [[1, 2], 'x']);
run('array_multisort', [3]);
run('array_multisort', [[], []]);
run('array_multisort', [$holes, [1, 2, 3, 4]]);
run('array_multisort', [[3, 1, 3, 1], 3, [1, 2, 3, 4], 4]);
run('array_multisort', [['x' => 1, 'y' => 1, 'z' => 0], [3, 2, 1]]);

// ---------------------------------------------------------------------------
// count, min, max, search
// ---------------------------------------------------------------------------
foreach ($inputs as $in) {
    run('count', [$in]);
    run('count', [$in, 1]);
    run('min', [$in]);
    run('max', [$in]);
    run('array_sum', [$in]);
    run('array_product', [$in]);
    run('array_values', [$in]);
    run('array_keys', [$in]);
    run('array_count_values', [$in]);
    run('array_flip', [$in]);
    run('array_change_key_case', [$in]);
    run('array_change_key_case', [$in, 1]);
    run('array_key_first', [$in]);
    run('array_key_last', [$in]);
    run('array_first', [$in]);
    run('array_last', [$in]);
    run('array_is_list', [$in]);
    run('array_reverse', [$in]);
    run('array_reverse', [$in, true]);
    run('array_unique', [$in]);
    foreach ([0, 1, 5, 6, 2 | 8, 6 | 8] as $fl) {
        run('array_unique', [$in, $fl]);
    }
    run('array_fill_keys', [$in, 'v']);
    run('array_filter', [$in]);
    foreach ([[0, null], [1, null], [-2, null], [2, 3], [-3, -1], [1, -1], [10, null], [0, 0], [-100, 2]] as [$o, $l]) {
        run('array_slice', [$in, $o, $l]);
        run('array_slice', [$in, $o, $l, true]);
    }
    foreach ([1, 2, 3, 100] as $sz) {
        run('array_chunk', [$in, $sz]);
        run('array_chunk', [$in, $sz, true]);
    }
    foreach ([0, 3, -3, 10, -10] as $sz) {
        run('array_pad', [$in, $sz, 'p']);
    }
    foreach ([1, "1", 0, "0", "", null, true, false, "a", 1.5, "abc", [], [1], "10", 10] as $needle) {
        run('in_array', [$needle, $in]);
        run('in_array', [$needle, $in, true]);
        run('array_search', [$needle, $in]);
        run('array_search', [$needle, $in, true]);
        run('array_keys', [$in, $needle]);
        run('array_keys', [$in, $needle, true]);
    }
    foreach ([0, 1, '1', 'a', 'b', -5, 1.7, true, false, '', 'x'] as $k) {
        run('array_key_exists', [$k, $in]);
    }
    run('array_column', [$in, null]);
    run('array_column', [$in, 0]);
    run('array_column', [$in, 1, 0]);
    run('array_map', ['@concat', $in]);
    run('array_map', [null, $in]);
    run('array_map', [null, $in, [1, 2]]);
    run('array_map', ['@concat', $in, ['a' => 'x']]);
    run('array_combine', [$in, $in]);
    run('array_combine', [array_values($in), array_keys($in)]);
}
run('count', [[1, [2, [3, 4]], [5]], 1]);
run('count', [[1, 2], 2]);
run('count', [5]);
run('count', ["str"]);
run('count', [null]);
run('min', [1, 2, 0.5]);
run('min', [3, "2", 2.5]);
run('min', ["apple", "banana", 0]);
run('min', ["10", "9", 8]);
run('min', [1.0, 1]);
run('min', [1, 1.0]);
run('min', [[1, 2], [0, 5], 3]);
run('min', [PHP_INT_MAX, PHP_INT_MAX - 1, 9.2233720368547758E18]);
run('min', [null, false, 0, ""]);
run('min', [5]);
run('min', [[]]);
run('max', [1, 2, 0.5]);
run('max', [3, "2", 2.5, "3"]);
run('max', ["apple", "banana", 0]);
run('max', ["10", "9", 8]);
run('max', [1.0, 1]);
run('max', [1, 1.0]);
run('max', [[1, 2], [0, 5], 3]);
run('max', [INF, NAN, 1]);
run('max', [NAN, 1, INF]);
run('min', [NAN, 1, -INF]);
run('max', [true, 2]);
run('max', [5]);
run('max', [[]]);
run('max', ['a', [], 1]);

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------
foreach ([[0, 3], [5, 2], [-3, 4], [0, 0], [2, -1], [PHP_INT_MAX, 1], [PHP_INT_MAX, 2], [-1, 1], [3, 1]] as [$s, $n]) {
    run('array_fill', [$s, $n, 'v']);
}
run('array_fill', [0, 2147483648, 1]);
run('array_fill_keys', [[1, 'a', 1.5, true, null, '1', 'a'], 0]);
run('array_fill_keys', [[[1]], 0]);
run('array_combine', [[1, 2], [3]]);
run('array_combine', [[], []]);
run('array_combine', [['a', 1.5, true, null, 'a'], [1, 2, 3, 4, 5]]);
run('array_chunk', [[1, 2, 3], 0]);
run('array_chunk', [[1, 2, 3], -1]);
run('array_pad', [[1, 2], 1073741825, 0]);
run('array_pad', [['a' => 1, 5 => 2, 3], 5, 0]);
run('array_pad', [['a' => 1, 5 => 2, 3], -5, 0]);
run('array_change_key_case', [['ab' => 1, 'AB' => 2, 'Ab' => 3], 0]);
run('array_change_key_case', [['ab' => 1, 'AB' => 2, 'Ab' => 3], 1]);

$rows = [
    ['id' => 1, 'name' => 'a', 'g' => 'x', 5 => 'five'],
    ['id' => 2, 'name' => 'b', 5 => 'cinq'],
    ['id' => 3, 'g' => 'y'],
    'notarray',
    ['id' => 1.5, 'name' => 'c', 'g' => 0],
    ['id' => true, 'name' => 'd', 'g' => false],
    ['id' => '7', 'name' => 'e'],
    ['id' => false, 'name' => 'f'],
];
foreach ([[null, null], ['name', null], ['name', 'id'], [null, 'id'], ['g', 'name'], [5, null], ['5', 'id'], ['missing', null], ['name', 'missing'], ['name', 'g']] as [$c, $i]) {
    run('array_column', [$rows, $c, $i]);
}
run('array_column', [[['id' => [1], 'v' => 1]], 'v', 'id']);

// range
$range_cases = [
    [1, 5], [5, 1], [1, 5, 2], [5, 1, 2], [1, 5, -1], [5, 1, -2], [0, 1, 0.25], [1, 0, 0.25],
    [1, 2, 0], [1, 2, 0.0], [1, 1], [1.5, 4], [1, 4.5], ['a', 'e'], ['e', 'a'], ['a', 'e', 2],
    ['a', 'z', 30], ['A', 'z', 5], ['a', 'a'], ['aa', 'cc'], ['', 'c'], ['a', ''], ['', ''],
    ['1', '3'], ['1', 3], [1, '3'], ['a', 3], [3, 'a'], ['a', '3'], ['1.5', '3'], ['1e1', 12],
    ['a', 'e', 1.5], ['1', '5', 1.5], [1, 5, 1.0], [1, 2, 3], [5, 1, 10], [1, 10, -3],
    [0, 0.1, 0.01], [1, 2, INF], [1, 2, NAN], [INF, 2], [1, NAN], [PHP_INT_MIN, PHP_INT_MAX, PHP_INT_MAX],
    [-3, 3, 2], [-3, 3, 2], [3.0, 1.0, 0.5],
    [' 1', 3], ['x', 'z', 1.0], ['z', 'x', -1], ['a', 'c', -1], [1, 3, 1.1], ['A', 'C', 0.5],
    [PHP_INT_MAX - 2, PHP_INT_MAX], [PHP_INT_MIN + 2, PHP_INT_MIN], [0, -5, 2], ['9', '0'],
];
foreach ($range_cases as $rc) {
    run('range', $rc);
}

// ---------------------------------------------------------------------------
// Stack functions
// ---------------------------------------------------------------------------
foreach (['ints', 'assoc', 'holes', 'holes_hash', 'negkeys', 'empty', 'numkeys', 'one'] as $name) {
    $in = $inputs[$name];
    run('array_push', [$in, 'x', 'y']);
    run('array_pop', [$in]);
    run('array_shift', [$in]);
    run('array_unshift', [$in, 'x', 'y']);
    foreach ([[1, null], [0, 1], [-2, 1], [1, -1], [10, null], [-10, 2], [2, 0], [0, null]] as [$o, $l]) {
        run('array_splice', [$in, $o, $l]);
        run('array_splice', [$in, $o, $l, ['r1', 'k' => 'r2']]);
        run('array_splice', [$in, $o, $l, 'scalar']);
    }
}
run('array_push', [[PHP_INT_MAX => 1], 2]);
run('array_splice', [[1, 2, 3], 1, null, null]);

// ---------------------------------------------------------------------------
// Merge / replace
// ---------------------------------------------------------------------------
$m = [
    [[1, 2], [3]],
    [['a' => 1, 'b' => 2], ['b' => 3, 'c' => 4]],
    [['a' => 1, 5 => 'x'], [5 => 'y', 'a' => [2]]],
    [[], ['a' => 1, 3 => 2]],
    [['a' => 1, 3 => 2], []],
    [[], ['a' => 1, 'b' => 2]],
    [$holes, []],
    [[], $holes],
    [['a' => [1, 'x' => 2], 'b' => 3], ['a' => [3, 'x' => 4], 'b' => [5]], ['a' => 'z']],
    [['a' => null], ['a' => 1]],
    [['a' => 1], ['a' => null]],
    [['a' => 'x'], ['a' => ['y']]],
    [['k' => ['a' => ['b' => 1]]], ['k' => ['a' => ['b' => 2, 'c' => 3]]]],
    [[-5 => 1], [-3 => 2]],
    [['x' => 1]],
    [],
    [[1], 2],
    [1, [2]],
];
foreach ($m as $args) {
    run('array_merge', $args);
    run('array_merge_recursive', $args);
    if ($args) {
        run('array_replace', $args);
        run('array_replace_recursive', $args);
    }
}

// ---------------------------------------------------------------------------
// Set operations
// ---------------------------------------------------------------------------
$sets = [
    [[1, 2, 3, 4], [2, 4, 6]],
    [['a' => 'green', 'red', 'blue', 'red'], ['b' => 'green', 'yellow', 'red']],
    [['a' => 'green', 'b' => 'brown', 'c' => 'blue', 'red'], ['a' => 'GREEN', 'B' => 'brown', 'yellow', 'red']],
    [[1, '1', 1.0, '01', true, null, ''], ['1', '']],
    [[1, 2, 3], [1, 2, 3], [2, 3, 4]],
    [[1, 2, 3], []],
    [[], [1]],
    [[5 => 'a', 6 => 'b', 'x' => 'c'], [5 => 'a', 'x' => 'd'], [5 => 'a', 6 => 'b']],
    [['a' => 1, 'b' => 2, 'c' => 3], ['A' => 1, 'b' => 2]],
    [[1.5, '1.5', 2], [1.5]],
    [[3, 1, 2, 3, 1], [1, 3]],
    [[3, 1, 2, 3, 1], [1, 3], [3]],
    [['x'], 5],
    [5, ['x']],
    [[1, 2]],
    [[0 => 'a', 1 => 'b', 2 => 'c'], ['1' => 'b', '2' => 'c'], [2 => 'c']],
];
foreach ($sets as $args) {
    foreach (['array_diff', 'array_diff_key', 'array_diff_assoc', 'array_intersect_key', 'array_intersect_assoc'] as $f) {
        run($f, $args);
    }
    run('array_intersect', $args);
    foreach (['@cmp', '@cmp_str', '@cmp_zero'] as $cb) {
        run('array_udiff', array_merge($args, [$cb]));
        run('array_uintersect', array_merge($args, [$cb]));
        run('array_diff_ukey', array_merge($args, [$cb]));
        run('array_intersect_ukey', array_merge($args, [$cb]));
        run('array_diff_uassoc', array_merge($args, [$cb]));
        run('array_intersect_uassoc', array_merge($args, [$cb]));
        run('array_udiff_assoc', array_merge($args, [$cb]));
        run('array_uintersect_assoc', array_merge($args, [$cb]));
        run('array_udiff_uassoc', array_merge($args, [$cb, '@cmp']));
        run('array_uintersect_uassoc', array_merge($args, [$cb, '@cmp']));
        run('array_udiff_uassoc', array_merge($args, ['@cmp', $cb]));
        run('array_uintersect_uassoc', array_merge($args, ['@cmp', $cb]));
    }
}
run('array_diff', [[1, [1], 2], [2]]);
run('array_diff', [[[1]], [2]]);
run('array_diff', [['a'], [[1], 'a']]);
run('array_diff_assoc', [[[1], 'a'], [[1], 'b']]);
run('array_intersect_assoc', [[[1], 'a'], [[1], 'b']]);
run('array_intersect', [[1, 2]]);

// ---------------------------------------------------------------------------
// Callbacks
// ---------------------------------------------------------------------------
foreach (['ints', 'assoc', 'holes', 'holes_hash', 'empty', 'mixed', 'bools'] as $name) {
    $in = $inputs[$name];
    run('array_filter', [$in, '@is_odd']);
    run('array_filter', [$in, '@truthy']);
    run('array_filter', [$in, '@is_str', 2]);
    run('array_filter', [$in, '@both', 1]);
    run('array_filter', [$in, null, 1]);
    run('array_map', ['@double', $in]);
    run('array_map', ['@concat', $in, [9, 8]]);
    run('array_map', [null, $in, $in]);
    run('array_reduce', [$in, '@sum2']);
    run('array_reduce', [$in, '@sum2', 10]);
    run('array_find', [$in, '@gt2']);
    run('array_find_key', [$in, '@gt2']);
    run('array_any', [$in, '@gt2']);
    run('array_all', [$in, '@gt2']);
    run('array_walk', [$in, '@walk_append']);
    run('array_walk', [$in, '@walk_append', '!']);
    run('array_walk_recursive', [$in, '@walk_append']);
}
run('array_walk_recursive', [[1, [2, [3]], 'k' => [4]], '@walk_append', '?']);
run('array_walk', [[1, 2], '@throw']);
run('array_map', ['@throw', [1]]);
run('array_filter', [[1], '@throw']);
run('array_reduce', [[1, 2], '@throw']);
run('array_map', [null, [1], 'x']);
run('array_map', [null, 'x']);
run('array_filter', [['a' => 1, 'b' => 2], '@v_k', 1]);

// ---------------------------------------------------------------------------
// extract / compact
// ---------------------------------------------------------------------------
$vars = ['a' => 'A0', 'b' => 'B0', 'pre_c' => 'PC0'];
$src = ['a' => 1, 'c' => 3, 'b' => 2, '1x' => 'bad', 5 => 'five', 'GLOBALS' => 'g', '' => 'empty', 'd e' => 'sp', "\x80" => 'hi'];
foreach ([0, 1, 2, 3, 4, 5, 6, 7, -1] as $t) {
    run('extract', [$vars, $src, $t, 'pre']);
}
run('extract', [$vars, $src]);
run('extract', [$vars, $src, 2]);
run('extract', [$vars, $src, 3, '1bad']);
run('extract', [$vars, $src, 3, '']);
run('extract', [$vars, ['this' => 1], 0]);
run('extract', [$vars, ['this' => 1], 6]);
run('extract', [$vars, ['this' => 1], 1]);
run('extract', [$vars, ['this' => 1], 2, 'p']);
run('extract', [$vars, ['this' => 1], 4, 'p']);
run('extract', [$vars, [1, 2, 3], 3, 'p']);
run('extract', [$vars, [1, 2, 3], 0]);
run('compact', [$vars, 'a', 'b']);
run('compact', [$vars, 'a', ['b', ['pre_c']], 'missing']);
run('compact', [$vars, 'a', 5, null]);
run('compact', [$vars, []]);

// ---------------------------------------------------------------------------
// Random (seeded Mt19937)
// ---------------------------------------------------------------------------
foreach ([1, 42, 1234] as $seed) {
    foreach (['ints', 'assoc', 'holes', 'holes_hash', 'one', 'empty', 'strs'] as $name) {
        run('seed:shuffle', [$seed, $inputs[$name]]);
        foreach ([1, 2, 3, 0, -1, 100] as $n) {
            run('seed:array_rand', [$seed, $inputs[$name], $n]);
        }
    }
    run('seed:str_shuffle', [$seed, 'abcdefghij']);
    run('seed:str_shuffle', [$seed, '']);
    run('seed:array_rand', [$seed, range(0, 99), 50]);
    run('seed:array_rand', [$seed, range(0, 99), 99]);
    $sparse = range(0, 99);
    foreach (range(0, 97) as $i) unset($sparse[$i]);
    run('seed:array_rand', [$seed, $sparse, 1]);
}

echo implode("\n", $out), "\n";
