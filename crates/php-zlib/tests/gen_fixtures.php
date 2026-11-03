<?php
// Regenerates fixtures/oracle.txt from the C implementation (PHP + system zlib):
//   php crates/php-zlib/tests/gen_fixtures.php > crates/php-zlib/tests/fixtures/oracle.txt
// Line formats (tab separated, binary data hex encoded; outputs longer than
// 1500 bytes are written as H:<len>:<crc32b>; failures as W:<warning>):
//   input <idx> <data>
//   enc   <func> <level> <encoding|-> <input idx> <output>
//   dec   <func> <input> <outputs for max_length 0,1,5,100 (,)>
//         (dec and infl outputs longer than 32 bytes use the H: form)
//   defl  <encoding> <options-json> <chunk idxs(,)> <flushes(,)> <outputs(,)>
//   infl  <window> <dict-json> <flush> <outputs(,)> <status> <read_len>
//         (inflates the concatenated outputs of the preceding defl line in
//          61-byte pieces; the last piece uses ZLIB_FINISH unless <flush> is
//          ZLIB_FINISH, then ZLIB_SYNC_FLUSH)
mt_srand(42);
$rand = '';
for ($i = 0; $i < 700; $i++) $rand .= chr(mt_rand(0, 255));
$text = '';
for ($i = 0; $i < 600; $i++) $text .= $i . ' ';
$inputs = [
    '', 'a', 'hello', "hello world\n", str_repeat('a', 1000), str_repeat('abcdefgh', 300),
    "\0\1\2\3\xff\xfe", $rand, $text,
    "The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog.",
    'world', 'tail', 'b', 'c', 'd', substr($text, 0, 300), 'hello ', substr($rand, 0, 150),
];
foreach ($inputs as $i => $in) {
    echo "input\t$i\t", bin2hex($in), "\n";
}
function out($r, $limit = 1500) {
    if ($r === false) {
        $e = error_get_last();
        error_clear_last();
        return 'W:' . preg_replace('/^[a-z_]+\(\): /', '', $e['message']);
    }
    if (strlen($r) > $limit) {
        return 'H:' . strlen($r) . ':' . hash('crc32b', $r);
    }
    return bin2hex($r);
}
$encs = [ZLIB_ENCODING_RAW, ZLIB_ENCODING_GZIP, ZLIB_ENCODING_DEFLATE];
foreach (array_slice($inputs, 0, 10) as $idx => $in) {
    foreach (range(-1, 9) as $lvl) {
        foreach (['gzcompress', 'gzdeflate', 'gzencode'] as $f) {
            echo "enc\t$f\t$lvl\t-\t$idx\t", out(@$f($in, $lvl)), "\n";
        }
        foreach ($encs as $e) {
            echo "enc\tzlib_encode\t$lvl\t$e\t$idx\t", out(@zlib_encode($in, $e, $lvl)), "\n";
        }
    }
}
$dec_inputs = [];
foreach (array_slice($inputs, 0, 10) as $in) {
    foreach ($encs as $e) {
        $c = zlib_encode($in, $e);
        if (strlen($c) > 1200) continue;
        $dec_inputs[] = $c;
        $dec_inputs[] = substr($c, 0, (int) (strlen($c) / 2));
        $dec_inputs[] = $c . "trailing junk";
        if (strlen($c) > 4) { $d = $c; $d[(int) (strlen($d) / 2)] = chr(ord($d[(int) (strlen($d) / 2)]) ^ 0x55); $dec_inputs[] = $d; }
    }
}
$dec_inputs[] = '';
$dec_inputs[] = 'abc';
$dec_inputs[] = "\x1f\x8b";
$dec_inputs[] = "\x78\x9c";
$dec_inputs[] = gzdeflate(str_repeat("\0", 2000000));
$dec_inputs[] = gzencode("first") . gzencode("second");
foreach ($dec_inputs as $in) {
    foreach (['gzinflate', 'gzuncompress', 'gzdecode', 'zlib_decode'] as $f) {
        $res = [];
        foreach ([0, 1, 5, 100] as $max) {
            $res[] = out(@$f($in, $max), 32);
        }
        echo "dec\t$f\t", bin2hex($in), "\t", implode(',', $res), "\n";
    }
}
$flushes = [ZLIB_NO_FLUSH, ZLIB_PARTIAL_FLUSH, ZLIB_SYNC_FLUSH, ZLIB_FULL_FLUSH, ZLIB_BLOCK, ZLIB_FINISH];
$optsets = [[], ['level' => 9], ['level' => 0], ['level' => 1, 'memory' => 1], ['window' => 9],
    ['strategy' => ZLIB_FILTERED], ['strategy' => ZLIB_HUFFMAN_ONLY], ['strategy' => ZLIB_RLE], ['strategy' => ZLIB_FIXED],
    ['dictionary' => 'hello world'], ['dictionary' => ['ab', 'cd']]];
$chunksets = [[16, 10, 0], [15, 9, 0], [1, 12, 13, 14], [17, 11, 0]];
foreach ($encs as $e) {
    foreach ($optsets as $o) {
        foreach ($chunksets as $chunks) {
            foreach ($flushes as $fl) {
                $ctx = deflate_init($e, $o);
                $outs = [];
                $raw = [];
                $fls = [];
                foreach ($chunks as $i => $ci) {
                    $f = ($i == count($chunks) - 1) ? ZLIB_FINISH : $fl;
                    $fls[] = $f;
                    $r = deflate_add($ctx, $inputs[$ci], $f);
                    $raw[] = $r;
                    $outs[] = out($r);
                }
                echo "defl\t$e\t", json_encode($o), "\t", implode(',', $chunks), "\t", implode(',', $fls), "\t", implode(',', $outs), "\n";
                $all = implode('', $raw);
                $io = [];
                if (isset($o['window'])) $io['window'] = $o['window'];
                if (isset($o['dictionary'])) $io['dictionary'] = $o['dictionary'];
                $ictx = inflate_init($e, $io);
                $pieces = $all === '' ? [''] : str_split($all, 61);
                $iouts = [];
                foreach ($pieces as $i => $p) {
                    $f = ($i == count($pieces) - 1) ? ZLIB_FINISH : $fl;
                    if ($i == count($pieces) - 1 && $fl == ZLIB_FINISH) $f = ZLIB_SYNC_FLUSH;
                    $iouts[] = out(@inflate_add($ictx, $p, $f), 32);
                }
                echo "infl\t", $io['window'] ?? 15, "\t", json_encode($io['dictionary'] ?? null), "\t$fl\t", implode(',', $iouts), "\t", inflate_get_status($ictx), "\t", inflate_get_read_len($ictx), "\n";
            }
        }
    }
}
