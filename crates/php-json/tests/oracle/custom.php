<?php
// Hand-written oracle cases (run with wrappers.php prepended).

class Pub { public $a = 1; public $b = [1, 2]; protected $c = 3; private $d = 4; public $e = null; }
class OnlyHidden { protected $x = 1; private $y = 2; }
class Typed { public int $i = 5; public ?string $s = "str"; public array $arr = []; }
class WithDyn { public $first = 'f'; }

function t($f) { try { $f(); } catch (Throwable $e) {} }

$all_encode_flags = [
    0,
    JSON_HEX_TAG, JSON_HEX_AMP, JSON_HEX_APOS, JSON_HEX_QUOT,
    JSON_HEX_TAG | JSON_HEX_AMP | JSON_HEX_APOS | JSON_HEX_QUOT,
    JSON_FORCE_OBJECT, JSON_NUMERIC_CHECK, JSON_UNESCAPED_SLASHES, JSON_PRETTY_PRINT,
    JSON_UNESCAPED_UNICODE, JSON_PARTIAL_OUTPUT_ON_ERROR, JSON_PRESERVE_ZERO_FRACTION,
    JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_LINE_TERMINATORS, JSON_UNESCAPED_LINE_TERMINATORS,
    JSON_INVALID_UTF8_IGNORE, JSON_INVALID_UTF8_SUBSTITUTE,
    JSON_INVALID_UTF8_SUBSTITUTE | JSON_UNESCAPED_UNICODE,
    JSON_INVALID_UTF8_IGNORE | JSON_INVALID_UTF8_SUBSTITUTE,
    JSON_PRETTY_PRINT | JSON_FORCE_OBJECT, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE,
    JSON_THROW_ON_ERROR, JSON_THROW_ON_ERROR | JSON_PARTIAL_OUTPUT_ON_ERROR,
    JSON_NUMERIC_CHECK | JSON_PRESERVE_ZERO_FRACTION, JSON_NUMERIC_CHECK | JSON_FORCE_OBJECT,
    JSON_PARTIAL_OUTPUT_ON_ERROR | JSON_PRETTY_PRINT,
];

$dyn = new WithDyn; $dyn->second = 's'; $dyn->{'0'} = 'zero'; $dyn->{''} = 'empty';
$std = new stdClass; $std->a = 1; $std->{'x y'} = [true]; $std->{'12'} = 'num';
$nested = (object)['a' => (object)['b' => (object)[]], 'c' => [], 'd' => [[]], 'e' => [(object)[]]];

$values = [
    null, true, false, 0, 1, -1, PHP_INT_MAX, PHP_INT_MIN,
    0.0, -0.0, 1.0, 1.5, -1.5, 0.1, 0.1 + 0.2, 1e15, 1e16, 1e17, 1e22, 1.0e25, 1e-5, 1e-7, 123456789.125,
    -1.25e-10, 2.5e300, 5e-324, PHP_FLOAT_MAX, PHP_FLOAT_EPSILON, 100.0, 3.0,
    NAN, INF, -INF,
    "", "a", "abc", "with \"quotes\" and \\backslash\\ and /slash/",
    "<tag> & 'apos' \"quot\"", "\x00\x01\x02\x1f\x7f", "\x08\x0c\n\r\t",
    "é", "€", "😀", "\u{2028}\u{2029}", "\u{FFFF}", "\u{10FFFF}", "日本語テキスト",
    "\xff", "a\xffb", "\xc3", "\xc3\x28", "\xe2\x82", "\xe2\x82\x28", "\xf0\x9f\x98", "\xf0\x28\x8c\x28",
    "\xed\xa0\x80", "\xe0\x80\x80", "\xc0\xaf", "\xf4\x90\x80\x80", "\xf5\x80\x80\x80", "\x80\x80",
    "ok\xe2\x82\xacok\xff\xfeend", "\xe2\x28\xa1",
    "123", "-123", "1.5", "1e3", " 12", "12 ", "0x1A", "1e1000", "-1e1000", ".5", "5.", "0123", "+1", "1_000",
    "9223372036854775807", "9223372036854775808", "-9223372036854775809", " ", "abc123", "1.0", "-0", "0.0",
    [], [1, 2, 3], [1 => 'a', 2 => 'b'], ['a' => 1, 'b' => 2], [3 => 3, 1 => 1], [0 => 'a', 2 => 'c'],
    ['' => 'empty key'], ["\0abc" => 'nul key'], ["\xff" => 1, 'next' => 2], ['k/' => '/v'],
    ['a' => [], 'b' => new stdClass, 'c' => [[]], 'd' => [new stdClass]],
    [[[[1]]]], [1, [2, [3, [4, []]]]], ['x' => ['y' => ['z' => []]]],
    [1.0, 2.0, 0.5], [NAN, 1], ['a' => INF, 'b' => 2, 'c' => "\xff", 'd' => 4],
    ["\xff" => 'bad key'], ["ok", "\xff", "ok2"],
    ['12' => 'a', '13' => 'b'], [-5 => 'neg'], [PHP_INT_MAX => 'max'],
    ["1.5" => 'x', "1e3" => 'y'],
    new stdClass, $std, $dyn, new Pub, new OnlyHidden, new Typed, $nested,
    (object)['0' => 'a', '1' => 'b'], (object)[1, 2],
    ['pub' => new Pub, 'list' => [new OnlyHidden]],
];

foreach ($values as $v) {
    foreach ($all_encode_flags as $f) {
        t(fn() => __je($v, $f));
    }
}

// Depth handling.
$deep = [[[1]], ['a' => ['b' => []]], (object)['a' => (object)['b' => 1]], [[[]]], [], (object)[], 1];
foreach ($deep as $v) {
    foreach ([-1, 0, 1, 2, 3, 4] as $d) {
        foreach ([0, JSON_PARTIAL_OUTPUT_ON_ERROR, JSON_PRETTY_PRINT | JSON_PARTIAL_OUTPUT_ON_ERROR, JSON_THROW_ON_ERROR] as $f) {
            t(fn() => __je($v, $f, $d));
        }
    }
}
$deeparr = 1;
for ($i = 0; $i < 600; $i++) $deeparr = [$deeparr];
t(fn() => __je($deeparr));
t(fn() => __je($deeparr, 0, 601));
t(fn() => __je($deeparr, JSON_PARTIAL_OUTPUT_ON_ERROR));

// Float precision.
$floats = [0.1, 1.5, 2.5, 1/3, 100.0, 1e25, 123456.0, 0.000012345, -0.0, 1e15, 12345678901234567890.0, 3.0];
foreach ([-1, 0, 1, 2, 5, 14, 17, 20, 40] as $p) {
    ini_set('serialize_precision', $p);
    foreach ($floats as $x) {
        t(fn() => __je($x));
        t(fn() => __je($x, JSON_PRESERVE_ZERO_FRACTION));
        t(fn() => __je((string)$x, JSON_NUMERIC_CHECK));
    }
    t(fn() => __jd('[0.1,1.5,1e25,-0.0,123456789012345678901234567890]'));
}
ini_set('serialize_precision', -1);

// Decoding.
$docs = [
    '1', '-1', '0', '-0', '0.0', '-0.0', '1.5', '-1.5e3', '1E2', '1e-2', '1.0', '1e400', '-1e400',
    '9223372036854775807', '9223372036854775808', '-9223372036854775808', '-9223372036854775809',
    '12345678901234567890', '123456789012345678901234567890', '1.5e', '01', '00', '-', '-a', '+1', '.5', '5.',
    '0x1', 'Infinity', 'NaN', 'true', 'false', 'null', 'True', 'NULL', 'nul', 'tru', 'nullx', ' true ',
    '""', '"abc"', '"a\\"b"', '"\\\\"', '"\\/"', '"\\b\\f\\n\\r\\t"', '"\\u0041"', '"\\u00e9"', '"\\u20ac"',
    '"\\ud83d\\ude00"', '"\\uD83D\\uDE00"', '"\\ud83d"', '"\\ude00"', '"\\ud83dx"', '"\\ud83d\\u0041"',
    '"\\ud83d\\ud83d"', '"\\u0000"', '"a\\u0000b"', '"\\u12"', '"\\u12G4"', '"\\x"', '"\\\'"', '"\\U0041"',
    '"\\"', '"abc', '"', "\"a\nb\"", "\"a\tb\"", "\"\x00\"", "\"\x1f\"", "\"\x7f\"", "\"é\"", "\"€😀\"",
    "\"\xff\"", "\"a\xffb\"", "\"\xc3\"", "\"\xe2\x82\"", "\"\xed\xa0\x80\"", "\"\xf4\x90\x80\x80\"",
    "\"\xc0\xaf\"", "\"ok\xe2\x82\xacok\xff\xfeend\"", "[\"\xff\"]", "{\"\xff\":1}",
    "\xff", "é", "\x7f", "\x00", "[1]\x00", "[1] \x00", "\x01", "[\x01]", "\t[1]\r\n", " ", "\n", "\r\n1\r\n",
    '[]', '{}', '[1,2,3]', '[1,[2,[3]]]', '{"a":1}', '{"a":{"b":{"c":[]}}}', '{"a":1,"a":2}',
    '{"a":1,"b":2,"a":3}', '{"":1}', '{"":1,"_empty_":2}', '{"0":1,"1":2}', '{"1":1,"0":0}',
    '{"-1":1}', '{"01":1}', '{"1.5":1}', '{"9223372036854775808":1}', '{"9223372036854775807":1}',
    '{"\\u0000":1}', '{"\\u0000a":1}', '{"a\\u0000":1}', '{"a":{"\\u0000b":2}}', '[{"\\u0000":1}]',
    '{"\\u0000":{"x":1}}', '[[[[]]]]', '[{}]', '{"a":[{}]}', '[{"a":{}},{}]', '{"a":{},"b":{}}',
    '[', ']', '{', '}', '[1', '[1,', '[1,]', '[,1]', '[1 2]', '[1}', '{]', '{"a":1]', '{"a"}', '{"a":}',
    '{"a" 1}', '{a:1}', '{"a":1,}', '{,}', '{"a":1 "b":2}', '{1:1}', '[1]]', '[1][2]', '1 2', '"a" "b"',
    '[}', '{"a":[}', '[{]', '[{"a":1]', '{"a":[1}', '{"a":{"b":1]}', '[[]]]', '[[1,2]]}',
    "[\n1,\n2\n", "{\n\"a\":\n}", "\n\n  [1, 2,\n   3, x]",
    '{"a":1}garbage', '[1] [2]', '[1]   ', '   [1]', '[1,2,3,]', '[null,true,false]', '[nul]',
    '{"a":true,"b":false,"c":null,"d":1,"e":1.5,"f":"s","g":[],"h":{}}',
    '[1.0, -0, -0.0, 0e0, 0E+1, 1E-1, 12.5e-3]',
    '"\\u00e9\\u00E9"', '"\\u0080"', '"\\u07ff"', '"\\u0800"', '"\\uffff"', '"\\ud7ff"', '"\\ue000"',
    '"\\udbff\\udfff"', '"\\ud800\\udc00"', '"\\uDBFF\\uDFFF"', '"\\ud800\\uDC00x"',
];
$decode_flags = [0, JSON_OBJECT_AS_ARRAY, JSON_BIGINT_AS_STRING, JSON_INVALID_UTF8_IGNORE,
    JSON_INVALID_UTF8_SUBSTITUTE, JSON_INVALID_UTF8_IGNORE | JSON_INVALID_UTF8_SUBSTITUTE,
    JSON_THROW_ON_ERROR, JSON_OBJECT_AS_ARRAY | JSON_BIGINT_AS_STRING];
foreach ($docs as $doc) {
    foreach ($decode_flags as $f) {
        foreach ([null, true, false] as $assoc) {
            t(fn() => __jd($doc, $assoc, 512, $f));
        }
    }
    foreach ([1, 2, 3] as $d) {
        t(fn() => __jd($doc, true, $d));
        t(fn() => __jv($doc, $d));
    }
    t(fn() => __jv($doc));
    t(fn() => __jv($doc, 512, JSON_INVALID_UTF8_IGNORE));
}
// Errors keep/clear the last error.
t(fn() => __jd(''));
t(fn() => __jd('', true, 512, JSON_THROW_ON_ERROR));
t(fn() => __jd('[', true, 0));
t(fn() => __jd('[', true, -1));
t(fn() => __jd('[1]', true, PHP_INT_MAX));
t(fn() => __jv(''));
t(fn() => __jv('[1]', 0));
t(fn() => __jv('[1]', 512, JSON_OBJECT_AS_ARRAY));
t(fn() => __jv('[1]', 512, JSON_INVALID_UTF8_SUBSTITUTE));

// Depth limits on deeply nested documents.
foreach ([1, 2, 511, 512, 513] as $n) {
    $a = str_repeat('[', $n) . str_repeat(']', $n);
    $o = str_repeat('{"a":', $n) . '1' . str_repeat('}', $n);
    foreach ([$n - 1, $n, $n + 1, 512] as $d) {
        if ($d < 1) continue;
        t(fn() => __jd($a, true, $d));
        t(fn() => __jd($o, false, $d));
        t(fn() => __jv($a, $d));
    }
}
