// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Writes of static properties on the cached paths
//! (`exec::spec::statics`): a typed property takes a value its type
//! accepts as it is without the full write (an int, a string, an array, an
//! instance of the one class it names, a null once it is initialized),
//! and `++`/`--` keep an int an int and a float a float. Everything else
//! is the full write's, and must come out the same: coercions and their
//! `TypeError`s for every value kind and property type, an uninitialized
//! property, `private(set)`, an int overflow in a typed int, a reference
//! bound to the property, the destructor of an overwritten object running
//! at the assignment, inherited and redeclared properties. Each round
//! repeats the writes, so the later rounds run on warm cache entries.

use std::io::Write;
use std::process::{Command, Stdio};

/// Runs `source` (read from standard input) with no php.ini, every error
/// shown and none logged.
fn run(source: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_php"))
        .args([
            "-n",
            "-d",
            "memory_limit=1G",
            "-d",
            "log_errors=0",
            "-d",
            "display_errors=1",
            "-d",
            "error_reporting=-1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the CLI");
    child
        .stdin
        .take()
        .expect("CLI stdin")
        .write_all(source.as_bytes())
        .expect("write the script");
    let out = child.wait_with_output().expect("run the script");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Expected output: PHP 8.5.
#[test]
fn static_property_writes_match_the_full_write() {
    assert_eq!(run(SOURCE), EXPECTED);
}

const SOURCE: &str = r##"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "[destruct {$this->n}]\n"; } }
class S {
  public static int $i = 0; public static ?int $ni = null; public static float $f = 1.5; public static string $s = 'a'; public static bool $b = false;
  public static int|string $u = 0; public static array $a = []; public static ?array $na = null; public static ?D $d = null; public static object $o;
  public static iterable $it = []; public static $plain = 0; public static int $uninit; public private(set) static int $ps = 1; public static mixed $m = 1;
  public static ?S $self = null; public static int|float $num = 0; public static \Closure|null $c = null;
  static function setPs($v) { self::$ps = $v; } static function setI($v) { static::$i = $v; self::$i = $v; return self::$i; }
}
class T extends S { public static int $own = 5; }
function show($f) { try { $r = $f(); var_dump($r); } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; } }
$vals = [1, '5', 'x', 1.5, 2.0, true, null, [1], new stdClass];
for ($round = 0; $round < 3; $round++) {
  echo "round $round\n";
  foreach ($vals as $k => $v) {
    echo "-- val $k\n";
    show(function () use ($v) { S::$i = $v; return S::$i; }); show(function () use ($v) { S::$ni = $v; return S::$ni; }); show(function () use ($v) { S::$f = $v; return S::$f; });
    show(function () use ($v) { S::$s = $v; return S::$s; }); show(function () use ($v) { S::$b = $v; return S::$b; }); show(function () use ($v) { S::$u = $v; return S::$u; });
    show(function () use ($v) { S::$a = $v; return S::$a; }); show(function () use ($v) { S::$na = $v; return S::$na; }); show(function () use ($v) { S::$o = $v; return S::$o; });
    show(function () use ($v) { S::$it = $v; return S::$it; }); show(function () use ($v) { S::$plain = $v; return S::$plain; }); show(function () use ($v) { S::$m = $v; return S::$m; });
    show(function () use ($v) { S::$num = $v; return S::$num; }); show(function () use ($v) { return S::$i = $v; });
  }
  show(fn() => S::$uninit); show(function () { S::$uninit = 3; return S::$uninit; }); show(fn() => isset(S::$uninit));
  show(function () { S::$ps = 2; return S::$ps; }); show(function () { S::setPs(7); return S::$ps; }); show(fn() => S::setI(9)); show(fn() => T::setI(11)); show(function () { T::$own = 6; T::$i = 12; return [T::$own, S::$i]; });
  show(function () { S::$i = PHP_INT_MAX; S::$i++; return S::$i; }); show(function () { S::$i = 5; S::$i++; ++S::$i; S::$i--; return S::$i++; }); show(fn() => S::$i);
  show(function () { S::$f = 1.5; S::$f++; return S::$f; }); show(function () { S::$num = PHP_INT_MAX; S::$num++; return S::$num; }); show(function () { S::$s = 'a'; S::$s++; return S::$s; }); show(function () { S::$ni = null; S::$ni++; return S::$ni; });
  show(function () { S::$d = new D('first'); S::$d = new D('second'); echo "assigned\n"; S::$d = null; echo "nulled\n"; return S::$d; });
  show(function () { S::$plain = new D('p1'); S::$plain = [new D('p2')]; echo "replaced\n"; S::$plain = 0; echo "zeroed\n"; return S::$plain; });
  show(function () { $r = &S::$i; $r = 40; S::$i = 41; $x = $r; unset($r); return [$x, S::$i]; }); show(function () { $r = &S::$i; S::$i = 'str'; return 0; }); show(function () { $r = &S::$plain; S::$plain = [1, 2]; return $r; });
  show(function () { S::$a = [1]; $copy = S::$a; S::$a = [2, 3]; return [$copy, S::$a]; }); show(function () { S::$a[] = 4; S::$a['k'] = 5; return S::$a; }); show(function () { S::$self = new S; return S::$self instanceof S; }); show(function () { S::$self = new stdClass; });
  show(function () { S::$c = fn() => 1; return (S::$c)(); }); show(function () { S::$c = 'strlen'; });
  show(function () { $v = 3; S::$i = $v + 1; S::$s = 'x' . $v; S::$a = [$v]; return [S::$i, S::$s, S::$a]; });
}
echo "end\n";
"##;

const EXPECTED: &str = r##"round 0
-- val 0
int(1)
int(1)
float(1)
string(1) "1"
bool(true)
int(1)
TypeError: Cannot assign int to property S::$a of type array
TypeError: Cannot assign int to property S::$na of type ?array
TypeError: Cannot assign int to property S::$o of type object
TypeError: Cannot assign int to property S::$it of type Traversable|array
int(1)
int(1)
int(1)
int(1)
-- val 1
int(5)
int(5)
float(5)
string(1) "5"
bool(true)
string(1) "5"
TypeError: Cannot assign string to property S::$a of type array
TypeError: Cannot assign string to property S::$na of type ?array
TypeError: Cannot assign string to property S::$o of type object
TypeError: Cannot assign string to property S::$it of type Traversable|array
string(1) "5"
string(1) "5"
int(5)
int(5)
-- val 2
TypeError: Cannot assign string to property S::$i of type int
TypeError: Cannot assign string to property S::$ni of type ?int
TypeError: Cannot assign string to property S::$f of type float
string(1) "x"
bool(true)
string(1) "x"
TypeError: Cannot assign string to property S::$a of type array
TypeError: Cannot assign string to property S::$na of type ?array
TypeError: Cannot assign string to property S::$o of type object
TypeError: Cannot assign string to property S::$it of type Traversable|array
string(1) "x"
string(1) "x"
TypeError: Cannot assign string to property S::$num of type int|float
TypeError: Cannot assign string to property S::$i of type int
-- val 3

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 17
int(1)

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 17
int(1)
float(1.5)
string(3) "1.5"
bool(true)

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 18
int(1)
TypeError: Cannot assign float to property S::$a of type array
TypeError: Cannot assign float to property S::$na of type ?array
TypeError: Cannot assign float to property S::$o of type object
TypeError: Cannot assign float to property S::$it of type Traversable|array
float(1.5)
float(1.5)
float(1.5)

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 21
int(1)
-- val 4
int(2)
int(2)
float(2)
string(1) "2"
bool(true)
int(2)
TypeError: Cannot assign float to property S::$a of type array
TypeError: Cannot assign float to property S::$na of type ?array
TypeError: Cannot assign float to property S::$o of type object
TypeError: Cannot assign float to property S::$it of type Traversable|array
float(2)
float(2)
float(2)
int(2)
-- val 5
int(1)
int(1)
float(1)
string(1) "1"
bool(true)
int(1)
TypeError: Cannot assign true to property S::$a of type array
TypeError: Cannot assign true to property S::$na of type ?array
TypeError: Cannot assign true to property S::$o of type object
TypeError: Cannot assign true to property S::$it of type Traversable|array
bool(true)
bool(true)
int(1)
int(1)
-- val 6
TypeError: Cannot assign null to property S::$i of type int
NULL
TypeError: Cannot assign null to property S::$f of type float
TypeError: Cannot assign null to property S::$s of type string
TypeError: Cannot assign null to property S::$b of type bool
TypeError: Cannot assign null to property S::$u of type string|int
TypeError: Cannot assign null to property S::$a of type array
NULL
TypeError: Cannot assign null to property S::$o of type object
TypeError: Cannot assign null to property S::$it of type Traversable|array
NULL
NULL
TypeError: Cannot assign null to property S::$num of type int|float
TypeError: Cannot assign null to property S::$i of type int
-- val 7
TypeError: Cannot assign array to property S::$i of type int
TypeError: Cannot assign array to property S::$ni of type ?int
TypeError: Cannot assign array to property S::$f of type float
TypeError: Cannot assign array to property S::$s of type string
TypeError: Cannot assign array to property S::$b of type bool
TypeError: Cannot assign array to property S::$u of type string|int
array(1) {
  [0]=>
  int(1)
}
array(1) {
  [0]=>
  int(1)
}
TypeError: Cannot assign array to property S::$o of type object
array(1) {
  [0]=>
  int(1)
}
array(1) {
  [0]=>
  int(1)
}
array(1) {
  [0]=>
  int(1)
}
TypeError: Cannot assign array to property S::$num of type int|float
TypeError: Cannot assign array to property S::$i of type int
-- val 8
TypeError: Cannot assign stdClass to property S::$i of type int
TypeError: Cannot assign stdClass to property S::$ni of type ?int
TypeError: Cannot assign stdClass to property S::$f of type float
TypeError: Cannot assign stdClass to property S::$s of type string
TypeError: Cannot assign stdClass to property S::$b of type bool
TypeError: Cannot assign stdClass to property S::$u of type string|int
TypeError: Cannot assign stdClass to property S::$a of type array
TypeError: Cannot assign stdClass to property S::$na of type ?array
object(stdClass)#1 (0) {
}
TypeError: Cannot assign stdClass to property S::$it of type Traversable|array
object(stdClass)#1 (0) {
}
object(stdClass)#1 (0) {
}
TypeError: Cannot assign stdClass to property S::$num of type int|float
TypeError: Cannot assign stdClass to property S::$i of type int
Error: Typed static property S::$uninit must not be accessed before initialization
int(3)
bool(true)
Error: Cannot modify private(set) property S::$ps from global scope
int(7)
int(9)
int(11)
array(2) {
  [0]=>
  int(6)
  [1]=>
  int(12)
}
TypeError: Cannot increment property S::$i of type int past its maximal value
int(6)
int(7)
float(2.5)
float(9.223372036854776E+18)

Deprecated: Increment on non-numeric string is deprecated, use str_increment() instead in Standard input code on line 26
string(1) "b"
int(1)
[destruct first]
assigned
[destruct second]
nulled
NULL
[destruct p1]
replaced
[destruct p2]
zeroed
int(0)
array(2) {
  [0]=>
  int(41)
  [1]=>
  int(41)
}
TypeError: Cannot assign string to property S::$i of type int
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(2) {
  [0]=>
  array(1) {
    [0]=>
    int(1)
  }
  [1]=>
  array(2) {
    [0]=>
    int(2)
    [1]=>
    int(3)
  }
}
array(4) {
  [0]=>
  int(2)
  [1]=>
  int(3)
  [2]=>
  int(4)
  ["k"]=>
  int(5)
}
bool(true)
TypeError: Cannot assign stdClass to property S::$self of type ?S
int(1)
TypeError: Cannot assign string to property S::$c of type ?Closure
array(3) {
  [0]=>
  int(4)
  [1]=>
  string(2) "x3"
  [2]=>
  array(1) {
    [0]=>
    int(3)
  }
}
round 1
-- val 0
int(1)
int(1)
float(1)
string(1) "1"
bool(true)
int(1)
TypeError: Cannot assign int to property S::$a of type array
TypeError: Cannot assign int to property S::$na of type ?array
TypeError: Cannot assign int to property S::$o of type object
TypeError: Cannot assign int to property S::$it of type Traversable|array
int(1)
int(1)
int(1)
int(1)
-- val 1
int(5)
int(5)
float(5)
string(1) "5"
bool(true)
string(1) "5"
TypeError: Cannot assign string to property S::$a of type array
TypeError: Cannot assign string to property S::$na of type ?array
TypeError: Cannot assign string to property S::$o of type object
TypeError: Cannot assign string to property S::$it of type Traversable|array
string(1) "5"
string(1) "5"
int(5)
int(5)
-- val 2
TypeError: Cannot assign string to property S::$i of type int
TypeError: Cannot assign string to property S::$ni of type ?int
TypeError: Cannot assign string to property S::$f of type float
string(1) "x"
bool(true)
string(1) "x"
TypeError: Cannot assign string to property S::$a of type array
TypeError: Cannot assign string to property S::$na of type ?array
TypeError: Cannot assign string to property S::$o of type object
TypeError: Cannot assign string to property S::$it of type Traversable|array
string(1) "x"
string(1) "x"
TypeError: Cannot assign string to property S::$num of type int|float
TypeError: Cannot assign string to property S::$i of type int
-- val 3

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 17
int(1)

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 17
int(1)
float(1.5)
string(3) "1.5"
bool(true)

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 18
int(1)
TypeError: Cannot assign float to property S::$a of type array
TypeError: Cannot assign float to property S::$na of type ?array
TypeError: Cannot assign float to property S::$o of type object
TypeError: Cannot assign float to property S::$it of type Traversable|array
float(1.5)
float(1.5)
float(1.5)

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 21
int(1)
-- val 4
int(2)
int(2)
float(2)
string(1) "2"
bool(true)
int(2)
TypeError: Cannot assign float to property S::$a of type array
TypeError: Cannot assign float to property S::$na of type ?array
TypeError: Cannot assign float to property S::$o of type object
TypeError: Cannot assign float to property S::$it of type Traversable|array
float(2)
float(2)
float(2)
int(2)
-- val 5
int(1)
int(1)
float(1)
string(1) "1"
bool(true)
int(1)
TypeError: Cannot assign true to property S::$a of type array
TypeError: Cannot assign true to property S::$na of type ?array
TypeError: Cannot assign true to property S::$o of type object
TypeError: Cannot assign true to property S::$it of type Traversable|array
bool(true)
bool(true)
int(1)
int(1)
-- val 6
TypeError: Cannot assign null to property S::$i of type int
NULL
TypeError: Cannot assign null to property S::$f of type float
TypeError: Cannot assign null to property S::$s of type string
TypeError: Cannot assign null to property S::$b of type bool
TypeError: Cannot assign null to property S::$u of type string|int
TypeError: Cannot assign null to property S::$a of type array
NULL
TypeError: Cannot assign null to property S::$o of type object
TypeError: Cannot assign null to property S::$it of type Traversable|array
NULL
NULL
TypeError: Cannot assign null to property S::$num of type int|float
TypeError: Cannot assign null to property S::$i of type int
-- val 7
TypeError: Cannot assign array to property S::$i of type int
TypeError: Cannot assign array to property S::$ni of type ?int
TypeError: Cannot assign array to property S::$f of type float
TypeError: Cannot assign array to property S::$s of type string
TypeError: Cannot assign array to property S::$b of type bool
TypeError: Cannot assign array to property S::$u of type string|int
array(1) {
  [0]=>
  int(1)
}
array(1) {
  [0]=>
  int(1)
}
TypeError: Cannot assign array to property S::$o of type object
array(1) {
  [0]=>
  int(1)
}
array(1) {
  [0]=>
  int(1)
}
array(1) {
  [0]=>
  int(1)
}
TypeError: Cannot assign array to property S::$num of type int|float
TypeError: Cannot assign array to property S::$i of type int
-- val 8
TypeError: Cannot assign stdClass to property S::$i of type int
TypeError: Cannot assign stdClass to property S::$ni of type ?int
TypeError: Cannot assign stdClass to property S::$f of type float
TypeError: Cannot assign stdClass to property S::$s of type string
TypeError: Cannot assign stdClass to property S::$b of type bool
TypeError: Cannot assign stdClass to property S::$u of type string|int
TypeError: Cannot assign stdClass to property S::$a of type array
TypeError: Cannot assign stdClass to property S::$na of type ?array
object(stdClass)#1 (0) {
}
TypeError: Cannot assign stdClass to property S::$it of type Traversable|array
object(stdClass)#1 (0) {
}
object(stdClass)#1 (0) {
}
TypeError: Cannot assign stdClass to property S::$num of type int|float
TypeError: Cannot assign stdClass to property S::$i of type int
int(3)
int(3)
bool(true)
Error: Cannot modify private(set) property S::$ps from global scope
int(7)
int(9)
int(11)
array(2) {
  [0]=>
  int(6)
  [1]=>
  int(12)
}
TypeError: Cannot increment a reference held by property S::$i of type int past its maximal value
int(6)
int(7)
float(2.5)
float(9.223372036854776E+18)

Deprecated: Increment on non-numeric string is deprecated, use str_increment() instead in Standard input code on line 26
string(1) "b"
int(1)
[destruct first]
assigned
[destruct second]
nulled
NULL
[destruct p1]
replaced
[destruct p2]
zeroed
int(0)
array(2) {
  [0]=>
  int(41)
  [1]=>
  int(41)
}
TypeError: Cannot assign string to property S::$i of type int
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(2) {
  [0]=>
  array(1) {
    [0]=>
    int(1)
  }
  [1]=>
  array(2) {
    [0]=>
    int(2)
    [1]=>
    int(3)
  }
}
array(4) {
  [0]=>
  int(2)
  [1]=>
  int(3)
  [2]=>
  int(4)
  ["k"]=>
  int(5)
}
bool(true)
TypeError: Cannot assign stdClass to property S::$self of type ?S
int(1)
TypeError: Cannot assign string to property S::$c of type ?Closure
array(3) {
  [0]=>
  int(4)
  [1]=>
  string(2) "x3"
  [2]=>
  array(1) {
    [0]=>
    int(3)
  }
}
round 2
-- val 0
int(1)
int(1)
float(1)
string(1) "1"
bool(true)
int(1)
TypeError: Cannot assign int to property S::$a of type array
TypeError: Cannot assign int to property S::$na of type ?array
TypeError: Cannot assign int to property S::$o of type object
TypeError: Cannot assign int to property S::$it of type Traversable|array
int(1)
int(1)
int(1)
int(1)
-- val 1
int(5)
int(5)
float(5)
string(1) "5"
bool(true)
string(1) "5"
TypeError: Cannot assign string to property S::$a of type array
TypeError: Cannot assign string to property S::$na of type ?array
TypeError: Cannot assign string to property S::$o of type object
TypeError: Cannot assign string to property S::$it of type Traversable|array
string(1) "5"
string(1) "5"
int(5)
int(5)
-- val 2
TypeError: Cannot assign string to property S::$i of type int
TypeError: Cannot assign string to property S::$ni of type ?int
TypeError: Cannot assign string to property S::$f of type float
string(1) "x"
bool(true)
string(1) "x"
TypeError: Cannot assign string to property S::$a of type array
TypeError: Cannot assign string to property S::$na of type ?array
TypeError: Cannot assign string to property S::$o of type object
TypeError: Cannot assign string to property S::$it of type Traversable|array
string(1) "x"
string(1) "x"
TypeError: Cannot assign string to property S::$num of type int|float
TypeError: Cannot assign string to property S::$i of type int
-- val 3

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 17
int(1)

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 17
int(1)
float(1.5)
string(3) "1.5"
bool(true)

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 18
int(1)
TypeError: Cannot assign float to property S::$a of type array
TypeError: Cannot assign float to property S::$na of type ?array
TypeError: Cannot assign float to property S::$o of type object
TypeError: Cannot assign float to property S::$it of type Traversable|array
float(1.5)
float(1.5)
float(1.5)

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 21
int(1)
-- val 4
int(2)
int(2)
float(2)
string(1) "2"
bool(true)
int(2)
TypeError: Cannot assign float to property S::$a of type array
TypeError: Cannot assign float to property S::$na of type ?array
TypeError: Cannot assign float to property S::$o of type object
TypeError: Cannot assign float to property S::$it of type Traversable|array
float(2)
float(2)
float(2)
int(2)
-- val 5
int(1)
int(1)
float(1)
string(1) "1"
bool(true)
int(1)
TypeError: Cannot assign true to property S::$a of type array
TypeError: Cannot assign true to property S::$na of type ?array
TypeError: Cannot assign true to property S::$o of type object
TypeError: Cannot assign true to property S::$it of type Traversable|array
bool(true)
bool(true)
int(1)
int(1)
-- val 6
TypeError: Cannot assign null to property S::$i of type int
NULL
TypeError: Cannot assign null to property S::$f of type float
TypeError: Cannot assign null to property S::$s of type string
TypeError: Cannot assign null to property S::$b of type bool
TypeError: Cannot assign null to property S::$u of type string|int
TypeError: Cannot assign null to property S::$a of type array
NULL
TypeError: Cannot assign null to property S::$o of type object
TypeError: Cannot assign null to property S::$it of type Traversable|array
NULL
NULL
TypeError: Cannot assign null to property S::$num of type int|float
TypeError: Cannot assign null to property S::$i of type int
-- val 7
TypeError: Cannot assign array to property S::$i of type int
TypeError: Cannot assign array to property S::$ni of type ?int
TypeError: Cannot assign array to property S::$f of type float
TypeError: Cannot assign array to property S::$s of type string
TypeError: Cannot assign array to property S::$b of type bool
TypeError: Cannot assign array to property S::$u of type string|int
array(1) {
  [0]=>
  int(1)
}
array(1) {
  [0]=>
  int(1)
}
TypeError: Cannot assign array to property S::$o of type object
array(1) {
  [0]=>
  int(1)
}
array(1) {
  [0]=>
  int(1)
}
array(1) {
  [0]=>
  int(1)
}
TypeError: Cannot assign array to property S::$num of type int|float
TypeError: Cannot assign array to property S::$i of type int
-- val 8
TypeError: Cannot assign stdClass to property S::$i of type int
TypeError: Cannot assign stdClass to property S::$ni of type ?int
TypeError: Cannot assign stdClass to property S::$f of type float
TypeError: Cannot assign stdClass to property S::$s of type string
TypeError: Cannot assign stdClass to property S::$b of type bool
TypeError: Cannot assign stdClass to property S::$u of type string|int
TypeError: Cannot assign stdClass to property S::$a of type array
TypeError: Cannot assign stdClass to property S::$na of type ?array
object(stdClass)#1 (0) {
}
TypeError: Cannot assign stdClass to property S::$it of type Traversable|array
object(stdClass)#1 (0) {
}
object(stdClass)#1 (0) {
}
TypeError: Cannot assign stdClass to property S::$num of type int|float
TypeError: Cannot assign stdClass to property S::$i of type int
int(3)
int(3)
bool(true)
Error: Cannot modify private(set) property S::$ps from global scope
int(7)
int(9)
int(11)
array(2) {
  [0]=>
  int(6)
  [1]=>
  int(12)
}
TypeError: Cannot increment a reference held by property S::$i of type int past its maximal value
int(6)
int(7)
float(2.5)
float(9.223372036854776E+18)

Deprecated: Increment on non-numeric string is deprecated, use str_increment() instead in Standard input code on line 26
string(1) "b"
int(1)
[destruct first]
assigned
[destruct second]
nulled
NULL
[destruct p1]
replaced
[destruct p2]
zeroed
int(0)
array(2) {
  [0]=>
  int(41)
  [1]=>
  int(41)
}
TypeError: Cannot assign string to property S::$i of type int
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(2) {
  [0]=>
  array(1) {
    [0]=>
    int(1)
  }
  [1]=>
  array(2) {
    [0]=>
    int(2)
    [1]=>
    int(3)
  }
}
array(4) {
  [0]=>
  int(2)
  [1]=>
  int(3)
  [2]=>
  int(4)
  ["k"]=>
  int(5)
}
bool(true)
TypeError: Cannot assign stdClass to property S::$self of type ?S
int(1)
TypeError: Cannot assign string to property S::$c of type ?Closure
array(3) {
  [0]=>
  int(4)
  [1]=>
  string(2) "x3"
  [2]=>
  array(1) {
    [0]=>
    int(3)
  }
}
end
"##;
