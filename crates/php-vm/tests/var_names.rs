// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Variable-table names shared with the op array (`op_array->vars`): a
//! frame copies the compiled-variable names only when a variable is created
//! at run time. Expected PHP outputs are php-src's (`/usr/bin/php8.5 -n`).
use php_runtime::{Value, ZStr};
use php_vm::opcode::OpArray;
use php_vm::vm::{Frame, FrameKind, VarStore};
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

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    let output = String::from_utf8(output).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

fn code(names: &[&str]) -> Rc<OpArray> {
    // Set the field on a default: the functional-update syntax cannot name
    // the crate-private fields (`tape` and the caches) from outside.
    let mut code = OpArray::default();
    code.cv_names = names.iter().map(|n| ZStr::from(*n)).collect();
    Rc::new(code)
}

#[test]
fn a_frame_shares_the_op_arrays_cv_names_until_a_variable_is_added() {
    let code = code(&["a", "b"]);
    let f = Frame::new(code.clone(), FrameKind::Function);
    {
        let vars = f.vars().borrow();
        assert_eq!(vars.slots.len(), 2);
        assert_eq!(vars.names.len(), 2);
        assert_eq!(vars.names[1].as_bytes(), b"b");
        // Shared: the store holds the op array (this handle, the frame's
        // `code` and the store), not a copy of the names.
        assert_eq!(vars.names.capacity(), 0);
        assert_eq!(Rc::strong_count(&code), 3);
        assert_eq!(format!("{:?}", vars.names), format!("{:?}", code.cv_names));
    }
    // A run-time variable copies the names once; the op array is untouched
    // and no longer held by the store.
    assert_eq!(f.vars().borrow_mut().find_or_add(b"c"), 2);
    let mut vars = f.vars().borrow_mut();
    assert_eq!(vars.slots.len(), 3);
    assert_eq!(vars.names.len(), 3);
    assert_eq!(vars.names[0].as_bytes(), b"a");
    assert_eq!(vars.names[2].as_bytes(), b"c");
    assert!(vars.names.capacity() >= 3);
    assert_eq!(code.cv_names.len(), 2);
    assert_eq!(Rc::strong_count(&code), 2);
    assert_eq!(vars.find(b"a"), Some(0));
    assert_eq!(vars.find(b"c"), Some(2));
    assert_eq!(vars.find(b"d"), None);
    // An existing name does not grow the table.
    assert_eq!(vars.find_or_add(b"a"), 0);
    assert_eq!(vars.slots.len(), 3);
}

#[test]
fn with_names_owns_its_copy() {
    let names = [ZStr::from("x"), ZStr::from("y")];
    let mut st = VarStore::with_names(&names);
    assert_eq!(st.slots.len(), 2);
    assert_eq!(st.names.len(), 2);
    assert_eq!(st.names[0].as_bytes(), b"x");
    assert!(st.names.capacity() >= 2);
    assert_eq!(st.find_or_add(b"z"), 2);
    assert_eq!(st.names.len(), 3);
    st.slots[2] = Some(Value::Int(1));
    // `to_array` pairs names with defined slots only.
    let arr = st.to_array();
    assert_eq!(arr.len(), 1);
    assert!(matches!(
        arr.get(&php_runtime::ArrayKey::from("z")),
        Some(Value::Int(1))
    ));
}

#[test]
fn pooled_frames_share_names_on_reinit_and_drop_them_on_clear() {
    let vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
    let first = code(&["p", "q"]);
    let mut frame = Frame::new_pooled(vm.empty_code.clone());
    assert!(frame.vars().borrow().names.is_empty());
    frame.reinit_for_user(first.clone(), FrameKind::Function);
    {
        let vars = frame.vars().borrow();
        assert_eq!(vars.slots.len(), 2);
        assert_eq!(vars.names.len(), 2);
        assert_eq!(vars.names[0].as_bytes(), b"p");
        assert_eq!(vars.names.capacity(), 0);
    }
    // This handle, the frame's `code` and the store.
    assert_eq!(Rc::strong_count(&first), 3);
    assert_eq!(frame.vars().borrow_mut().find_or_add(b"dyn"), 2);
    assert_eq!(Rc::strong_count(&first), 2);
    frame.clear_for_pool(&vm.empty_code);
    {
        let vars = frame.vars().borrow();
        assert!(vars.slots.is_empty());
        assert!(vars.names.is_empty());
        // The small owned buffer stays for the next run-time copy.
        assert!(vars.names.capacity() >= 1);
    }
    // Nothing in the pool holds the op array.
    assert_eq!(Rc::strong_count(&first), 1);
    // Reused for another function: that one's names, nothing left over.
    let other = code(&["z"]);
    frame.reinit_for_user(other.clone(), FrameKind::Function);
    let vars = frame.vars().borrow();
    assert_eq!(vars.slots.len(), 1);
    assert_eq!(vars.names.len(), 1);
    assert_eq!(vars.names[0].as_bytes(), b"z");
    assert_eq!(Rc::strong_count(&other), 3);
}

#[test]
fn lookups_in_a_large_shared_table_use_the_index() {
    let names: Vec<String> = (0..9).map(|i| format!("v{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let f = Frame::new(code(&refs), FrameKind::Function);
    let mut vars = f.vars().borrow_mut();
    assert_eq!(vars.find(b"v8"), Some(8));
    assert_eq!(vars.find(b"v9"), None);
    assert_eq!(vars.find_or_add(b"v9"), 9);
    assert_eq!(vars.find(b"v9"), Some(9));
    assert_eq!(vars.find(b"v0"), Some(0));
    assert_eq!(vars.names.len(), 10);
    assert_eq!(vars.slots.len(), 10);
}

#[test]
fn dynamic_variables_in_pooled_frames_match_reference() {
    assert_eq!(
        run(r#"<?php
function dyn($n) {
    $a = 1; $b = 2;
    $name = "v$n";
    $$name = $n * 10;
    extract(["e" => $n, "a" => 100]);
    $r = compact("a", "b", "e", $name);
    echo serialize($r), " ", serialize(get_defined_vars()), "\n";
}
for ($i = 0; $i < 3; $i++) { dyn($i); }
function plain($x) { return get_defined_vars(); }
echo serialize(plain(5)), "\n";
function rec($n) { if ($n > 0) { $k = "d$n"; $$k = $n; rec($n - 1); } echo serialize(get_defined_vars()), "\n"; }
rec(2);
function gv() { global $g; $g = 7; $h = &$GLOBALS['g']; return get_defined_vars(); }
echo serialize(gv()), "\n";
function unsetcv($a) { unset($a); $b = 1; return get_defined_vars(); }
echo serialize(unsetcv(1)), "\n";
function ev($x) { eval('$y = $x + 1; $z = "zz";'); echo serialize(get_defined_vars()), "\n"; return $y; }
echo ev(1), ev(2), "\n";
function many($a, $b, $c, $d, $e, $f, $g, $h, $i) { $$a = 1; ${"q$b"} = 2; return serialize(get_defined_vars()); }
echo many("p", 1, 2, 3, 4, 5, 6, 7, 8), "\n";
echo many("r", 1, 2, 3, 4, 5, 6, 7, 8), "\n";
function cl() { $c = function() { $q = 1; $$q = 2; return get_defined_vars(); }; return $c(); }
echo serialize(cl()), serialize(cl()), "\n";
function g1() { $x = 1; yield get_defined_vars(); $y = 2; ${"w"} = 3; yield get_defined_vars(); }
foreach (g1() as $v) echo serialize($v), "\n";
"#),
        r#"a:4:{s:1:"a";i:100;s:1:"b";i:2;s:1:"e";i:0;s:2:"v0";i:0;} a:7:{s:1:"n";i:0;s:1:"a";i:100;s:1:"b";i:2;s:4:"name";s:2:"v0";s:1:"r";a:4:{s:1:"a";i:100;s:1:"b";i:2;s:1:"e";i:0;s:2:"v0";i:0;}s:2:"v0";i:0;s:1:"e";i:0;}
a:4:{s:1:"a";i:100;s:1:"b";i:2;s:1:"e";i:1;s:2:"v1";i:10;} a:7:{s:1:"n";i:1;s:1:"a";i:100;s:1:"b";i:2;s:4:"name";s:2:"v1";s:1:"r";a:4:{s:1:"a";i:100;s:1:"b";i:2;s:1:"e";i:1;s:2:"v1";i:10;}s:2:"v1";i:10;s:1:"e";i:1;}
a:4:{s:1:"a";i:100;s:1:"b";i:2;s:1:"e";i:2;s:2:"v2";i:20;} a:7:{s:1:"n";i:2;s:1:"a";i:100;s:1:"b";i:2;s:4:"name";s:2:"v2";s:1:"r";a:4:{s:1:"a";i:100;s:1:"b";i:2;s:1:"e";i:2;s:2:"v2";i:20;}s:2:"v2";i:20;s:1:"e";i:2;}
a:1:{s:1:"x";i:5;}
a:1:{s:1:"n";i:0;}
a:3:{s:1:"n";i:1;s:1:"k";s:2:"d1";s:2:"d1";i:1;}
a:3:{s:1:"n";i:2;s:1:"k";s:2:"d2";s:2:"d2";i:2;}
a:2:{s:1:"g";i:7;s:1:"h";R:2;}
a:1:{s:1:"b";i:1;}
a:3:{s:1:"x";i:1;s:1:"y";i:2;s:1:"z";s:2:"zz";}
2a:3:{s:1:"x";i:2;s:1:"y";i:3;s:1:"z";s:2:"zz";}
3
a:11:{s:1:"a";s:1:"p";s:1:"b";i:1;s:1:"c";i:2;s:1:"d";i:3;s:1:"e";i:4;s:1:"f";i:5;s:1:"g";i:6;s:1:"h";i:7;s:1:"i";i:8;s:1:"p";i:1;s:2:"q1";i:2;}
a:11:{s:1:"a";s:1:"r";s:1:"b";i:1;s:1:"c";i:2;s:1:"d";i:3;s:1:"e";i:4;s:1:"f";i:5;s:1:"g";i:6;s:1:"h";i:7;s:1:"i";i:8;s:1:"r";i:1;s:2:"q1";i:2;}
a:2:{s:1:"q";i:1;s:1:"1";i:2;}a:2:{s:1:"q";i:1;s:1:"1";i:2;}
a:1:{s:1:"x";i:1;}
a:3:{s:1:"x";i:1;s:1:"y";i:2;s:1:"w";i:3;}
"#
    );
}

#[test]
fn an_include_adds_its_variables_to_the_including_frame() {
    let dir = std::env::temp_dir().join(format!("phprust-var-names-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let inc = dir.join("inc.php");
    std::fs::write(&inc, b"<?php $from_include = $x + 1; $x = 'changed';").expect("include file");
    let script = format!(
        "<?php\nfunction f($x) {{ include '{}'; return get_defined_vars(); }}\necho serialize(f(1)), serialize(f(2)), \"\\n\";\n",
        inc.display()
    );
    let output = run(&script);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        output,
        "a:2:{s:1:\"x\";s:7:\"changed\";s:12:\"from_include\";i:2;}\
         a:2:{s:1:\"x\";s:7:\"changed\";s:12:\"from_include\";i:3;}\n"
    );
}
