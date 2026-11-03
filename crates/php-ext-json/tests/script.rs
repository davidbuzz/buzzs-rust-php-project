//! End-to-end tests: PHP scripts run on a VM with the `json` extension.

#![allow(clippy::needless_raw_string_hashes)]

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use php_vm::ext::Extension;
use php_vm::{Vm, VmOptions};

/// A `Write` sink shared with the test.
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

fn run(code: &str) -> String {
    let sink = Sink::default();
    let ext = php_ext_json::Json;
    let exts: [&dyn Extension; 1] = [&ext];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn encode_flags_and_constants() {
    let out = run(
        r#"echo json_encode(["a" => 1, "b" => [1.0, "é/<"]], JSON_PRESERVE_ZERO_FRACTION | JSON_UNESCAPED_SLASHES | JSON_HEX_TAG);"#,
    );
    assert_eq!(out, r#"{"a":1,"b":[1.0,"\u00e9/\u003C"]}"#);
}

#[test]
fn decode_objects_and_last_error() {
    let out = run(r#"$o = json_decode('{"x":{"y":[1,2]},"":3}');
        echo $o->x->y[1], $o->{""}, json_last_error(), '|';
        var_dump(json_decode('[1', true), json_last_error(), json_last_error_msg());"#);
    assert_eq!(
        out,
        "230|NULL\nint(4)\nstring(30) \"Syntax error near location 1:3\"\n"
    );
}

#[test]
fn throw_on_error_uses_json_exception_with_code() {
    let out = run(
        r#"try { json_decode("{", false, 512, JSON_THROW_ON_ERROR); }
        catch (JsonException $e) { echo get_class($e), ':', $e->getCode(), ':', $e->getMessage(), ':', $e instanceof Exception ? 'E' : '-'; }"#,
    );
    assert_eq!(out, "JsonException:4:Syntax error near location 1:2:E");
}

#[test]
fn value_errors_for_depth() {
    let out = run(
        r#"try { json_decode("1", false, 0); } catch (ValueError $e) { echo $e->getMessage(), '|'; }
        try { json_validate("1", 1, 5); } catch (ValueError $e) { echo $e->getMessage(); }"#,
    );
    assert_eq!(
        out,
        "json_decode(): Argument #3 ($depth) must be greater than 0|json_validate(): Argument #3 ($flags) must be a valid flag (allowed flags: JSON_INVALID_UTF8_IGNORE)"
    );
}

#[test]
fn json_serializable_and_enums() {
    let out = run(
        r#"class P implements JsonSerializable { function __construct(private $v) {} function jsonSerialize(): mixed { return ['v' => $this->v]; } }
        enum S: string { case A = 'a'; }
        class Q { public $pub = 1; protected $pro = 2; private $pri = 3; }
        echo json_encode([new P(5), S::A, new Q]);"#,
    );
    assert_eq!(out, r#"[{"v":5},"a",{"pub":1}]"#);
}

#[test]
fn hooked_properties_use_get_hooks() {
    let out = run(
        r#"class C { public $a = 1; public $h { get => 5; } public $v { set { } } }
        echo json_encode(new C);"#,
    );
    assert_eq!(out, r#"{"a":1,"h":5}"#);
}

#[test]
fn recursion_is_detected() {
    let out = run(r#"$a = ['x' => 1]; $a['self'] = &$a;
        var_dump(json_encode($a), json_last_error_msg());
        echo json_encode($a, JSON_PARTIAL_OUTPUT_ON_ERROR);"#);
    assert_eq!(
        out,
        "bool(false)\nstring(18) \"Recursion detected\"\n{\"x\":1,\"self\":null}"
    );
}

#[test]
fn memory_limit_is_modelled() {
    let out = run(
        r#"ini_set('memory_limit', '4M'); json_encode(str_repeat("\x01", 1 << 20)); echo "not reached";"#,
    );
    assert!(
        out.contains("Allowed memory size of 4194304 bytes exhausted"),
        "{out}"
    );
    assert!(!out.contains("not reached"));
}
