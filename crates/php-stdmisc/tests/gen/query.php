<?php
// Fixture generator: http_build_query() over many shapes and parse_str()
// over many query strings.
// Output: "hbq" hex(serialize(data)) hex(prefix) sepTag enc hex(result)
//         "ps" hex(input) hex(serialize(result))
class Pub { public $a = 1; public $b = [1, 2]; protected $c = 3; private $d = 4; public $e = null; }
$o = new stdClass; $o->x = "y"; $o->{"sp ace"} = "v&w"; $o->{"0"} = "zero"; $o->n = new stdClass; $o->n->deep = true;
$datasets = [
  [], ["a" => "b"], ["a" => "b", "c" => "d e"], [1, 2, 3], ["foo", "bar", "k" => "v"], [-5 => "neg", "1.5" => "f", "01" => "lead"],
  ["a" => null, "b" => true, "c" => false, "d" => 0, "e" => -1, "f" => 1.5, "g" => 0.1 + 0.2, "h" => 1e100, "i" => -0.0, "j" => 1e15, "k" => 123456789012345678],
  ["a" => ["b" => ["c" => "d"]], "e" => [1, [2, [3]]]], ["x" => []], [[]], [[1, 2], [3, 4]], ["k y" => ["s p" => "v a"]],
  ["ü" => "ö", "€" => "~-_.!*'()", "a+b" => "c/d?e#f", "%" => "%25"], ["arr" => ["k" => null, "l" => [null]]],
  ["a" => "\0\x01\x7f\xff", "\x80" => " "], [5 => ["x" => [7 => "y"]]], ["obj" => $o], $o, new Pub, ["p" => new Pub],
  ["a" => [0 => "a", 1 => "b", "x" => ["y" => [0 => "z"]]]], [PHP_INT_MAX => "max", PHP_INT_MIN => "min"],
];
$prefixes = ["", "p_", "0", "a b", "&"];
$seps = [null, "&", "&amp;", ";", "", "[]"];
foreach ($datasets as $d) {
  foreach ($prefixes as $p) {
    foreach ($seps as $si => $s) {
      foreach ([PHP_QUERY_RFC1738, PHP_QUERY_RFC3986] as $enc) {
        $r = @http_build_query($d, $p, $s, $enc);
        echo "hbq ", bin2hex(serialize($d)), " ", bin2hex($p), " ", $s === null ? "n" : "s" . bin2hex($s), " $enc ", bin2hex($r), "\n";
      }
    }
  }
}
$inputs = [
  "", "a", "a=", "=a", "=", "&", "&&", "a=1&b=2", "a=1&&b=2&", "a=1&a=2", "a[]=1&a[]=2", "a[]=1&a=2&a[]=3", "a[b]=1&a[c]=2", "a[b][c]=1", "a[b][]=1&a[b][]=2",
  "a[0]=x&a[1]=y", "a[1]=x&a[]=y", "a[-1]=x&a[]=y", "a[01]=x", "a[ x]=1", "a[  x]=1", "a[\tx]=1", "a[ ]=1", "a[x ]=1", "a[x]y=1", "a[x]y[z]=1", "a[x][y=1", "a[x=1", "a[=1", "a]=1",
  "a.b=1", "a b=1", " a=1", "  a b.c=1", "a.b[c.d]=1", "a b[c d]=1", "a[b.c=1", "a[b c=1", "a[b[c]=1", "a[]]=1", "a[[]=1", "[]=1", "[a]=1", ".=1", " =1",
  "a%5B%5D=1&a%5B%5D=2", "a%20b=c%20d", "a+b=c+d", "a%2Bb=c%2Bd", "%61=%62", "a=%zz", "a=%", "a=%4", "a=%41%", "a%00b=c", "a=b%00c", "ab%5b=1",
  "GLOBALS=1", "this=1", "_GET=1", "123=x", "-1=x", "1.5=x", "0x1=x", "a[123]=x&a[0123]=y", "a[9223372036854775807]=x&a[]=y", "a[9223372036854775808]=x",
  "__Host-x=1", "..Host-x=1", "__Secure-x=1", "..Secure-x=1", "a[__Host-x]=1", "a[..Host-x]=1", "__Host-x[a]=1", "..Host-x[a]=1",
  "a=1;b=2", "a=1&amp;b=2", "first=value&arr[]=foo+bar&arr[]=baz", "a[b]=1&a=2&a[c]=3", "a=1&a[b]=2", "a[b]=1&a[b][c]=2",
  "x[a][b][c][d][e][f]=1", "foo[bar]baz=1", "a[b]c[d]=1", "a[]b=1", "a[b]=1&a[B]=2", "arr[1]=sid&arr[4][2]=fred", "arr[1]=sid&arr[4][2]=fred&arr[4][3]=jason",
  "a=1=2", "a==1", "&a=1", "a&b&c", "a[]&b[]", "%26=%3D", "%3D=%26", "a%5Bb%5D=1", "a%5B=1", "a%5D=1", "a[%5D]=1",
  "\xc3\xa9=\xc3\xa9", "\xff=\xfe", str_repeat("a[", 70) . "=1", str_repeat("[a]", 65) . "x=1", "b" . str_repeat("[a]", 63) . "=1", "b" . str_repeat("[a]", 64) . "=1", "b" . str_repeat("[a]", 65) . "=1",
];
foreach ($inputs as $in) {
  parse_str($in, $r);
  echo "ps ", bin2hex($in), " ", bin2hex(serialize($r)), "\n";
}
