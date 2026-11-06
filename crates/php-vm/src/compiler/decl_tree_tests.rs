//! Ownership of compiled declarations: an op array's table holds only the
//! declarations nested in it (php-src's `dynamic_func_defs`), so no
//! declared function's code refers back to a table that holds the function,
//! and dropping a unit frees every function, closure and class compiled in
//! it.

use super::{compile, CompileOptions, CompiledUnit};
use crate::func::UserFunction;
use crate::opcode::{Op, OpArray};
use std::rc::{Rc, Weak};

const SRC: &str = "<?php
function f() {
    $c = function () { return fn() => 1; };
    function g() {}
    class Inner { function m() { return function () {}; } }
}
class C {
    const K = 1;
    function m() { return static function () {}; }
}
if (true) { function cond() {} }
$h = function () {};
";

fn compile_src(src: &str) -> CompiledUnit {
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    let opts = CompileOptions {
        filename: "t.php".into(),
        ..CompileOptions::default()
    };
    compile(&program, Rc::from(src.as_bytes()), opts).expect("compile")
}

/// Every user function of the unit: its table lists the nested ones too,
/// and the methods hang off the class declarations.
fn functions(unit: &CompiledUnit) -> Vec<Rc<UserFunction>> {
    let mut out: Vec<Rc<UserFunction>> = unit.decls.functions.borrow().clone();
    for class in unit.decls.classes.borrow().iter() {
        out.extend(class.methods.iter().map(|m| m.func.clone()));
    }
    out
}

/// The declaration ops of `code` index its own table.
fn assert_ops_index_own_table(code: &OpArray, what: &str) {
    let functions = code.decls.functions.borrow().len();
    let classes = code.decls.classes.borrow().len();
    for op in &code.ops {
        match op {
            Op::DeclareFunction(i) | Op::MakeClosure { idx: i, .. } => {
                assert!((*i as usize) < functions, "{what}: {op:?}");
            }
            Op::DeclareClass(i) | Op::DeclareAnonClass { idx: i, .. } => {
                assert!((*i as usize) < classes, "{what}: {op:?}");
            }
            _ => {}
        }
    }
}

#[test]
fn nested_op_arrays_own_their_declarations() {
    let unit = compile_src(SRC);
    assert!(Rc::ptr_eq(&unit.main.code.decls, &unit.decls));
    assert_ops_index_own_table(&unit.main.code, "main");
    let all = functions(&unit);
    // f, $c, its arrow function, g, Inner::m's closure, C::m's closure,
    // cond, $h; and the methods Inner::m, C::m.
    assert_eq!(all.len(), 10, "{all:?}");
    for func in &all {
        assert!(
            !Rc::ptr_eq(&func.code.decls, &unit.decls),
            "{} refers to the unit's table",
            func.name
        );
        assert_ops_index_own_table(&func.code, &func.name.to_string_lossy());
    }
    let f = all.iter().find(|u| u.name.as_bytes() == b"f").expect("f");
    let names: Vec<String> = f
        .code
        .decls
        .functions
        .borrow()
        .iter()
        .map(|u| u.name.to_string_lossy())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names[0].starts_with("{closure:"), "{names:?}");
    assert_eq!(names[1], "g");
    let classes = f.code.decls.classes.borrow();
    assert_eq!(classes.len(), 1);
    assert_eq!(classes[0].name.as_bytes(), b"Inner");
    // The closure's table holds only its arrow function.
    let closure = &f.code.decls.functions.borrow()[0];
    assert_eq!(closure.code.decls.functions.borrow().len(), 1);
    // `g()` declares nothing: no table of its own to allocate.
    let g = &f.code.decls.functions.borrow()[1];
    assert!(g.code.decls.functions.borrow().is_empty());
}

#[test]
fn dropping_a_unit_frees_its_declarations() {
    let unit = compile_src(SRC);
    let weak_functions: Vec<Weak<UserFunction>> =
        functions(&unit).iter().map(Rc::downgrade).collect();
    let classes: Vec<_> = unit
        .decls
        .classes
        .borrow()
        .iter()
        .map(Rc::downgrade)
        .collect();
    let table = Rc::downgrade(&unit.decls);
    let main = Rc::downgrade(&unit.main.code);
    assert_eq!(classes.len(), 2);
    drop(unit);
    assert!(table.upgrade().is_none(), "the unit's table leaked");
    assert!(main.upgrade().is_none(), "the main op array leaked");
    for (i, w) in weak_functions.iter().enumerate() {
        assert!(w.upgrade().is_none(), "function #{i} leaked");
    }
    for (i, w) in classes.iter().enumerate() {
        assert!(w.upgrade().is_none(), "class #{i} leaked");
    }
}
