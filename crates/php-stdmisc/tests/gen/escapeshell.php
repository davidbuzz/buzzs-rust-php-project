<?php
// Fixture generator: escapeshellarg/escapeshellcmd in the C.UTF-8 (startup)
// and C LC_CTYPE. Output: locale hex(input) hex(arg) hex(cmd)
$inputs = ["", "a", "a b", "'", "''", "\"", "\"\"", "a'b", "a\"b", "'a'", "\"a\"", "'a\"b'", "\"a'b\"", "'a'b'", "\"a\"b\"", "'\"'\"", "\"'\"'",
  "#&;`|*?~<>^()[]{}\$\\", "\n", "\r\t", "\xff", "\x80", "\xc3\xa9", "\xc3", "a\xc3", "\xe2\x82\xac", "\xe2\x82", "\xf0\x9f\x98\x80", "\xf0\x9f\x98",
  "\xed\xa0\x80", "\xc0\xaf", "\xf4\x90\x80\x80", "caf\xc3\xa9 'x' \"y\" \$z", "%PATH%!", "ls -la; rm -rf /", "echo `id`", "a\\'b", "\x01\x02\x7f",
  "'\xc3\xa9'", "\"\xff\"", str_repeat("'", 5), str_repeat("\"", 3) . "'"];
foreach (["C.UTF-8", "C"] as $loc) {
  setlocale(LC_CTYPE, $loc);
  foreach ($inputs as $in) {
    echo $loc, " ", bin2hex($in), " ", bin2hex(escapeshellarg($in)), " ", bin2hex(escapeshellcmd($in)), "\n";
  }
}
