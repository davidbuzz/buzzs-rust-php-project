// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Consistency of the per-class member tables (`Class::methods`,
//! `const_map`, `static_map`, `prop_map`: php-src's `function_table`,
//! `constants_table` and `properties_info`) with their declaration-order
//! lists, for internal classes and linked user classes alike, and the member
//! orders PHP shows (`get_class_methods`, `ReflectionClass::getConstants()`),
//! which come from those lists and not from the tables' iteration order.
use php_vm::class::{Class, NameMap};
use php_vm::func::Method;
use php_vm::{Vm, VmOptions};
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

/// Inheritance, an interface, a trait and an enum whose cases and constant
/// interleave; the printed orders are those of php-src (8.5 and master
/// agree on every line). The bare VM has no ext/standard array functions
/// (`array_keys`, `array_map`), so the keys and names are collected with
/// `foreach`.
const SCRIPT: &str = "<?php
interface I { const IC = 1; function i(); }
trait T { public static $ts = 1; const TC = 2; public $tp = 3; function t() {} abstract function ta(); }
abstract class P implements I { const PC = 3; public $pp = 1; private $priv = 2; protected static $ps = 5; function i() {} function p() {} private function pp() {} }
class C extends P { const CC = 4; public $cp = 5; public $pp = 6; function c() {} function i() {} }
class U { use T; const UC = 5; function ta() {} function u() {} }
enum E: string implements I {
    case A = 'a';
    const EC = 1;
    case B = 'b';
    function i() {}
}
function keys($a) { $k = []; foreach ($a as $n => $_) { $k[] = $n; } return implode(',', $k); }
function names($a) { $k = []; foreach ($a as $p) { $k[] = $p->name; } return implode(',', $k); }
echo implode(',', get_class_methods('C')), \"\\n\";
echo implode(',', get_class_methods('U')), \"\\n\";
echo keys((new ReflectionClass('C'))->getConstants()), \"\\n\";
echo keys((new ReflectionClass('U'))->getConstants()), \"\\n\";
echo keys((new ReflectionClass('E'))->getConstants()), \"\\n\";
echo names((new ReflectionClass('C'))->getProperties()), \"\\n\";
echo C::IC + C::PC + C::CC + U::TC + U::UC, \"\\n\";
";

const EXPECTED: &str = "c,i,p\nta,u,t\nCC,PC,IC\nUC,TC\nA,EC,B,IC\ncp,pp,ps\n15\n";

/// Every table entry is reachable through both key forms and agrees with
/// the declaration-order list it indexes.
fn check_tables(cls: &Rc<Class>) {
    // The public alias names the field's type.
    let methods: &NameMap<Rc<Method>> = &cls.methods;
    assert_eq!(methods.len(), cls.method_order.len(), "{cls:?}");
    for lc in &cls.method_order {
        let m = methods
            .get(lc)
            .unwrap_or_else(|| panic!("{cls:?}::{lc}() is in the order list but not in the table"));
        assert!(m.name.eq_ignore_ascii_case(lc.as_bytes()), "{cls:?}::{lc}");
        let by_bytes = methods.get(lc.as_bytes()).expect("lookup by bytes");
        assert!(Rc::ptr_eq(m, by_bytes), "{cls:?}::{lc}");
        let found = cls.find_method(lc.as_bytes()).expect("find_method");
        assert!(Rc::ptr_eq(m, found), "{cls:?}::{lc}");
    }
    assert_eq!(cls.const_map.len(), cls.consts.len(), "{cls:?}");
    for (i, c) in cls.consts.iter().enumerate() {
        assert_eq!(cls.const_map.get(&c.name), Some(&i), "{cls:?}::{}", c.name);
        let found = cls.find_const(c.name.as_bytes()).expect("find_const");
        assert!(Rc::ptr_eq(found, c), "{cls:?}::{}", c.name);
    }
    assert_eq!(cls.static_map.len(), cls.static_props.len(), "{cls:?}");
    for (i, sp) in cls.static_props.iter().enumerate() {
        assert_eq!(
            cls.static_map.get(&sp.name),
            Some(&i),
            "{cls:?}::${}",
            sp.name
        );
        let found = cls.find_static(sp.name.as_bytes()).expect("find_static");
        assert_eq!(found.name, sp.name, "{cls:?}");
    }
    for (name, pi) in &cls.prop_map {
        assert_eq!(*name, pi.name, "{cls:?}");
        let slot = usize::try_from(pi.slot).expect("slot index");
        let in_slot = cls
            .props
            .get(slot)
            .unwrap_or_else(|| panic!("{cls:?}::${name}: slot {slot} of {}", cls.props.len()));
        assert!(Rc::ptr_eq(in_slot, pi), "{cls:?}::${name} slot {slot}");
    }
}

#[test]
fn linked_tables_are_consistent_and_ordered_like_php() {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(SCRIPT.as_bytes(), "t.php");
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    assert_eq!(output, EXPECTED);
    for name in ["I", "T", "P", "C", "U", "E"] {
        let cls = vm
            .find_class(name.as_bytes())
            .unwrap_or_else(|| panic!("class {name}"));
        check_tables(&cls);
    }
    // The same invariants hold for every internal class, whichever way it
    // was registered (`ClassBuilder`, hand-built reflection classes, the
    // zend_test patches).
    let internal: Vec<Rc<Class>> = vm.classes.values().cloned().collect();
    assert!(internal.len() >= 50, "{}", internal.len());
    for core in [
        "stdClass",
        "Exception",
        "Closure",
        "ReflectionClass",
        "PropertyHookType",
    ] {
        assert!(vm.find_class(core.as_bytes()).is_some(), "{core}");
    }
    for cls in &internal {
        check_tables(cls);
    }
}

#[test]
fn user_class_tables_find_members_by_either_key_form() {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink));
    assert_eq!(
        vm.run_script(
            b"<?php class K { const N = 1; public static $s = 2; public $p = 3; function getP() { return $this->p; } }",
            "t.php"
        ),
        0
    );
    let k = vm.find_class(b"K").expect("class K");
    let m = k.find_method(b"GETP").expect("method by any case");
    assert_eq!(m.name.as_bytes(), b"getP");
    assert!(Rc::ptr_eq(m, &k.methods[b"getp".as_slice()]));
    assert!(Rc::ptr_eq(
        m,
        k.methods
            .get(&php_runtime::ZStr::from("getp"))
            .expect("by ZStr")
    ));
    assert!(k.find_method(b"nope").is_none());
    assert!(k.find_const(b"N").is_some());
    // Constant names are case-sensitive.
    assert!(k.find_const(b"n").is_none());
    assert_eq!(
        k.find_static(b"s").map(|sp| sp.name.as_bytes().to_vec()),
        Some(b"s".to_vec())
    );
    assert_eq!(k.prop_map.get(b"p".as_slice()).map(|p| p.slot), Some(0));
}
