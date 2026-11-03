<?php
$patterns = ['*', '?', '*.txt', 'a*b', 'a?c', '[abc]', '[!abc]', '[^abc]', '[a-c]x', '[]]', '[]a]', '[!]]', '[a-]', '[-a]', '[a-', '[', 'a[', '[a', '[\\]]', '[\\', '\\', 'a\\', '\\*', '\\?', '\\[', '\\a', 'a\\b',
 '[[:alpha:]]', '[[:digit:]]*', '[[:upper:]]', '[[:lower:]]', '[[:space:]]', '[[:punct:]]', '[[:alnum:]_]*', '[[:xdigit:]]', '[[:bogus:]]', '[[:alpha:]', '[[:alpha]', '[[:]]', '[[.a.]]', '[[.-.]]', '[[=a=]]', '[[.ab.]]',
 '*/*', '*/', '/*', 'a/*', '*a*', '**', '*?', '?*', '*.*', '.*', '*/.*', 'a/.b', 'a/?b', 'a/[.]b', '*b/c', 'a*/c', '[A-C]', '[a-c]', 'ABC', '[!/]', '[/]', 'a[/]b', '*[/]*', '*\\/*', '\\.*', '[.]*', '?a', '*c', 'x*y*z', '[z-a]', '[\\]-\\^]', '[a\\-c]', '*[', 'a*[', '[*]', '[?]', '{a,b}', 'a**b', '*.[ch]', '[!a-z]*', '[a-c]*[!x]', '*a', '[[:alpha:][:digit:]]', '[a[:digit:]]', '[!a]?', '*\\', '[[:alpha:]-z]', '[a-[:digit:]]'];
$strings = ['', 'a', 'b', 'abc', 'aXb', 'ab', 'acb', 'file.txt', '.txt', '.hidden', 'a/b', 'a/.b', 'a/xb', '/x', 'x/', 'a/b/c', 'ab/c', ']', '[', '\\', '-', '*', '?', 'A', 'B', 'z', '7', ' ', '_x', 'a\\b', 'x.c', 'x.h', 'xyz', 'xaybz', '{a,b}', 'a[', '.', '..', 'aa', "\xe9", 'C'];
$flags = [0, FNM_PATHNAME, FNM_NOESCAPE, FNM_PERIOD, FNM_PATHNAME|FNM_PERIOD, FNM_CASEFOLD];
foreach ($patterns as $p) foreach ($strings as $s) foreach ($flags as $f) {
  if ($f != 0 && (crc32("$p|$s|$f") % 5) < 3) continue;
  echo bin2hex($p), "\t", bin2hex($s), "\t", $f, "\t", fnmatch($p, $s, $f) ? 1 : 0, "\n";
}
