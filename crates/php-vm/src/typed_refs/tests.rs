// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Script-level tests of typed references (expected output from
//! `/usr/bin/php -n`).

#![allow(clippy::needless_raw_string_hashes)]

use crate::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run(src: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(src.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn coercion_possible_rules() {
    use super::{coercion_possible, Assignable};
    use crate::types::mask;
    use php_runtime::Value;
    assert_eq!(
        coercion_possible(mask::DOUBLE, &Value::Int(1), true),
        Assignable::Coerce
    );
    assert_eq!(
        coercion_possible(mask::LONG, &Value::Float(1.0), true),
        Assignable::No
    );
    assert_eq!(
        coercion_possible(mask::LONG, &Value::Null, false),
        Assignable::No
    );
    assert_eq!(
        coercion_possible(mask::ARRAY, &Value::Int(1), false),
        Assignable::No
    );
    assert_eq!(
        coercion_possible(mask::BOOL, &Value::Int(1), false),
        Assignable::Coerce
    );
    assert_eq!(
        coercion_possible(mask::TRUE, &Value::Int(1), false),
        Assignable::No
    );
    assert_eq!(
        coercion_possible(mask::STRING, &Value::Int(1), false),
        Assignable::Coerce
    );
}

#[test]
fn instance_property_references() {
    let src = r#"<?php
class A { public int $i = 1; public string|int $u = 0; public ?float $f = null; public $plain; }
$a = new A;
$r = &$a->i;
$r = "42"; var_dump($a->i);
try { $r = "x"; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$r = 7.0; var_dump($a->i);
unset($a->i);
$r = "free"; var_dump($r);
$b = new A;
$b->u = 5;
$x = &$b->u;
$b->f = 1.5;
try { $b->f = &$b->u; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$c = new A;
$c->plain = &$c->i;
$c->plain++;
var_dump($c->i);
$c->i = PHP_INT_MAX;
try { $c->plain++; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
var_dump($c->i);
$d = clone $c;
try { $c->plain = []; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
unset($c);
try { $d->plain = []; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
"#;
    assert_eq!(
        run(src),
        r#"int(42)
Cannot assign string to reference held by property A::$i of type int
int(7)
string(4) "free"
Reference with value of type int held by property A::$u of type string|int is not compatible with property A::$f of type ?float
int(2)
Cannot increment a reference held by property A::$i of type int past its maximal value
int(9223372036854775807)
Cannot assign array to reference held by property A::$i of type int
Cannot assign array to reference held by property A::$i of type int
"#
    );
}

#[test]
fn multiple_sources_statics_and_internal_writes() {
    let src = r#"<?php
class B { public int|string $x = "1"; public float|string $y = "1"; public ?int $n; public int $m; public static ?int $s = 1; public static int $t; }
$b = new B;
$r = &$b->x; $b->y = &$r;
try { $r = 1.0; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$r = "s"; var_dump($b->x, $b->y);
$n = &$b->n; var_dump($n);
try { $n[] = 1; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { $m = &$b->m; } catch (Error $e) { echo $e->getMessage(), "\n"; }
$s = &B::$s; $s = "5"; var_dump(B::$s);
try { $s = "x"; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { var_dump(B::$t); } catch (Error $e) { echo $e->getMessage(), "\n"; }
var_dump(isset(B::$t));
B::$t = "3"; var_dump(B::$t);
try { settype($n, "array"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
settype($n, "integer"); var_dump($b->n);
try { is_callable('strlen', false, $b->n); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
var_dump($b->n);
function f(int &$p) { $p = "9"; }
f($b->n); var_dump($b->n);
"#;
    assert_eq!(
        run(src),
        r#"Cannot assign float to reference held by property B::$x of type string|int and property B::$y of type string|float, as this would result in an inconsistent type conversion
string(1) "s"
string(1) "s"
NULL
Cannot auto-initialize an array inside a reference held by property B::$n of type ?int
Cannot access uninitialized non-nullable property B::$m by reference
int(5)
Cannot assign string to reference held by property B::$s of type ?int
Typed static property B::$t must not be accessed before initialization
bool(false)
int(3)
Cannot assign array to reference held by property B::$n of type ?int
int(0)
Cannot assign string to reference held by property B::$n of type ?int
int(0)
int(9)
"#
    );
}

fn int_prop(name: &str) -> Rc<crate::class::PropInfo> {
    use crate::types::{mask, TypeDecl};
    Rc::new(crate::class::PropInfo {
        name: php_runtime::ZStr::from(name),
        slot: 0,
        flags: 0,
        ty: Some(TypeDecl::simple(mask::LONG)),
        class: php_runtime::ZStr::from("C"),
        default: None,
        doc_comment: None,
        attributes: Vec::new(),
        hooks: None,
    })
}

#[test]
fn sources_follow_their_holders() {
    use super::{add_source, auto_init_error, has_type_sources, type_sources, Holder};
    use php_runtime::{PhpRef, Value};
    let cell = Rc::new(RefCell::new(Value::Null));
    let other = Rc::new(RefCell::new(Value::Null));
    let r = PhpRef::new(Value::Null);
    *cell.borrow_mut() = Value::Reference(r.clone());
    *other.borrow_mut() = Value::Reference(r.clone());
    add_source(
        &r,
        Holder::Static(Rc::downgrade(&cell)),
        int_prop("a"),
        None,
    );
    add_source(
        &r,
        Holder::Static(Rc::downgrade(&other)),
        int_prop("b"),
        None,
    );
    // Re-adding a holder keeps its position.
    add_source(
        &r,
        Holder::Static(Rc::downgrade(&cell)),
        int_prop("a"),
        None,
    );
    let names: Vec<String> = type_sources(&r)
        .iter()
        .map(super::SourceProp::qualified_name)
        .collect();
    assert_eq!(names, ["C::$a", "C::$b"]);
    assert_eq!(
        auto_init_error(&r).as_deref(),
        Some(
            "Cannot auto-initialize an array inside a reference held by property C::$a of type int"
        )
    );
    // Unbinding one holder drops its source; dropping the other holder
    // leaves an untyped reference.
    *cell.borrow_mut() = Value::Null;
    assert_eq!(type_sources(&r).len(), 1);
    drop(other);
    assert!(!has_type_sources(&r));
    assert!(r.meta().is_none());
}

#[test]
fn incdec_guard_scopes_overflow() {
    use super::{incdec_overflow, IncDecGuard};
    use php_runtime::Value;
    assert_eq!(incdec_overflow(), None);
    {
        let _g = IncDecGuard::new(true, &Value::Int(i64::MAX), &Value::Float(1.0));
        assert_eq!(incdec_overflow(), Some(true));
        {
            let _inner = IncDecGuard::new(false, &Value::Int(1), &Value::Int(0));
            assert_eq!(incdec_overflow(), None);
        }
        assert_eq!(incdec_overflow(), Some(true));
    }
    assert_eq!(incdec_overflow(), None);
}
