// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end tests: PHP scripts run on a VM with the `SimpleXML`
//! extension (plus the SPL, JSON, streams and libxml bindings it builds
//! on). Expected output comes from the php-src `.phpt` files.

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

/// Keeps the test process's descriptors 0-2 usable: dropping the VM
/// closes the `STDIN`/`STDOUT`/`STDERR` stream resources, which really
/// close their descriptors (GH-8827 semantics).
struct StdioGuard([libc::c_int; 3]);

impl StdioGuard {
    fn save() -> Self {
        // SAFETY: dup() of the standard descriptors has no memory effects.
        Self([0, 1, 2].map(|fd| unsafe { libc::dup(fd) }))
    }
}

impl Drop for StdioGuard {
    fn drop(&mut self) {
        for (fd, saved) in self.0.iter().enumerate() {
            if *saved >= 0 {
                // SAFETY: restores a descriptor we duplicated above.
                unsafe {
                    libc::dup2(*saved, libc::c_int::try_from(fd).unwrap_or(0));
                    libc::close(*saved);
                }
            }
        }
    }
}

/// Runs `code` (without `<?php`) and returns its output.
fn run(code: &str) -> String {
    let sink = Sink::default();
    let streams = php_ext_streams::Streams;
    let spl = php_ext_spl::Spl;
    let json = php_ext_json::Json;
    let xml = php_ext_xml::Xml;
    let arr = php_ext_stdarr::StdArray;
    let sxe = php_ext_simplexml::SimpleXml;
    let exts: [&dyn Extension; 6] = [&streams, &arr, &spl, &json, &xml, &sxe];
    let saved = StdioGuard::save();
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    drop(saved);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn read_elements_and_attributes() {
    let code = r#"
        $x = simplexml_load_string('<r a="1"><i>x</i><i>y</i></r>');
        echo $x->i, $x->i[1], $x['a'], count($x->i), $x->getName();
        foreach ($x->i as $k => $v) { echo " $k=$v"; }
    "#;
    assert_eq!(run(code), "xy12r i=x i=y");
}

#[test]
fn writes_create_nodes() {
    let code = r"
        $x = simplexml_load_string('<r/>');
        $x->a = 'v';
        $x->a['k'] = 'w';
        $x->b->c = 'deep';
        $x->a[1] = 'second';
        echo $x->asXML();
    ";
    assert_eq!(
        run(code),
        "<?xml version=\"1.0\"?>\n<r><a k=\"w\">v</a><b><c>deep</c></b><a>second</a></r>\n"
    );
}

#[test]
fn isset_empty_unset() {
    let code = r#"
        $x = simplexml_load_string('<r a=""><i>0</i></r>');
        echo (int) isset($x->i), (int) isset($x->j), (int) empty($x->j), (int) isset($x['a']);
        unset($x->i);
        echo (int) isset($x->i);
    "#;
    assert_eq!(run(code), "10110");
}

#[test]
fn casts_and_json() {
    let code = r"
        $x = simplexml_load_string('<r><n>42</n><f>1.5</f></r>');
        var_dump((int) $x->n, (float) $x->f, (bool) $x->missing, json_encode($x));
    ";
    assert_eq!(
        run(code),
        "int(42)\nfloat(1.5)\nbool(false)\nstring(20) \"{\"n\":\"42\",\"f\":\"1.5\"}\"\n"
    );
}

#[test]
fn subclass_and_class_name_validation() {
    let code = r#"
        class E extends SimpleXMLElement {}
        $x = simplexml_load_string('<r><i/></r>', 'E');
        echo get_class($x), ' ', get_class($x->i), "\n";
        try { simplexml_load_string('<r/>', 'stdClass'); }
        catch (TypeError $e) { echo $e->getMessage(); }
    "#;
    assert_eq!(
        run(code),
        "E E\nsimplexml_load_string(): Argument #2 ($class_name) must be a class name derived from SimpleXMLElement, \"stdClass\" given"
    );
}

#[test]
fn user_overrides_win() {
    let code = r"
        class E extends SimpleXMLElement {
            public function __toString(): string { return 'custom'; }
            public function count(): int { return 7; }
        }
        $x = new E('<r><i/></r>');
        echo $x, count($x);
    ";
    assert_eq!(run(code), "custom7");
}

#[test]
fn add_element_warning_uses_main_prefix() {
    let code = r"
        $x = simplexml_load_string('<r><i/></r>');
        $x->i[5] = 'z';
    ";
    assert!(
        run(code).contains("main(): Cannot add element i number 5 when only 1 such elements exist")
    );
}

#[test]
fn uninitialized_complex_write_chains_both_errors() {
    // GH-12929: `GET_NODE` throws and carries on to the value check.
    assert_eq!(
        run(r"$x = (new ReflectionClass('SimpleXMLElement'))->newInstanceWithoutConstructor();
            try { $x->a = []; } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(), ' / ', $e->getPrevious()?->getMessage(), '|'; }
            try { $x->a = 1; } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(), ' / ', var_export($e->getPrevious(), true); }"),
        "TypeError: It's not possible to assign a complex type to properties, array given / SimpleXMLElement is not properly initialized|Error: SimpleXMLElement is not properly initialized / NULL"
    );
}
