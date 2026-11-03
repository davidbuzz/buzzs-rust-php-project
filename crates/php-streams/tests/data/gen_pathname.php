<?php
// Generates tests/data/pathname_oracle.txt: one case per line,
// fields are bin2hex-encoded and tab separated.
$paths = ["", "/", "//", "///", ".", "..", "a", "a/", "a//", "/a", "/a/", "//a", "a/b", "a/b/",
  "/a/b", "/a/b/", "/a/b//", "a//b", "a/./b", "../a", "./a", "/a/b/c.d", "c.d", "c.d.e", ".hidden",
  "/x/.hidden", "dir.ext/", "dir.ext/file", "file.", "/usr/local/lib/php.ini", "C:\\x\\y.z",
  "a\\b/c", "foo/bar.baz.qux", "  /  ", "/a\0b/c", "a/b\0.c", "/.", "/..", "a/..", "///x///y///",
  "x.tar.gz", "/tmp/", "~/file.txt", "http://example.com/a/b.html?x=1"];
$suffixes = ["", ".d", "c.d", ".", "b", "/", "x", "ini", "b.html?x=1"];
foreach ($paths as $p) {
  foreach ($suffixes as $s) {
    echo "B\t", bin2hex($p), "\t", bin2hex($s), "\t", bin2hex(basename($p, $s)), "\n";
  }
  for ($l = 1; $l <= 4; $l++) {
    echo "D\t", bin2hex($p), "\t", $l, "\t", bin2hex(dirname($p, $l)), "\n";
  }
  $i = pathinfo($p);
  $enc = function ($k) use ($i) { return array_key_exists($k, $i) ? bin2hex($i[$k]) : "-"; };
  echo "P\t", bin2hex($p), "\t", $enc("dirname"), "\t", $enc("basename"), "\t", $enc("extension"), "\t", $enc("filename"), "\n";
  foreach ([1, 2, 4, 8] as $f) {
    echo "F\t", bin2hex($p), "\t", $f, "\t", bin2hex(pathinfo($p, $f)), "\n";
  }
}
