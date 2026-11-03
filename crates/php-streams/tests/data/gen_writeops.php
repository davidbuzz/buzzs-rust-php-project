<?php
// Generates tests/data/writeops_oracle.txt: mode/kind/script results plus
// the final file contents.
$scripts = [
  "fwrite abc;ftell;fseek 0 0;fread 10;fwrite Z;ftell",
  "fread 2;fwrite XY;ftell;fseek 0 0;fread 100",
  "fseek 10 0;fwrite Q;ftell;fseek 0 0;fread 100",
  "fwrite 12;fseek -1 1;fwrite 3;fseek 0 2;ftell;ftruncate 2;ftell;fwrite W",
  "fgets;fwrite --;fgets;ftell",
];
$modes = ['r+', 'w', 'w+', 'a', 'a+', 'x+', 'c', 'c+'];
function enc($v) {
  if ($v === false) return 'F';
  if ($v === true) return 'T';
  if (is_int($v)) return 'I' . $v;
  return 'S' . bin2hex($v);
}
$dir = sys_get_temp_dir() . '/writeops-' . getmypid();
@mkdir($dir);
foreach (['file', 'memory'] as $kind) {
  foreach ($modes as $mode) {
    if ($kind == 'memory' && $mode[0] == 'x') continue;
    foreach ($scripts as $si => $script) {
      $path = "$dir/f";
      @unlink($path);
      if ($mode[0] != 'x') file_put_contents($path, "hello\nworld\n");
      if ($kind == 'file') {
        $f = fopen($path, $mode);
      } else {
        $f = fopen('php://memory', $mode);
        if ($mode[0] != 'w' && $mode[0] != 'c') { /* memory ignores existing content */ }
      }
      $out = [];
      set_error_handler(function ($no, $str) use (&$out) { $out[] = 'E' . bin2hex(preg_replace('/^\w+\(\): /', '', $str)); return true; });
      foreach (explode(';', $script) as $cmd) {
        $p = explode(' ', $cmd, 3);
        switch ($p[0]) {
          case 'fwrite': $out[] = enc(fwrite($f, $p[1])); break;
          case 'fread': $out[] = enc(fread($f, (int)$p[1])); break;
          case 'fgets': $out[] = enc(fgets($f)); break;
          case 'ftell': $out[] = enc(ftell($f)); break;
          case 'fseek': $out[] = enc(fseek($f, (int)$p[1], (int)$p[2])); break;
          case 'ftruncate': $out[] = enc(ftruncate($f, (int)$p[1])); break;
        }
      }
      restore_error_handler();
      fclose($f);
      $final = $kind == 'file' ? bin2hex(file_get_contents($path)) : '-';
      echo "$kind\t$mode\t$si\t", implode(',', $out), "\t$final\n";
    }
  }
}
@unlink("$dir/f"); @rmdir($dir);
