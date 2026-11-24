<?php
// Oracle generator for the session serializers.
// Run: /usr/bin/php8.5 -d memory_limit=1G -d session.save_path=/tmp -d session.use_strict_mode=0 \
//      gen_serializers.php > serializers.tsv
// Each line: name \t hex(serialize($shape)) \t hex(php)|FALSE \t hex(php_binary) \t warnings(;-joined)
// The php_serialize encoding equals serialize($shape).

class Point { public $x = 1; protected $y = 2; private $z = 3; }
class WithSer { public $a; function __construct($a = 0) { $this->a = $a; }
    function __serialize(): array { return ['k' => $this->a]; }
    function __unserialize(array $d): void { $this->a = $d['k']; } }

$shapes = [];
$scalars = [
    'null' => null, 'true' => true, 'false' => false, 'zero' => 0, 'one' => 1, 'neg' => -42,
    'max' => PHP_INT_MAX, 'min' => PHP_INT_MIN, 'f0' => 0.0, 'fneg0' => -0.0, 'f01' => 0.1,
    'fbig' => 1e100, 'fsmall' => 1.5e-300, 'finf' => INF, 'fninf' => -INF, 'fnan' => NAN,
    'f13' => 1/3, 'fint' => 3.0, 's_empty' => '', 's_abc' => 'abc', 's_quote' => 'a"b;c',
    's_pipe' => 'x|y', 's_nl' => "line\nbreak", 's_bin' => "\0\x01\xff", 's_utf8' => "h\u{e9}llo \u{1F600}",
    's_num' => '123', 's_long' => str_repeat('z', 300),
];
foreach ($scalars as $k => $v) { $shapes["scalar_$k"] = ['v' => $v]; }
$shapes['multi_scalars'] = $scalars;
$shapes['empty'] = [];
$shapes['nested'] = ['a' => [1, 2, [3, ['deep' => true]]], 'b' => ['x' => 'y']];
$shapes['list'] = ['l' => range(0, 20)];
$shapes['assoc_mixed_keys'] = ['m' => [5 => 'a', 'k' => 'b', -3 => 'c', '07' => 'd']];
$shapes['numeric_top'] = [0 => 'skip', 'keep' => 1, 1 => 'skip2'];
$shapes['numeric_only'] = [1 => 'a', 2 => 'b'];
$shapes['pipe_key'] = ['ok' => 1, 'bad|key' => 2, 'after' => 3];
$shapes['pipe_key_first'] = ['a|b' => 1];
$shapes['long_key'] = [str_repeat('k', 127) => 1, str_repeat('L', 128) => 2, 'short' => 3];
$shapes['key_empty'] = ['' => 'empty key'];
$shapes['key_spaces'] = ['with space' => 1, "tab\tkey" => 2, 'semi;colon' => 3, 'quote"key' => 4];
$shapes['key_bin'] = ["nul\0key" => 1, "\xff" => 2];
$a = 1; $shapes['ref_pair'] = ['x' => &$a, 'y' => &$a];
unset($a);
$b = [1, 2]; $shapes['ref_array'] = ['p' => &$b, 'q' => &$b, 'r' => 5];
unset($b);
$c = 'v'; $shapes['ref_nested'] = ['outer' => ['in' => &$c], 'top' => &$c];
unset($c);
$d = 7; $shapes['ref_three'] = ['n' => 0, 'a' => &$d, 'b' => 'mid', 'c' => &$d, 'e' => &$d];
unset($d);
$o = new stdClass; $o->p = 1;
$shapes['obj'] = ['o' => $o];
$shapes['obj_shared'] = ['o1' => $o, 'o2' => $o, 'x' => [$o]];
$shapes['obj_numeric_first'] = [3 => $o, 'x' => $o, 'y' => $o];
$o2 = new stdClass; $o2->self = $o2;
$shapes['obj_self'] = ['s' => $o2];
$shapes['point'] = ['pt' => new Point];
$shapes['withser'] = ['w' => new WithSer(9), 'w2' => new WithSer([1, 2])];
$e = new stdClass; $e->list = [1, 2]; $e->nested = new stdClass; $e->nested->q = 'z';
$shapes['obj_deep'] = ['e' => $e, 'again' => $e->nested];
$ro = new stdClass; $shapes['ref_obj'] = ['a' => &$ro, 'b' => &$ro, 'c' => $ro];
unset($ro);
// Generated combinations.
$vals = [0, 1, -1, 2.5, 'str', '', true, null, [], [1], ['k' => 'v']];
$i = 0;
foreach ($vals as $v1) {
    foreach ($vals as $v2) {
        $shapes['combo_' . $i++] = ['first' => $v1, 'second' => [$v2, 'x' => $v1], 'third' => $v2];
    }
}
for ($n = 0; $n < 40; $n++) {
    $arr = [];
    for ($j = 0; $j <= $n % 7; $j++) {
        $arr["k{$n}_$j"] = ($j % 3 == 0) ? $j * $n : (($j % 3 == 1) ? str_repeat(chr(65 + $j), $n % 5) : [$j => $n]);
    }
    $shapes["gen_$n"] = $arr;
}
for ($n = 0; $n < 20; $n++) {
    $r = $n; $arr = ['base' => 1];
    for ($j = 0; $j < $n % 5 + 1; $j++) { $arr["r$j"] = &$r; $arr["v$j"] = [$j, &$r]; }
    $shapes["genref_$n"] = $arr;
    unset($r, $arr);
}

$out = [];
$warnings = [];
set_error_handler(function ($no, $str) use (&$warnings) { $warnings[] = $str; return true; });
foreach ($shapes as $name => $shape) {
    $row = [$name, bin2hex(serialize($shape))];
    foreach (['php', 'php_binary'] as $handler) {
        $warnings = [];
        ini_set('session.serialize_handler', $handler);
        session_id('fixture');
        session_start();
        $_SESSION = $shape;
        $enc = session_encode();
        $row[] = $enc === false ? 'FALSE' : bin2hex($enc);
        $_SESSION = [];
        session_abort();
        $row[] = implode(';', $warnings);
    }
    $out[] = implode("\t", $row);
}
// Print only at the end: output would mark the headers as sent.
echo implode("\n", $out), "\n";
