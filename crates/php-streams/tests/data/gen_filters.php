<?php
// Generates filters_oracle.txt: bundled stream filters run on a write chain
// of php://temp. Each data piece is one fwrite(); stream_filter_remove()
// performs the closing flush.
//
// Line format (tab separated):
//   filter  params  pieces  =>  output  warnings
// params: "-" (none), "!<hex>" (a non-array string parameter) or a
// comma-separated list of key:type:hexvalue with type i (int), s (string),
// b (bool). pieces: comma-separated hex strings ("" = empty piece).
// output: hex, or "FAIL" when the filter could not be created.
// warnings: "|"-separated messages without the "function(): " prefix.
//
// Run: php gen_filters.php > filters_oracle.txt

$warnings = [];
set_error_handler(function ($no, $str) use (&$warnings) {
    $warnings[] = preg_replace('/^[a-z_]+\(\): /', '', $str);
    return true;
});

function enc_params($p) {
    if ($p === null) return '-';
    if (!is_array($p)) return '!' . bin2hex((string)$p);
    $out = [];
    foreach ($p as $k => $v) {
        if (is_bool($v)) $out[] = "$k:b:" . bin2hex($v ? '1' : '0');
        elseif (is_int($v)) $out[] = "$k:i:" . bin2hex((string)$v);
        else $out[] = "$k:s:" . bin2hex($v);
    }
    return implode(',', $out);
}

function split_pieces($s, $n) {
    if ($n === 0 || $s === '') return [$s];
    return str_split($s, $n);
}

function run_case($filter, $params, $pieces) {
    global $warnings;
    $warnings = [];
    $fp = fopen('php://temp', 'w+');
    $f = $params === null
        ? stream_filter_append($fp, $filter, STREAM_FILTER_WRITE)
        : stream_filter_append($fp, $filter, STREAM_FILTER_WRITE, $params);
    if ($f === false) {
        $out = 'FAIL';
    } else {
        foreach ($pieces as $p) {
            fwrite($fp, $p);
        }
        stream_filter_remove($f);
        rewind($fp);
        $out = bin2hex(stream_get_contents($fp));
    }
    fclose($fp);
    echo $filter, "\t", enc_params($params), "\t",
        implode(',', array_map('bin2hex', $pieces)), "\t=>\t", $out, "\t",
        implode('|', $warnings), "\n";
}

$bin100 = '';
for ($i = 0; $i < 100; $i++) $bin100 .= chr(($i * 37) & 0xff);
$text = "The quick brown fox jumps over the lazy dog. Pack my box with five dozen liquor jugs!";

$cases = [];

// base64-encode
$b64e_inputs = ['', 'a', 'ab', 'abc', 'abcd', 'Hello, World!', "\x00\xff\xfe", $bin100, $text];
$b64e_params = [
    null,
    [],
    ['line-length' => 8],
    ['line-length' => 8, 'line-break-chars' => "\n"],
    ['line-length' => 3],
    ['line-length' => 4, 'line-break-chars' => "|"],
    ['line-length' => 10, 'line-break-chars' => ''],
    ['line-length' => '20'],
    ['line-length' => -5],
    ['line-break-chars' => "\n"],
    ['line-length' => 76, 'line-break-chars' => "\r\n"],
];
foreach ($b64e_params as $p) {
    foreach ($b64e_inputs as $in) {
        $cases[] = ['convert.base64-encode', $p, $in];
    }
}

// base64-decode
$b64d_inputs = [
    'SGVsbG8=', "SGVs bG8=\n", 'SGVsbG8', 'QQ==', 'QQ', 'Q===', 'Q', 'SGV$sbG8=',
    'QQ==QQ==', "YWJj\r\nZGVm", '=', 'Zm9v=YmFy', 'Zm9vYmFy', 'Zm9vYg==', 'Zm9vYmE=',
    base64_encode($bin100), chunk_split(base64_encode($text), 10, "\r\n"), '====', 'QQ=x', 'QQ= =',
];
foreach ($b64d_inputs as $in) {
    $cases[] = ['convert.base64-decode', null, $in];
}
$cases[] = ['CONVERT.BASE64-DECODE', null, 'SGVsbG8='];
$cases[] = ['convert.Base64-Decode', null, 'SGVsbG8='];

// quoted-printable-encode
$qpe_inputs = [
    'Hello World', "a  \r\nb", "tab\t\r\nx", 'line=equals', "\x00\x01\xff", $text,
    'ends with space ', ".dot", "a\rb\nc", str_repeat('x', 80), "abc \r\n def\t\r\n", "\r\n\r\n",
    "trail  \r", "\r", " ", "a \tb", str_repeat("ab ", 40), "x\r\ny\r\n", str_repeat('=', 30),
    "caf\xc3\xa9 \xe9t\xe9",
];
$qpe_params = [
    null,
    ['line-length' => 10],
    ['line-length' => 10, 'line-break-chars' => "\n"],
    ['line-length' => 20, 'line-break-chars' => "\r\n"],
    ['binary' => true],
    ['binary' => true, 'line-length' => 10],
    ['force-encode-first' => true],
    ['force-encode-first' => true, 'line-length' => 10],
    ['line-length' => 4],
    ['line-length' => 3, 'binary' => false],
    ['line-break-chars' => "\n"],
];
foreach ($qpe_params as $p) {
    foreach ($qpe_inputs as $in) {
        $cases[] = ['convert.quoted-printable-encode', $p, $in];
    }
}

// quoted-printable-decode
$qpd_inputs = [
    'a=3Db', "soft=\r\nbreak", "soft=\nbreak", "soft=\rbreak", "x=  \r\ny", 'bad=ZZ', 'bad=4',
    'trailing=', 'lower=3d', '=41=42=43', "a=\r\n", "=\r\n=\n", "x= \ty", "x=\r\r\ny", "plain text\r\n",
    quoted_printable_encode($text . "\r\n" . $bin100), "a=\n", "=4", "=\r", "x=\t\r\nz",
];
$qpd_params = [
    null,
    ['line-break-chars' => "\r\n"],
    ['line-break-chars' => "\n"],
    ['line-break-chars' => '|'],
];
foreach ($qpd_params as $p) {
    foreach ($qpd_inputs as $in) {
        $cases[] = ['convert.quoted-printable-decode', $p, $in];
    }
}

// invalid / unknown
$cases[] = ['convert.base64-encode', 'notarray', 'abc'];
$cases[] = ['convert.base64-decode', 5, 'abc'];
$cases[] = ['convert.unknown', null, 'abc'];
$cases[] = ['convert.base64-encode', ['write_seek_mode' => 'bogus'], 'abc'];
$cases[] = ['convert.base64-encode', ['write_seek_mode' => 'reset'], 'abc'];
$cases[] = ['convert.quoted-printable-encode', ['line-length' => 10, 'write_seek_mode' => 'strict'], str_repeat('y', 25)];

// dechunk
$dechunk_inputs = [
    "5\r\nhello\r\n0\r\n\r\n", "5;ext=1\r\nhello\r\n6\r\n world\r\n0\r\nT: x\r\n\r\n",
    "a\r\n0123456789\r\n0\r\n", "3\nabc\n0\n", "zz", "2\r\nabXY", "A\r\n0123456789\r\n0\r\n",
    "ffffffffffffffffff\r\nx", "1\r\na\r\n1\r\nb\r\n", "0\r\n\r\nafter", "4 \r\nabcd\r\n0\r\n\r\n",
    "\r\n", "",
];
foreach ($dechunk_inputs as $in) {
    $cases[] = ['dechunk', null, $in];
}

// string filters
foreach (['string.rot13', 'string.toupper', 'string.tolower'] as $f) {
    $cases[] = [$f, null, "Hello World 123 \xe9\xc9 xyzXYZ"];
    $cases[] = [$f, null, ''];
}
$cases[] = ['STRING.ROT13', null, 'abc'];
$cases[] = ['string.nope', null, 'abc'];
$cases[] = ['nofilter', null, 'abc'];

foreach ($cases as [$filter, $params, $in]) {
    $splits = [0];
    if (strlen($in) > 1) {
        $splits = [0, 1, 2, 5];
    }
    foreach ($splits as $n) {
        run_case($filter, $params, split_pieces($in, $n));
    }
}
