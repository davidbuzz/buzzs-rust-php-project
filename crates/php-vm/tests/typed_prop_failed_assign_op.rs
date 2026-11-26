// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A compound assignment to a typed property or a typed reference whose
//! operation throws (`$o->i -= []`, `$o->i %= 0`): php-src operates into a
//! temporary (`zend_binary_assign_op_typed_prop`,
//! `zend_binary_assign_op_typed_ref`, Zend/zend_execute.c), which the
//! failure leaves UNDEF, and the weak-mode check of that temporary stores
//! its conversion: int 0 where the type takes an int, float 0 where it
//! takes a float. Untyped places stay as they were. The code runs in a
//! function called three times, so the cached and generic paths both run.
//! Expected output from `/usr/bin/php8.5 -n`.
mod common;

use php_vm::VmOptions;
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

/// `$o->p op= v` whose operation throws, for each property type, on a
/// typed static property, through a reference, and for `%`, `<<`, `**`
/// and `|`: int, ?int, int|string, mixed and int|float end up int 0, float
/// float 0, string, bool and array keep their value, and `.=` on a string
/// extends it in place.
#[test]
fn each_property_type_after_a_failed_operation() {
    assert_eq!(
        run(r###"<?php
class T{public int $i=-1; public ?int $ni=-1; public float $f=-1.5; public string $s='q'; public bool $b=true; public ?string $ns='q'; public int|string $u=-1; public array $a=[1]; public ?array $na=[1]; public mixed $m=-1; public int|float $if = -1; public static int $si=-1;}
function f() {
$t=new T;
foreach (['i','ni','f','s','b','ns','u','a','na','m','if'] as $p) {
  try { $t->$p -= []; } catch (Throwable $e) { echo $p, ': ', get_class($e), ' ', $e->getMessage(), ' prev=', $e->getPrevious() ? $e->getPrevious()->getMessage() : '-', "\n"; } var_dump($t->$p);
}
try { T::$si *= []; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump(T::$si);
$r = &$t->i; $t->i = -1; try { $r += []; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump($t->i);
$t->s = 'q'; try { $t->s .= []; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump($t->s);
$t->i = 5; try { $t->i %= 0; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump($t->i);
$t->i = 5; try { $t->i <<= -1; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump($t->i);
$t->i = 5; try { $t->i **= []; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump($t->i);
$t->i = 5; try { $t->i |= "a"; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump($t->i);
}
f(); f(); f();
"###),
        r###"i: TypeError Unsupported operand types: int - array prev=-
int(0)
ni: TypeError Unsupported operand types: int - array prev=-
int(0)
f: TypeError Unsupported operand types: float - array prev=-
float(0)
s: TypeError Unsupported operand types: string - array prev=-
string(1) "q"
b: TypeError Unsupported operand types: bool - array prev=-
bool(true)
ns: TypeError Unsupported operand types: string - array prev=-
string(1) "q"
u: TypeError Unsupported operand types: int - array prev=-
int(0)
a: TypeError Unsupported operand types: array - array prev=-
array(1) {
  [0]=>
  int(1)
}
na: TypeError Unsupported operand types: array - array prev=-
array(1) {
  [0]=>
  int(1)
}
m: TypeError Unsupported operand types: int - array prev=-
int(0)
if: TypeError Unsupported operand types: int - array prev=-
int(0)
Unsupported operand types: int * array
int(0)
Unsupported operand types: int + array
int(0)

Warning: Array to string conversion in t.php on line 10
string(6) "qArray"
Modulo by zero
int(0)
Bit shift by negative number
int(0)
Unsupported operand types: int ** array
int(0)
Unsupported operand types: int | string
int(0)
i: TypeError Unsupported operand types: int - array prev=-
int(0)
ni: TypeError Unsupported operand types: int - array prev=-
int(0)
f: TypeError Unsupported operand types: float - array prev=-
float(0)
s: TypeError Unsupported operand types: string - array prev=-
string(1) "q"
b: TypeError Unsupported operand types: bool - array prev=-
bool(true)
ns: TypeError Unsupported operand types: string - array prev=-
string(1) "q"
u: TypeError Unsupported operand types: int - array prev=-
int(0)
a: TypeError Unsupported operand types: array - array prev=-
array(1) {
  [0]=>
  int(1)
}
na: TypeError Unsupported operand types: array - array prev=-
array(1) {
  [0]=>
  int(1)
}
m: TypeError Unsupported operand types: int - array prev=-
int(0)
if: TypeError Unsupported operand types: int - array prev=-
int(0)
Unsupported operand types: int * array
int(0)
Unsupported operand types: int + array
int(0)

Warning: Array to string conversion in t.php on line 10
string(6) "qArray"
Modulo by zero
int(0)
Bit shift by negative number
int(0)
Unsupported operand types: int ** array
int(0)
Unsupported operand types: int | string
int(0)
i: TypeError Unsupported operand types: int - array prev=-
int(0)
ni: TypeError Unsupported operand types: int - array prev=-
int(0)
f: TypeError Unsupported operand types: float - array prev=-
float(0)
s: TypeError Unsupported operand types: string - array prev=-
string(1) "q"
b: TypeError Unsupported operand types: bool - array prev=-
bool(true)
ns: TypeError Unsupported operand types: string - array prev=-
string(1) "q"
u: TypeError Unsupported operand types: int - array prev=-
int(0)
a: TypeError Unsupported operand types: array - array prev=-
array(1) {
  [0]=>
  int(1)
}
na: TypeError Unsupported operand types: array - array prev=-
array(1) {
  [0]=>
  int(1)
}
m: TypeError Unsupported operand types: int - array prev=-
int(0)
if: TypeError Unsupported operand types: int - array prev=-
int(0)
Unsupported operand types: int * array
int(0)
Unsupported operand types: int + array
int(0)

Warning: Array to string conversion in t.php on line 10
string(6) "qArray"
Modulo by zero
int(0)
Bit shift by negative number
int(0)
Unsupported operand types: int ** array
int(0)
Unsupported operand types: int | string
int(0)
"###
    );
}

/// A typed reference: a string or bool type refuses the UNDEF result
/// with the reference's own `TypeError`, chained onto the operation's;
/// two int-taking sources agree on 0. Division and modulo by zero, a
/// non-numeric string and an object operand fail the same way; `.=`
/// fails on the type check afterwards and `??=` does not operate.
#[test]
fn typed_references_after_a_failed_operation() {
    assert_eq!(
        run(r###"<?php
class T{public int $i=-1; public float $f=-1.5; public string $s='q'; public ?bool $b=true; public int|float $if=-1;}
function p($e) { echo get_class($e), ': ', $e->getMessage(); for ($q = $e->getPrevious(); $q; $q = $q->getPrevious()) echo ' <- ', get_class($q), ': ', $q->getMessage(); echo "\n"; }
function f() {
$t=new T;
$r = &$t->s; try { $r -= []; } catch (Throwable $e) { p($e); } var_dump($t->s);
$r2 = &$t->b; try { $r2 -= []; } catch (Throwable $e) { p($e); } var_dump($t->b);
$r3 = &$t->f; try { $r3 -= []; } catch (Throwable $e) { p($e); } var_dump($t->f);
$t2 = new T; $r4 = &$t2->i; $t2->if = &$r4; try { $r4 -= []; } catch (Throwable $e) { p($e); } var_dump($t2->i);
try { $t->i /= 0; } catch (Throwable $e) { p($e); } var_dump($t->i);
$t->f = 2.5; try { $t->f %= 0; } catch (Throwable $e) { p($e); } var_dump($t->f);
$t->i = 3; try { $t->i .= []; } catch (Throwable $e) { p($e); } var_dump($t->i);
$t->i = 3; try { $t->i += "x"; } catch (Throwable $e) { p($e); } var_dump($t->i);
$t->i = 3; try { $t->i += new stdClass; } catch (Throwable $e) { p($e); } var_dump($t->i);
$t->i = 3; try { $t->i ??= 5; } catch (Throwable $e) { p($e); } var_dump($t->i);
}
f(); f(); f();
"###),
        r###"TypeError: Cannot assign null to reference held by property T::$s of type string <- TypeError: Unsupported operand types: string - array
string(1) "q"
TypeError: Cannot assign null to reference held by property T::$b of type ?bool <- TypeError: Unsupported operand types: bool - array
bool(true)
TypeError: Unsupported operand types: float - array
float(0)
TypeError: Unsupported operand types: int - array
int(0)
DivisionByZeroError: Division by zero
int(0)

Deprecated: Implicit conversion from float 2.5 to int loses precision in t.php on line 11
DivisionByZeroError: Modulo by zero
float(0)

Warning: Array to string conversion in t.php on line 12
TypeError: Cannot assign string to property T::$i of type int
int(3)
TypeError: Unsupported operand types: int + string
int(0)
TypeError: Unsupported operand types: int + stdClass
int(0)
int(3)
TypeError: Cannot assign null to reference held by property T::$s of type string <- TypeError: Unsupported operand types: string - array
string(1) "q"
TypeError: Cannot assign null to reference held by property T::$b of type ?bool <- TypeError: Unsupported operand types: bool - array
bool(true)
TypeError: Unsupported operand types: float - array
float(0)
TypeError: Unsupported operand types: int - array
int(0)
DivisionByZeroError: Division by zero
int(0)

Deprecated: Implicit conversion from float 2.5 to int loses precision in t.php on line 11
DivisionByZeroError: Modulo by zero
float(0)

Warning: Array to string conversion in t.php on line 12
TypeError: Cannot assign string to property T::$i of type int
int(3)
TypeError: Unsupported operand types: int + string
int(0)
TypeError: Unsupported operand types: int + stdClass
int(0)
int(3)
TypeError: Cannot assign null to reference held by property T::$s of type string <- TypeError: Unsupported operand types: string - array
string(1) "q"
TypeError: Cannot assign null to reference held by property T::$b of type ?bool <- TypeError: Unsupported operand types: bool - array
bool(true)
TypeError: Unsupported operand types: float - array
float(0)
TypeError: Unsupported operand types: int - array
int(0)
DivisionByZeroError: Division by zero
int(0)

Deprecated: Implicit conversion from float 2.5 to int loses precision in t.php on line 11
DivisionByZeroError: Modulo by zero
float(0)

Warning: Array to string conversion in t.php on line 12
TypeError: Cannot assign string to property T::$i of type int
int(3)
TypeError: Unsupported operand types: int + string
int(0)
TypeError: Unsupported operand types: int + stdClass
int(0)
int(3)
"###
    );
}

/// Strict mode accepts no UNDEF: every property keeps its value, and a
/// typed reference throws its `TypeError`.
#[test]
fn strict_mode_keeps_the_value() {
    assert_eq!(
        run(r###"<?php
declare(strict_types=1);
class T{public int $i=-1; public float $f=-1.5; public mixed $m=-1; public int|float $if=-1; public static int $si=-1;}
function f() {
$t=new T;
foreach (['i','f','m','if'] as $p) { try { $t->$p -= []; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump($t->$p); }
$t->i = 5; try { $t->i %= 0; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump($t->i);
try { T::$si *= []; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump(T::$si);
$r = &$t->i; try { $r -= []; } catch (Throwable $e) { echo $e->getMessage(), ' <- ', $e->getPrevious()?->getMessage(), "\n"; } var_dump($t->i);
}
f(); f(); f();
"###),
        r###"Unsupported operand types: int - array
int(-1)
Unsupported operand types: float - array
float(-1.5)
Unsupported operand types: int - array
int(-1)
Unsupported operand types: int - array
int(-1)
Modulo by zero
int(5)
Unsupported operand types: int * array
int(-1)
Cannot assign null to reference held by property T::$i of type int <- Unsupported operand types: int - array
int(5)
Unsupported operand types: int - array
int(-1)
Unsupported operand types: float - array
float(-1.5)
Unsupported operand types: int - array
int(-1)
Unsupported operand types: int - array
int(-1)
Modulo by zero
int(5)
Unsupported operand types: int * array
int(-1)
Cannot assign null to reference held by property T::$i of type int <- Unsupported operand types: int - array
int(5)
Unsupported operand types: int - array
int(-1)
Unsupported operand types: float - array
float(-1.5)
Unsupported operand types: int - array
int(-1)
Unsupported operand types: int - array
int(-1)
Modulo by zero
int(5)
Unsupported operand types: int * array
int(-1)
Cannot assign null to reference held by property T::$i of type int <- Unsupported operand types: int - array
int(5)
"###
    );
}

/// A dynamic property bound to a typed property follows the reference's
/// types; an untyped dynamic property keeps its value.
#[test]
fn dynamic_property_holding_a_typed_reference() {
    assert_eq!(
        run(r###"<?php
class T { public int $i = -1; public float $f = 2.0; }
function f() {
    $t = new T;
    $o = new stdClass;
    $o->p = &$t->i;
    $t->i = 4;
    try { $o->p -= []; } catch (Throwable $e) { echo $e->getMessage(), "\n"; }
    var_dump($t->i);
    $o->q = &$t->f;
    try { $o->q /= 0; } catch (Throwable $e) { echo $e->getMessage(), "\n"; }
    var_dump($t->f);
    $o->r = 7;
    try { $o->r -= []; } catch (Throwable $e) { echo $e->getMessage(), "\n"; }
    var_dump($o->r);
}
f(); f(); f();
"###),
        r###"Unsupported operand types: int - array
int(0)
Division by zero
float(0)
Unsupported operand types: int - array
int(7)
Unsupported operand types: int - array
int(0)
Division by zero
float(0)
Unsupported operand types: int - array
int(7)
Unsupported operand types: int - array
int(0)
Division by zero
float(0)
Unsupported operand types: int - array
int(7)
"###
    );
}
