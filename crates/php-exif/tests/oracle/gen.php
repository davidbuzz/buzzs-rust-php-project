<?php
// Regenerates tests/oracle/*.txt from the C implementation:
//   php -d memory_limit=1G tests/oracle/gen.php
// Every case is "#CASE <id>", zero or more "#DIAG <level> <hex message>",
// then "#RESULT <length>" followed by the raw serialize(result) ("FileDateTime" zeroed).
$root = dirname(__DIR__) . '/fixtures';
$files = [];
$it = new RecursiveIteratorIterator(new RecursiveDirectoryIterator($root, FilesystemIterator::SKIP_DOTS));
foreach ($it as $f) { $files[] = substr($f->getPathname(), strlen($root) + 1); }
sort($files, SORT_STRING);

$diags = [];
set_error_handler(function ($no, $str) use (&$diags) {
    $lvl = $no == E_WARNING ? 'W' : ($no == E_NOTICE ? 'N' : "E$no");
    $diags[] = "#DIAG $lvl " . bin2hex(preg_replace('/^[a-z_]+\([^)]*\): /', '', $str));
    return true;
});
function zero_time($r) {
    if (!is_array($r)) return $r;
    if (array_key_exists('FileDateTime', $r)) $r['FileDateTime'] = 0;
    if (isset($r['FILE']['FileDateTime'])) $r['FILE']['FileDateTime'] = 0;
    return $r;
}
function emit($out, $id, &$diags, $result) {
    fwrite($out, "#CASE $id\n");
    foreach ($diags as $d) fwrite($out, "$d\n");
    $s = serialize($result);
    fwrite($out, "#RESULT " . strlen($s) . "\n" . $s . "\n");
    $diags = [];
}

$out = fopen(__DIR__ . '/exif_read_data.txt', 'w');
$combos = [[null, false, false], [null, true, false], [null, false, true], [null, true, true],
           ['IFD0', false, false], ['EXIF,GPS COMMENT', true, false], ['THUMBNAIL', true, true]];
foreach ($files as $f) {
    foreach ($combos as [$sec, $arr, $thumb]) {
        $diags = [];
        $r = exif_read_data("$root/$f", $sec, $arr, $thumb);
        emit($out, "$f|" . var_export($sec, true) . "|" . (int)$arr . "|" . (int)$thumb, $diags, zero_time($r));
    }
}
fclose($out);

$out = fopen(__DIR__ . '/exif_thumbnail.txt', 'w');
foreach ($files as $f) {
    $diags = [];
    $w = $h = $t = null;
    $r = exif_thumbnail("$root/$f", $w, $h, $t);
    emit($out, "$f|whT", $diags, [$r, $w, $h, $t]);
    $r = exif_thumbnail("$root/$f");
    emit($out, "$f|plain", $diags, $r);
}
fclose($out);

$out = fopen(__DIR__ . '/imagetype.txt', 'w');
foreach ($files as $f) {
    $diags = [];
    $r = exif_imagetype("$root/$f");
    emit($out, "$f|exif_imagetype", $diags, $r);
    $info = null;
    $r = getimagesize("$root/$f", $info);
    emit($out, "$f|getimagesize", $diags, [$r, $info]);
    $r = getimagesizefromstring(file_get_contents("$root/$f"));
    emit($out, "$f|getimagesizefromstring", $diags, $r);
}
fclose($out);

$out = fopen(__DIR__ . '/tagname.txt', 'w');
for ($i = -2; $i <= 0x10001; $i++) {
    $r = exif_tagname($i);
    if ($r !== false) fwrite($out, "$i $r\n");
}
fclose($out);

$out = fopen(__DIR__ . '/iptc.txt', 'w');
$iptc = "\x1C\x02x\x00\x0ATest image\x1C\x02t\x00\x05(c)PH";
foreach ($files as $f) {
    if (filesize("$root/$f") > 60000) continue;
    $diags = [];
    $r = @iptcembed($iptc, "$root/$f");
    emit($out, "$f|iptcembed", $diags, $r);
    $r = iptcparse(is_string($r) ? $r : file_get_contents("$root/$f"));
    emit($out, "$f|iptcparse", $diags, $r);
}
fclose($out);
