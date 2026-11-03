<?php
// Fixture generator: crypt() over many password/salt shapes.
// Output: hex(password) SP hex(salt) SP hex(result)
$pws = ["", "a", "password", "rasmuslerdorf", "U*U*U*U*", "12345678", "123456789", "long password with spaces and more than seventy two characters in it to test truncation behaviour",
        "\xff\xa3" . "34" . "\xff\xff\xff\xa3" . "345", "\xd1\x91", "\x80\x81\x82", "a\0b", str_repeat("x", 72), str_repeat("x", 73), "\xff", "8b \xd0\xc1\xd2\xcf\xcc\xd8"];
$salts = [
  "", "a", "ab", "rl", "..", "./", "a:", "a\n", ":a", "a\x80", "\x80a", "*0", "*1", "*", "*2", "~a", "a~", "zz", "Az9.", "\x01\x02",
  "_J9..CCCC", "_J9..", "_........", "_/!......", "_/......!", "_K9..Salt", "_....abcd", "_zz..abcd", "_.z..abcd", "_J9..CCCCextra",
  "\$1\$", "\$1\$a", "\$1\$abcdefgh", "\$1\$abcdefghijk", "\$1\$abc\$def", "\$1\$\$", "\$1\$rasmusle\$", "\$1\$\xff\xfe",
  "\$5\$", "\$5\$saltstring", "\$5\$rounds=1000\$salt", "\$5\$rounds=999\$salt", "\$5\$rounds=\$salt", "\$5\$rounds=1000salt", "\$5\$rounds=+1000\$s", "\$5\$rounds= 1000\$s", "\$5\$rounds=01000\$s",
  "\$5\$rounds=1000000000\$s", "\$5\$rounds=-1\$s", "\$5\$toolongsaltstringtoolong", "\$5\$a\$b", "\$5\$rounds=1000\$", "\$5\$rounds=2000",
  "\$6\$", "\$6\$saltstring", "\$6\$rounds=1000\$salt", "\$6\$rounds=999\$salt", "\$6\$toolongsaltstringtoolong", "\$6\$rounds=1001\$0123456789abcdefgh",
  "\$2y\$04\$abcdefghijklmnopqrstuu", "\$2a\$04\$abcdefghijklmnopqrstuu", "\$2b\$04\$abcdefghijklmnopqrstuu", "\$2x\$04\$abcdefghijklmnopqrstuu",
  "\$2c\$04\$abcdefghijklmnopqrstuu", "\$2y\$03\$abcdefghijklmnopqrstuu", "\$2y\$32\$abcdefghijklmnopqrstuu", "\$2y\$4\$abcdefghijklmnopqrstuuu", "\$2y\$05\$abcdefghijklmnopqrstu",
  "\$2y\$05\$abcdefghijklmnopqrstuv", "\$2y\$05\$abcdefghijklmnopqrstu\$", "\$2y\$05\$abcdefghijklmnopqrst!u", "\$2y\$05\$usesomesillystringforsalt\$", "\$2y\$05\$.................................",
  "\$2\$05\$abcdefghijklmnopqrstuu", "\$2y\$05\$/////////////////////u", "\$2y\$05\$9999999999999999999999", "\$2y\$05\$abcdefghijklmnopqrstuu\$extra", "\$2y\$04\$", "\$2a\$05\$CCCCCCCCCCCCCCCCCCCCC.",
  "\$3\$", "\$7\$", "\$argon2i\$", "\$2y\$", "\$", "\$\$", str_repeat("a", 200), "\$1\$" . str_repeat("a", 200),
];
foreach ($salts as $s) {
  // The slow algorithms (MD5/SHA/bcrypt) get a smaller password sample.
  $sample = ($s !== '' && $s[0] === '$') ? [$pws[0], $pws[2], $pws[7], $pws[8], $pws[11], $pws[13]] : $pws;
  // Default-round SHA salts (5000 rounds) are slow in debug builds: two passwords.
  if (preg_match('/^\$[56]\$(?!rounds=)/', $s)) $sample = [$pws[0], $pws[7]];
  foreach ($sample as $p) {
    echo bin2hex($p), " ", bin2hex($s), " ", bin2hex(crypt($p, $s)), "\n";
  }
}
