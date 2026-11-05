use php_runtime::{Value, ZStr};
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
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn literals_and_folded_initializers_are_interned() {
    assert_eq!(
        run(r#"<?php
$s = 'literal';
debug_zval_dump($s, 'fold' . 'ed', ["nested", ["deep"]]);
const C = 'constant';
class A { public $s = 'property'; }
function f($s = 'default') { debug_zval_dump($s); }
debug_zval_dump(C, (new A)->s);
f();
$s[0] = 'L';
echo $s, "\n";
debug_zval_dump('literal');
"#),
        "string(7) \"literal\" interned\n\
         string(6) \"folded\" interned\n\
         array(2) packed refcount(2){\n  [0]=>\n  string(6) \"nested\" refcount(1)\n  [1]=>\n  array(1) packed refcount(1){\n    [0]=>\n    string(4) \"deep\" refcount(1)\n  }\n}\n\
         string(8) \"constant\" interned\nstring(8) \"property\" interned\n\
         string(7) \"default\" interned\nLiteral\nstring(7) \"literal\" interned\n"
    );
}

#[test]
fn bare_string_adapters_share_unchanged_storage() {
    let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
    for input in [
        ZStr::new(b"binary\0\xff".to_vec()),
        ZStr::new(b"literal".to_vec()),
        ZStr::intern(b"literal"),
    ] {
        let repeated = vm
            .call_function("str_repeat", vec![Value::Str(input.clone()), Value::Int(1)])
            .expect("str_repeat");
        let Value::Str(repeated) = repeated else {
            panic!("string result");
        };
        assert!(ZStr::ptr_eq(&input, &repeated));
        assert_eq!(input.is_interned(), repeated.is_interned());
        let escaped = vm
            .call_function(
                "htmlspecialchars",
                vec![
                    Value::Str(input.clone()),
                    Value::Int(0),
                    Value::Str(ZStr::from("ISO-8859-1")),
                ],
            )
            .expect("htmlspecialchars");
        let Value::Str(escaped) = escaped else {
            panic!("string result");
        };
        assert!(ZStr::ptr_eq(&input, &escaped));
    }
}

#[test]
fn repeat_empty_and_zero_results_are_interned_but_negative_counts_fail() {
    assert_eq!(
        run(r#"<?php
debug_zval_dump(str_repeat('abc', 1), str_repeat('abc', 0), str_repeat('', 2));
try { str_repeat('', -1); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
"#),
        "string(3) \"abc\" interned\nstring(0) \"\" interned\nstring(0) \"\" interned\n\
         str_repeat(): Argument #2 ($times) must be greater than or equal to 0\n"
    );
}
