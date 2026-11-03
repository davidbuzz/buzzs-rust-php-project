<?php
// Generates tests/data/readops_oracle.txt. For each (content, kind, script)
// the sequence of read operations is run against a stream of that kind and
// every result is recorded. Kinds: file, memory, temp, filtered (file with a
// string.rot13 read filter), chunk3 (file with chunk size 3).
$contents = [
  "line1\nline2\nline3",
  "a\n\nb\r\nc\n",
  str_repeat("x", 9000) . "\n" . str_repeat("y", 100) . "\nend",
  "",
  "no newline at all",
  "\r\r\n\n",
];
$scripts = [
  "fgets;fgets;fgets;fgets;feof",
  "fgets 3;fgets 3;fgets 2;fgets 1;ftell;feof",
  "fread 4;fread 100;feof;fread 1;feof",
  "fgetc;fgetc;ftell;fseek 0 0;fgets;ftell",
  "sgl 100 \\n;sgl 3 ;sgl 100 zz;sgl 100 \\n;feof",
  "fseek 2 1;ftell;fgets;fseek -3 2;fread 10;feof;fseek 1 0;fgetc",
  "fread 8192;fread 8192;fgets;feof;ftell",
  "sgc -1 3;sgc 2 0;sgc -1 -1",
  "fgets 9002;fgets;fgets",
];
function enc($v) {
  if ($v === false) return 'F';
  if ($v === true) return 'T';
  if (is_int($v)) return 'I' . $v;
  return 'S' . bin2hex($v);
}
function arg($s) { return str_replace(['\n'], ["\n"], $s); }
$dir = sys_get_temp_dir() . '/readops-' . getmypid();
@mkdir($dir);
foreach ($contents as $ci => $content) {
  foreach (['file', 'memory', 'temp', 'filtered', 'chunk3'] as $kind) {
    foreach ($scripts as $si => $script) {
      $path = "$dir/f";
      file_put_contents($path, $content);
      switch ($kind) {
        case 'file': $f = fopen($path, 'r'); break;
        case 'memory': $f = fopen('php://memory', 'w+'); fwrite($f, $content); rewind($f); break;
        case 'temp': $f = fopen('php://temp/maxmemory:16', 'w+'); fwrite($f, $content); rewind($f); break;
        case 'filtered': $f = fopen($path, 'r'); stream_filter_append($f, 'string.rot13', STREAM_FILTER_READ); break;
        case 'chunk3': $f = fopen($path, 'r'); stream_set_chunk_size($f, 3); break;
      }
      $out = [];
      foreach (explode(';', $script) as $cmd) {
        $p = explode(' ', $cmd, 3);
        switch ($p[0]) {
          case 'fgets': $out[] = enc(isset($p[1]) ? fgets($f, (int)$p[1]) : fgets($f)); break;
          case 'fread': $out[] = enc(fread($f, (int)$p[1])); break;
          case 'fgetc': $out[] = enc(fgetc($f)); break;
          case 'feof': $out[] = enc(feof($f)); break;
          case 'ftell': $out[] = enc(ftell($f)); break;
          case 'fseek': $out[] = enc(fseek($f, (int)$p[1], (int)$p[2])); break;
          case 'sgl': $out[] = enc(stream_get_line($f, (int)$p[1], arg($p[2] ?? ''))); break;
          case 'sgc': $out[] = enc(stream_get_contents($f, (int)$p[1] < 0 ? null : (int)$p[1], (int)$p[2])); break;
        }
      }
      echo "$ci\t$kind\t$si\t", implode(',', $out), "\n";
      fclose($f);
    }
  }
}
@unlink("$dir/f"); @rmdir($dir);
