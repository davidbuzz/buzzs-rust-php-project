<?php
$modes = [FILEINFO_NONE, FILEINFO_MIME, FILEINFO_MIME_TYPE, FILEINFO_MIME_ENCODING, FILEINFO_EXTENSION, FILEINFO_APPLE];
$f = finfo_open();
foreach (array_slice($argv, 1) as $p) {
  $data = @file_get_contents($p);
  foreach ($modes as $m) {
    echo "$p\t$m\tfile\t", finfo_file($f, $p, $m), "\n";
    echo "$p\t$m\tbuffer\t", finfo_buffer($f, $data, $m), "\n";
  }
}
