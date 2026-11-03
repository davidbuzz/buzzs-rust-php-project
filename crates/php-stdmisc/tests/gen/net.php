<?php
// Fixture generator: deterministic network functions.
// Output: func hex(arg) hex(var_export(result))
function out($f, $a, $r) { echo $f, " ", bin2hex($a), " ", bin2hex(var_export($r, true)), "\n"; }
$ips = ["127.0.0.1", "0.0.0.0", "255.255.255.255", "256.1.1.1", "1.2.3", "1.2.3.4.5", "01.2.3.4", "1.2.3.04", " 1.2.3.4", "1.2.3.4 ", "1.2.3.-4", "a.b.c.d", "1..2.3", "0x7f.0.0.1",
  "::1", "::", "::ffff:1.2.3.4", "::1.2.3.4", "2001:db8::1", "2001:DB8:0:0:0:0:0:1", "1:2:3:4:5:6:7:8", "1:2:3:4:5:6:7:8:9", "1::2::3", "fe80::1%eth0", "::ffff:256.1.1.1", ":", "1.2.3.4:80", "", "x", "1:0:0:2:0:0:0:3", "0:0:1:0:0:0:0:0"];
foreach ($ips as $ip) {
  out("inet_pton", $ip, @inet_pton($ip));
  if ($ip !== "") out("ip2long", $ip, ip2long($ip));
}
$packed = ["", "a", "abc", "abcd", "\x7f\0\0\1", str_repeat("\0", 16), "\0\0\0\0\0\0\0\0\0\0\xff\xff\1\2\3\4", "\0\0\0\0\0\0\0\0\0\0\0\0\1\2\3\4", "\x20\x01\x0d\xb8" . str_repeat("\0", 11) . "\1", "0123456789abcdef", "0123456789abcde", str_repeat("\xff", 16), "\0\1\0\0\0\0\0\0\0\0\0\0\0\0\0\0", "\0\0\0\0\0\1\0\0\0\0\0\0\0\0\0\1"];
foreach ($packed as $p) out("inet_ntop", $p, inet_ntop($p));
foreach ([0, 1, -1, 2130706433, 4294967295, 4294967296, PHP_INT_MAX, PHP_INT_MIN, -2130706433, 3232235777] as $l) out("long2ip", (string)$l, long2ip($l));
foreach (["tcp", "udp", "icmp", "ip", "ipv6", "TCP", "bogus", ""] as $p) out("getprotobyname", $p, getprotobyname($p));
foreach ([0, 1, 6, 17, 41, 255, 9999, -1] as $n) out("getprotobynumber", (string)$n, getprotobynumber($n));
foreach ([["http", "tcp"], ["https", "tcp"], ["ssh", "tcp"], ["domain", "udp"], ["www", "tcp"], ["http", "udp"], ["bogus", "tcp"], ["http", ""], ["ftp", "tcp"]] as [$s, $p]) out("getservbyname", "$s/$p", getservbyname($s, $p));
foreach ([[80, "tcp"], [443, "tcp"], [22, "tcp"], [53, "udp"], [65535, "tcp"], [80 + 65536, "tcp"], [-1, "tcp"], [21, ""]] as [$n, $p]) out("getservbyport", "$n/$p", getservbyport($n, $p));
out("gethostbyname", "localhost", gethostbyname("localhost"));
out("gethostbyname", "127.0.0.1", gethostbyname("127.0.0.1"));
out("gethostbyname", "1.2.3", gethostbyname("1.2.3"));
out("gethostbynamel", "127.0.0.1", gethostbynamel("127.0.0.1"));
out("gethostbyaddr", "127.0.0.1", gethostbyaddr("127.0.0.1"));
out("gethostbyaddr", "::1", gethostbyaddr("::1"));
out("gethostbyaddr", "bogus", @gethostbyaddr("bogus"));
