<?php
// Hand-written oracle cases for gen_oracle.php: every filter over
// representative inputs, flags and options, plus deterministic fuzzing of
// the parsers (int, float, ip, mac, domain, url, email).

function fv(...$a) { try { return __filter_rec('filter_var', ...$a); } catch (Throwable $e) { return null; } }
function fva(...$a) { try { return __filter_rec('filter_var_array', ...$a); } catch (Throwable $e) { return null; } }

class WithToString { public function __toString(): string { return "123"; } }
class WithToString2 { public function __toString(): string { return " yes "; } }
class NoToString {}

$scalars = [null, true, false, 0, 1, -1, 42, PHP_INT_MAX, PHP_INT_MIN, 1.5, -0.0, 1e20, 0.1, NAN, INF,
            new WithToString, new NoToString, new WithToString2];

$all_filters = [];
foreach (filter_list() as $name) $all_filters[] = filter_id($name);
$all_filters = array_values(array_unique(array_merge($all_filters, [0x0104, 0x0209, 0x0100, 0x0200])));

// --- int ---------------------------------------------------------------
$ints = ["", " ", "0", "00", "-0", "+0", "1", "-1", "+1", " 12 ", "\t12\n", "\x0b12\r", "\f12", "12\f",
    "12abc", "1.0", "1e3", "0x1A", "0X1a", "0x", "0xg", " 0x1F ", "0o17", "0O17", "0o", "0o8", "017", "08", "0b101",
    "9223372036854775807", "9223372036854775808", "-9223372036854775808", "-9223372036854775809",
    "0xffffffffffffffff", "0x1ffffffffffffffff", "0x7fffffffffffffff", "01777777777777777777777",
    "02000000000000000000000", "- 1", "1 2", "12\0", "\0", "+", "-", "+-1", "--1", "0-1", "123456789012345678901",
    "0000000000000000001", "1000000000000000000", "99999999999999999999", "-0x1", "0x-1", "0 ", " 0", "0\t", "00x1"];
foreach ($ints as $s) {
    foreach ([0, FILTER_FLAG_ALLOW_OCTAL, FILTER_FLAG_ALLOW_HEX, FILTER_FLAG_ALLOW_OCTAL | FILTER_FLAG_ALLOW_HEX, FILTER_NULL_ON_FAILURE] as $f) {
        fv($s, FILTER_VALIDATE_INT, $f);
    }
}
foreach ([["min_range" => 0, "max_range" => 10], ["min_range" => "5"], ["max_range" => 1.5], ["min_range" => "abc"],
          ["min_range" => -5, "max_range" => -1], ["default" => "dflt"], ["default" => 7, "min_range" => 100],
          ["min_range" => null], ["max_range" => true], ["min_range" => 10, "max_range" => 0]] as $o) {
    foreach (["0", "5", "10", "11", "-3", "abc", "", "1.5"] as $s) {
        fv($s, FILTER_VALIDATE_INT, ["options" => $o]);
        fv($s, FILTER_VALIDATE_INT, ["options" => $o, "flags" => FILTER_NULL_ON_FAILURE]);
    }
}
foreach ($scalars as $v) foreach ($all_filters as $f) fv($v, $f);

// --- bool --------------------------------------------------------------
foreach (["1", "0", "true", "TRUE", "True", "on", "On", "ON", "yes", "YES", "no", "NO", "off", "Off", "false", "FALSE",
          "", " ", " yes ", "\tno\n", "y", "n", "2", "-1", "truee", "\0", "1 ", "\f1", "on\0", "nO", "oFf", "enabled"] as $s) {
    foreach ([0, FILTER_NULL_ON_FAILURE] as $f) {
        fv($s, FILTER_VALIDATE_BOOL, $f);
        fv($s, FILTER_VALIDATE_BOOL, ["flags" => $f, "options" => ["default" => "D"]]);
    }
}

// --- float -------------------------------------------------------------
$floats = ["", " ", "1", "1.5", "-1.5", "+1.5", "1e5", "1E-5", "1e", "1e+", "1e-", ".5", "5.", ".", "-", "+", "-.5",
    "1,000", "1,000.5", "1.000,5", "1'000", "12,34", "1,0000", ",100", "1,,000", "1,000,000", "1 000", "1e309", "-1e309",
    "1e-400", "0.0", "0e5", "00.5", "0x1A", "1.5.5", " 1.5 ", "NAN", "INF", "1_000", "1000,000", "1,000e3", "1,000.5e-3",
    "1,00", "1,0", "-1,000", "+1,000", "1.5e3.5", "9223372036854775807", "9223372036854775808", "1e1000", "0.000",
    "0e0", "-0", "1.", "1.e5", ".e5", "12345678901234567890.5", "1\0", "1,\0" . "000", "١"];
foreach ($floats as $s) {
    foreach ([0, FILTER_FLAG_ALLOW_THOUSAND, FILTER_NULL_ON_FAILURE] as $f) fv($s, FILTER_VALIDATE_FLOAT, $f);
}
foreach ([["decimal" => ","], ["decimal" => ""], ["decimal" => "ab"], ["thousand" => " "], ["thousand" => ""],
          ["thousand" => "."], ["decimal" => ",", "thousand" => "."], ["min_range" => 0, "max_range" => 10],
          ["min_range" => "1.5"], ["max_range" => -1], ["decimal" => 5], ["thousand" => "ab"], ["decimal" => "e"],
          ["decimal" => " "], ["thousand" => "\0"]] as $o) {
    foreach (["1.5", "1,5", "1,000.5", "1.000,5", "1 000,5", "10", "11", "-2", "1e3", "1 ", "1ab000", "abc", "0.5"] as $s) {
        fv($s, FILTER_VALIDATE_FLOAT, ["options" => $o]);
        fv($s, FILTER_VALIDATE_FLOAT, ["options" => $o, "flags" => FILTER_FLAG_ALLOW_THOUSAND]);
    }
}

// --- regexp ------------------------------------------------------------
foreach (["/^a+$/", "/b/", "/^[0-9]+$/D", "invalid", "/(/", "~x~i"] as $re) {
    foreach (["aaa", "abc", "123", "x", "", "X"] as $s) {
        fv($s, FILTER_VALIDATE_REGEXP, ["options" => ["regexp" => $re]]);
    }
}
fv("a", FILTER_VALIDATE_REGEXP);
fv("a", FILTER_VALIDATE_REGEXP, ["options" => ["regexp" => 5]]);
fv("a", FILTER_VALIDATE_REGEXP, ["options" => []]);

// --- domain ------------------------------------------------------------
$domains = ["", ".", "a", "a.", "a..", "a..b", ".a", "-a", "a-", "a-.", "a-b", "a_b", "_a", "a.b-.c", "a.-b.c",
    "example.com", "example.com.", "localhost", "1.2.3.4", "a.b.c.d.e", str_repeat("a", 63), str_repeat("a", 64),
    str_repeat("a", 63) . ".com", str_repeat("a", 64) . ".com", str_repeat("a.", 126) . "a", str_repeat("a.", 127),
    str_repeat("ab.", 84) . "a", str_repeat("ab.", 85), "a b", "a\0", "\0a", "a.\0", "ex@mple.com", "ex%mple.com",
    "xn--80ak6aa92e.com", "1a", "a1", "A.B", "a-\0", "a.b.", "a.b..", "-", "--", "a--b", "a.-", "ä.com"];
foreach ($domains as $s) foreach ([0, FILTER_FLAG_HOSTNAME] as $f) fv($s, FILTER_VALIDATE_DOMAIN, $f);

// --- url ---------------------------------------------------------------
$urls = ["http://example.com", "https://example.com/", "HTTP://EXAMPLE.COM", "http://example.com/path?q=1#frag",
    "http://user:pass@example.com", "http://user@example.com", "http://us er@example.com", "http://user:p%41ss@example.com",
    "http://user:p%4@example.com", "http://user:p%zz@example.com", "http://us%2fer@example.com", "http://u{s}@example.com",
    "http://:@example.com", "http://@example.com", "http://example.com:80", "http://example.com:", "http://example.com:0",
    "http://example.com:65535", "http://example.com:65536", "http://example.com:123456", "http://example.com:qq",
    "http://[::1]", "http://[::1]:80/", "http://[::1", "http://[foo]", "http://[1.2.3.4]", "http://[::ffff:1.2.3.4]/x",
    "http://-example.com", "http://example-.com", "http://exa_mple.com", "http://ex..ample.com", "http://.example.com",
    "http://example.com.", "http://1.2.3.4", "http://256.256.256.256", "http://localhost", "http://a", "http://",
    "http:", "http:/", "http:///", "http:///path", "http//example.com", "http:/example.com", "ftp://ftp.example.com/x",
    "mailto:foo@bar.com", "MAILTO:foo@bar.com", "news:comp.lang.php", "file:///etc/passwd", "file://host/path",
    "file:/path", "javascript:alert(1)", "data:text/plain,hi", "foo://bar", "foo:bar", "//example.com/x", "/path",
    "path", "", "?q", "#f", "a.com:80", "a.com:80/x", "example.com", "http://example.com/ space", "http://exa mple.com",
    "http://example.com/\x7f", "http://example.com/\xc3\xa4", "http://ex\xc3\xa4mple.com", "http://example.com/?a[]=1",
    "http://example.com/#a#b", "http://example.com?x", "http://example.com#x", "https://a.b.c.d.e.f.g/h",
    "http://" . str_repeat("a", 64) . ".com", "http://" . str_repeat("a", 63) . ".com", "gopher://x", "http://x:80:90",
    "http://user:pass:word@example.com", "http://user@pass@example.com", "http://example.com:80@evil.com",
    "http://1.2.3.4:5/", "scheme+x-y.z://host", "1http://example.com", "h ttp://example.com", "http://example.com/a\"b",
    "http://example.com/<>", "http://example.com/{}", "http://example.com/|", "http://example.com/^", "http://example.com/`",
    "http://example.com/~", "http://example.com/\\", "http://example.com/%", "c:\\windows", "file:///c:/x", "x:1/y"];
foreach ($urls as $s) {
    foreach ([0, FILTER_FLAG_PATH_REQUIRED, FILTER_FLAG_QUERY_REQUIRED, FILTER_FLAG_PATH_REQUIRED | FILTER_FLAG_QUERY_REQUIRED] as $f) {
        fv($s, FILTER_VALIDATE_URL, $f);
    }
}
fv("http://example.com", FILTER_VALIDATE_URL, ["options" => ["uri_parser_class" => 5]]);

// --- email -------------------------------------------------------------
$emails = ["a@b.cd", "a@b.c", "a@b", "a.b@c.de", "a..b@c.de", ".a@c.de", "a.@c.de", "\"a\"@c.de", "\"a b\"@c.de",
    "\"a\\\"b\"@c.de", "\"a\\ b\"@c.de", "\"\"@c.de", "\"a\"b@c.de", "a\"b\"@c.de", "\"a\".b@c.de", "a.\"b\"@c.de",
    "a@[1.2.3.4]", "a@[1.2.3.256]", "a@[01.2.3.4]", "a@[1.2.3]", "a@[IPv6:::1]", "a@[ipv6:::1]", "a@[IPv6:1:2:3:4:5:6:7:8]",
    "a@[IPv6:1:2:3:4:5:6:7]", "a@[IPv6:1:2:3:4:5:6:7:8:9]", "a@[IPv6:1::2:3:4:5:6]", "a@[IPv6:1::2:3:4:5:6:7]",
    "a@[IPv6:1:2:3:4:5:6::]", "a@[IPv6:::1:2:3:4:5:6]", "a@[IPv6:::ffff:1.2.3.4]", "a@[IPv6:1:2:3:4:5:6:1.2.3.4]",
    "a@[IPv6:1:2:3:4:5:1.2.3.4]", "a@[IPv6:1:2::3:4:1.2.3.4]", "a@[IPv6:1:2::3:4:5:1.2.3.4]", "a@[IPv6:1::1.2.3.4]",
    "a@[IPv6:1:2:3:4::1.2.3.4]", "a@[IPv6:1:2:3:4:5::1.2.3.4]", "a@[IPv6:12345::1]", "a@[IPv6:g::1]", "a@[]",
    "a@example.com", "A@EXAMPLE.COM", "a+b@example.com", "a_b@example.com", "a-b@ex-ample.com", "a@ex--ample.com",
    "a@-example.com", "a@example-.com", "a@example.c0m", "a@example.0com", "a@example.xn--p1ai", "a@example.xn--",
    "a@xn--example.com", "a@1.2.3.4", "a@localhost", "a@a.b.c.d.e.f", "a@example.com.", "a@.example.com",
    "a@example..com", "a@@example.com", "@example.com", "a@", "", "a", "a@b.cd\n", "a\n@b.cd", "a b@c.de",
    "!#$%&'*+-/=?^_`{|}~@example.com", "a(b)@c.de", "a,b@c.de", "a;b@c.de", "a:b@c.de", "a<b>@c.de", "a[b]@c.de",
    "a\\b@c.de", "a\x7f@c.de", "\"a\x7f\"@c.de", "\"a\x00\"@c.de", "\"\\\x00\"@c.de", "\"a\tb\"@c.de", "\"\\\t\"@c.de",
    "ä@example.com", "a@ä.com", "a@example.ä", "\xff@example.com", "a@ex\xc3\xa4mple.com", "δοκιμή@παράδειγμα.δοκιμή",
    "\"ä\"@example.com", "a@b.\xc5\xbfe", "a@b.\xe2\x84\xaa", "a@\xe2\x84\xaa.com", "١@example.com", "a@example.c-m",
    str_repeat("a", 64) . "@b.cd", str_repeat("a", 65) . "@b.cd", "a@" . str_repeat("b", 63) . ".cd",
    "a@" . str_repeat("b", 64) . ".cd", "a@b." . str_repeat("c", 63), "a@b." . str_repeat("c", 64),
    str_repeat("a", 64) . "@" . str_repeat("b.", 95) . "cd", str_repeat("a", 60) . "@" . str_repeat("bcdefghij.", 25) . "cd",
    str_repeat("\"a\"", 30) . "@b.cd", str_repeat("\\a", 33) . "@b.cd", "\"" . str_repeat("\\a", 32) . "\"@b.cd",
    "\"" . str_repeat("\\a", 33) . "\"@b.cd", "\"" . str_repeat("a", 62) . "\"@b.cd", "\"" . str_repeat("a", 63) . "\"@b.cd",
    str_repeat("a.", 32) . "a@b.cd", "a@" . str_repeat("b.", 160) . "cd", "a@" . str_repeat("b.", 150) . "cd",
    "\"@\"@b.cd", "\"a@b\"@c.de", "a\"@\"b@c.de"];
foreach ($emails as $s) foreach ([0, FILTER_FLAG_EMAIL_UNICODE] as $f) fv($s, FILTER_VALIDATE_EMAIL, $f);

// --- ip ----------------------------------------------------------------
$ips = ["0.0.0.0", "0.1.2.3", "1.2.3.4", "8.8.8.8", "10.0.0.1", "100.64.0.1", "100.128.0.1", "127.0.0.1", "169.254.1.1",
    "172.16.0.1", "172.31.255.255", "172.32.0.1", "192.0.0.1", "192.0.0.8", "192.0.2.1", "192.88.99.1", "192.168.1.1",
    "198.18.0.1", "198.20.0.1", "198.51.100.1", "203.0.113.1", "224.0.0.1", "239.255.255.255", "240.0.0.1",
    "255.255.255.255", "256.0.0.1", "1.2.3", "1.2.3.4.5", "01.2.3.4", "1.2.3.04", "1.2.3.4 ", " 1.2.3.4", "1..2.3",
    "1.2.3.", ".1.2.3", "1.2.3.4\0", "1.2.3.1000", "", "abc", "::", "::1", "::2", "1::", "1::1", "fe80::1", "febf::1",
    "fec0::1", "fc00::1", "fdff::1", "fe00::1", "2001:db8::1", "2001:0db8:85a3:0000:0000:8a2e:0370:7334", "2001::1",
    "2001:1ff::1", "2001:200::1", "2001:10::1", "2002::1", "64:ff9b::1.2.3.4", "64:ff9b::1", "100::1", "100:0:0:0:1::",
    "::ffff:1.2.3.4", "::ffff:10.0.0.1", "::1.2.3.4", "1:2:3:4:5:6:1.2.3.4", "1:2:3:4:5:6:7:1.2.3.4", "1::1.2.3.4",
    ":1::1", "1:::1", "1::2::3", "1:2:3:4:5:6:7:8", "1:2:3:4:5:6:7:8:9", "1:2:3:4:5:6:7", "1:2:3:4:5:6:7::",
    "::1:2:3:4:5:6:7", "1:2:3:4:5:6:7::8", "12345::1", "g::1", "1:", ":1", ":", ":::", "[::1]", "::1%eth0",
    "2001:db8::1.2.3", "::1.2.3.04", "1.2.3.4:5", "ff02::1", "2620:0:2d0:200::7", "2a00:1450:4001:80b::200e",
    "::ffff:0.0.0.0", "0:0:0:0:0:ffff:1.2.3.4", "::0:ffff:1.2.3.4", "1:2:3:4:5:6:7:8.9", "1.2.3.4::"];
foreach ($ips as $s) {
    foreach ([0, FILTER_FLAG_IPV4, FILTER_FLAG_IPV6, FILTER_FLAG_IPV4 | FILTER_FLAG_IPV6, FILTER_FLAG_NO_PRIV_RANGE,
              FILTER_FLAG_NO_RES_RANGE, FILTER_FLAG_GLOBAL_RANGE, FILTER_FLAG_NO_PRIV_RANGE | FILTER_FLAG_NO_RES_RANGE] as $f) {
        fv($s, FILTER_VALIDATE_IP, $f);
    }
}

// --- mac ---------------------------------------------------------------
$macs = ["01:23:45:67:89:ab", "01-23-45-67-89-AB", "0123.4567.89ab", "01:23:45:67:89", "01:23:45:67:89:ab:cd",
    "01-23:45-67:89-ab", "01:23-45:67:89:ab", "0123.4567.89ag", "0123:4567:89ab", "01:23:45:67:89:gg", "", "0",
    "01.23.45.67.89.ab", "0123-4567-89ab", "01 23 45 67 89 ab", "01:23:45:67:89:a", "+1:23:45:67:89:ab",
    "0x23:45:67:89:ab:c", " 1:23:45:67:89:ab", "01234567.89ab", "0123.456789ab"];
foreach ($macs as $s) {
    fv($s, FILTER_VALIDATE_MAC);
    foreach ([":", "-", ".", "", "::", 5] as $sep) fv($s, FILTER_VALIDATE_MAC, ["options" => ["separator" => $sep]]);
}

// --- sanitizers --------------------------------------------------------
$allbytes = '';
for ($i = 0; $i < 256; $i++) $allbytes .= chr($i);
$texts = [$allbytes, "", "<b>bold</b> 'quoted' \"double\" & amp", "a<!-- comment -->b", "x<?php echo 1; ?>y",
    "<script>alert(1)</script>", "1 < 2 > 0", "<a href=\"x>y\">t</a>", "&amp; &foo; &#65; &#x41; &#xZ; &apos; &#1114112; &",
    "é ü € \xff", "\xc3\xa4\xe2\x82\xac\xf0\x9f\x98\x80", "`backtick`", "tab\there\nnl\0nul", "O'Henry",
    "http://a.b/c?d=e&f=g#h", "foo@bar.com (comment)", "-1,234.567e+8x", "+-.,eE09abc", "<?xml version='1.0'?>a",
    "<!DOCTYPE html>b", "a < b <c> d", "\"<\"a\">\"", "<<a>>", "a>b", "<a\n>b", "\\'\"\0"];
$sflags = [0, FILTER_FLAG_STRIP_LOW, FILTER_FLAG_STRIP_HIGH, FILTER_FLAG_STRIP_BACKTICK, FILTER_FLAG_ENCODE_LOW,
    FILTER_FLAG_ENCODE_HIGH, FILTER_FLAG_ENCODE_AMP, FILTER_FLAG_NO_ENCODE_QUOTES, FILTER_FLAG_EMPTY_STRING_NULL,
    FILTER_FLAG_STRIP_LOW | FILTER_FLAG_STRIP_HIGH | FILTER_FLAG_STRIP_BACKTICK,
    FILTER_FLAG_ENCODE_LOW | FILTER_FLAG_ENCODE_HIGH | FILTER_FLAG_ENCODE_AMP,
    FILTER_FLAG_STRIP_LOW | FILTER_FLAG_ENCODE_HIGH, FILTER_FLAG_ALLOW_FRACTION, FILTER_FLAG_ALLOW_THOUSAND,
    FILTER_FLAG_ALLOW_SCIENTIFIC, FILTER_FLAG_ALLOW_FRACTION | FILTER_FLAG_ALLOW_THOUSAND | FILTER_FLAG_ALLOW_SCIENTIFIC];
foreach ([0x0201, FILTER_SANITIZE_ENCODED, FILTER_SANITIZE_SPECIAL_CHARS, FILTER_SANITIZE_FULL_SPECIAL_CHARS,
          FILTER_UNSAFE_RAW, FILTER_SANITIZE_EMAIL, FILTER_SANITIZE_URL, FILTER_SANITIZE_NUMBER_INT,
          FILTER_SANITIZE_NUMBER_FLOAT, FILTER_SANITIZE_ADD_SLASHES] as $filter) {
    foreach ($texts as $t) foreach ($sflags as $f) fv($t, $filter, $f);
    foreach ($scalars as $v) fv($v, $filter);
}
foreach (["<a>", "", "x"] as $t) fv($t, 0x0201, ["flags" => FILTER_FLAG_EMPTY_STRING_NULL, "options" => ["default" => "d"]]);

// --- engine: flags, arrays, options ------------------------------------
$arrays = [[], [1, "2", "x"], ["a" => "1", "b" => ["2", ["3", "y"]]], [[[["deep"]]]], ["k" => null, 5 => true],
           [new WithToString, new NoToString]];
foreach ([FILTER_VALIDATE_INT, FILTER_UNSAFE_RAW, FILTER_VALIDATE_BOOL, FILTER_SANITIZE_NUMBER_INT] as $filter) {
    foreach (array_merge($arrays, ["5", 5, null]) as $v) {
        foreach ([0, FILTER_REQUIRE_ARRAY, FILTER_FORCE_ARRAY, FILTER_REQUIRE_SCALAR, FILTER_NULL_ON_FAILURE,
                  FILTER_REQUIRE_ARRAY | FILTER_NULL_ON_FAILURE, FILTER_FORCE_ARRAY | FILTER_NULL_ON_FAILURE,
                  FILTER_REQUIRE_ARRAY | FILTER_FORCE_ARRAY, FILTER_REQUIRE_SCALAR | FILTER_FORCE_ARRAY] as $f) {
            fv($v, $filter, $f);
            fv($v, $filter, ["flags" => $f, "options" => ["default" => "D", "min_range" => 2]]);
        }
    }
}
foreach ([[], ["flags" => "abc"], ["flags" => "16777216"], ["flags" => 1.9], ["filter" => FILTER_VALIDATE_INT],
          ["filter" => "257"], ["filter" => 9999], ["filter" => FILTER_VALIDATE_INT, "flags" => FILTER_FORCE_ARRAY],
          ["options" => "notarray"], ["options" => ["default" => [1]]], ["flags" => null], ["filter" => null],
          ["options" => null, "flags" => FILTER_NULL_ON_FAILURE]] as $o) {
    foreach (["12", "x", ["1", "x"], null] as $v) {
        fv($v, FILTER_VALIDATE_INT, $o);
        fv($v, FILTER_DEFAULT, $o);
    }
}
foreach ([-1, 0, 1, 255, 256, 0x104, 0x116, 0x1ff, 0x209, 0x20c, 0x3ff, 0x401, 999, PHP_INT_MAX] as $id) fv("x", $id);

// callbacks
fv("abc", FILTER_CALLBACK, ["options" => "strtoupper"]);
fv(["a", ["b"]], FILTER_CALLBACK, ["options" => "strtoupper"]);
fv(["a", ["b"]], FILTER_CALLBACK, ["options" => "strtoupper", "flags" => FILTER_REQUIRE_SCALAR]);
fv(["a", ["b"]], FILTER_CALLBACK, ["options" => "strtoupper", "flags" => FILTER_FORCE_ARRAY]);
fv("abc", FILTER_CALLBACK, ["options" => function ($s) { return strlen($s); }]);
fv("abc", FILTER_CALLBACK, ["options" => function ($s) { return null; }]);
fv(5, FILTER_CALLBACK, ["options" => function ($s) { return gettype($s); }]);
fv("abc", FILTER_CALLBACK, ["options" => "no_such_function"]);
fv("abc", FILTER_CALLBACK, ["options" => ["no", "callable"]]);
fv("abc", FILTER_CALLBACK);
fv("abc", FILTER_CALLBACK, ["flags" => 0]);
fv("abc", FILTER_CALLBACK, 0);
fv("abc", FILTER_VALIDATE_INT, ["filter" => FILTER_CALLBACK, "options" => "strrev"]);

// filter_var_array
$data = ["int" => "12", "bad" => "x", "arr" => ["1", "2", "z"], "f" => "1.5", "s" => "<b>t</b>", "e" => "", "n" => null,
         "nested" => ["a" => ["1"]], 7 => "seven", "b" => "yes"];
$defs = [
    FILTER_VALIDATE_INT, FILTER_DEFAULT, 0, 9999, FILTER_CALLBACK,
    ["int" => FILTER_VALIDATE_INT, "bad" => FILTER_VALIDATE_INT, "missing" => FILTER_VALIDATE_INT],
    ["int" => ["filter" => FILTER_VALIDATE_INT, "flags" => FILTER_FORCE_ARRAY]],
    ["arr" => ["filter" => FILTER_VALIDATE_INT, "flags" => FILTER_REQUIRE_ARRAY]],
    ["arr" => FILTER_VALIDATE_INT, "f" => FILTER_VALIDATE_FLOAT, "b" => FILTER_VALIDATE_BOOL],
    ["arr" => ["filter" => FILTER_VALIDATE_INT, "flags" => FILTER_FORCE_ARRAY | FILTER_NULL_ON_FAILURE]],
    ["int" => ["flags" => FILTER_NULL_ON_FAILURE]], ["int" => []], ["int" => 9999], ["int" => "abc"],
    ["int" => ["filter" => 9999]], ["s" => ["filter" => FILTER_UNSAFE_RAW, "flags" => FILTER_FLAG_ENCODE_HIGH]],
    ["e" => ["filter" => FILTER_UNSAFE_RAW, "flags" => FILTER_FLAG_EMPTY_STRING_NULL]],
    ["n" => FILTER_VALIDATE_INT, "nested" => ["filter" => FILTER_VALIDATE_INT, "flags" => FILTER_REQUIRE_ARRAY]],
    ["int" => ["filter" => FILTER_VALIDATE_INT, "options" => ["min_range" => 20, "default" => 99]]],
    [7 => FILTER_VALIDATE_INT], ["" => FILTER_VALIDATE_INT], ["7" => FILTER_VALIDATE_INT], ["int" => FILTER_VALIDATE_INT, 0 => 1],
    ["int" => ["filter" => FILTER_VALIDATE_INT, "flags" => FILTER_NULL_ON_FAILURE | 0x10000000]],
];
foreach ($defs as $d) {
    fva($data, $d);
    fva($data, $d, false);
}
fva([], ["a" => FILTER_VALIDATE_INT]);
fva(["a" => ["b" => "1"]], FILTER_VALIDATE_INT);
fva(["a" => new WithToString, "b" => new NoToString], FILTER_VALIDATE_INT);

// --- deterministic fuzzing --------------------------------------------
mt_srand(20261103);
function rnd(string $alphabet, int $max): string {
    $n = mt_rand(0, $max);
    $s = '';
    for ($i = 0; $i < $n; $i++) $s .= $alphabet[mt_rand(0, strlen($alphabet) - 1)];
    return $s;
}
for ($i = 0; $i < 600; $i++) {
    $s = rnd("0123456789xXoO+- \t.e", 8);
    fv($s, FILTER_VALIDATE_INT, FILTER_FLAG_ALLOW_OCTAL | FILTER_FLAG_ALLOW_HEX);
    fv($s, FILTER_VALIDATE_INT);
}
for ($i = 0; $i < 800; $i++) {
    $s = rnd("0123456789.,'eE+- ", 10);
    fv($s, FILTER_VALIDATE_FLOAT, FILTER_FLAG_ALLOW_THOUSAND);
    fv($s, FILTER_VALIDATE_FLOAT);
}
for ($i = 0; $i < 800; $i++) {
    $s = rnd("0123456789abcdefABCDEF:.", 16);
    fv($s, FILTER_VALIDATE_IP);
    fv($s, FILTER_VALIDATE_IP, FILTER_FLAG_NO_RES_RANGE | FILTER_FLAG_NO_PRIV_RANGE);
}
for ($i = 0; $i < 400; $i++) {
    fv(rnd("0123456789abcdefg:-.", 17), FILTER_VALIDATE_MAC);
    $m = implode(mt_rand(0, 1) ? ':' : '-', array_map(fn($x) => rnd("0123456789abcdefABCDEFg", 2), range(1, 6)));
    fv($m, FILTER_VALIDATE_MAC);
}
for ($i = 0; $i < 600; $i++) {
    $s = rnd("ab1-._", 10);
    fv($s, FILTER_VALIDATE_DOMAIN, FILTER_FLAG_HOSTNAME);
    fv($s, FILTER_VALIDATE_DOMAIN);
}
for ($i = 0; $i < 1500; $i++) {
    $s = rnd("ab1:/@?#[]-.%_", 18);
    fv($s, FILTER_VALIDATE_URL);
    fv("http://" . rnd("ab1:/@?#[]-.%", 12), FILTER_VALIDATE_URL);
}
for ($i = 0; $i < 2500; $i++) {
    $s = rnd("ab1.@\"\\[]:- x", 14);
    fv($s, FILTER_VALIDATE_EMAIL);
    fv(rnd("ab\"\\.", 6) . "@" . rnd("ab1-.", 8), FILTER_VALIDATE_EMAIL);
    fv(rnd("ab.", 3) . "@[" . rnd("IPv6:1af.", 22) . "]", FILTER_VALIDATE_EMAIL);
}
for ($i = 0; $i < 300; $i++) {
    fv(rnd("<>!-?\"'xml\\ab/", 14), 0x0201);
}
