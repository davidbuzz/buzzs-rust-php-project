//! End-to-end tests: PHP scripts run on a VM with the `xmlreader`
//! extension (plus the streams and libxml bindings it builds on).
//! Expected output comes from the php-src `.phpt` files.

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

/// Runs `code` (without `<?php`) and returns its output.
fn run(code: &str) -> String {
    let sink = Sink::default();
    let streams = php_ext_streams::Streams;
    let xml = php_ext_xml::Xml;
    let reader = php_ext_xmlreader::XmlReaderExt;
    let exts: [&dyn Extension; 3] = [&streams, &xml, &reader];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn read_names_on_instance_and_statically() {
    let code = r#"
        $r = new XMLReader();
        var_dump($r->XML('<a><b x="1"/></a>'));
        while ($r->read()) { echo $r->name, $r->nodeType, ' '; }
        $s = XMLReader::XML('<z/>');
        $s->read();
        echo get_class($s), ' ', $s->localName;
    "#;
    assert_eq!(run(code), "bool(true)\na1 b1 a15 XMLReader z");
}

#[test]
fn attributes_and_properties() {
    let code = r#"
        $r = XMLReader::fromString('<a x="1" y="2"/>');
        $r->read();
        echo $r->attributeCount, $r->getAttribute('y'), var_export($r->getAttribute('q'), true);
        echo isset($r->depth) ? 'I' : 'i', empty($r->depth) ? 'E' : 'e';
        try { $r->depth = 3; } catch (Error $e) { echo ' ', $e->getMessage(); }
    "#;
    assert_eq!(
        run(code),
        "22NULLIE Cannot modify readonly property XMLReader::$depth"
    );
}

#[test]
fn errors_before_loading() {
    let code = r#"
        $r = new XMLReader();
        try { $r->read(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
        try { $r->XML(''); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
        var_dump($r->nodeType);
    "#;
    assert_eq!(
        run(code),
        "Data must be loaded before reading\nXMLReader::XML(): Argument #1 ($source) must not be empty\nint(0)\n"
    );
}

#[test]
fn from_stream_and_subclass() {
    let code = r#"
        class R extends XMLReader { public function __construct() { echo "ctor "; } }
        $h = fopen('php://memory', 'w+');
        fwrite($h, '<root><c/></root>');
        rewind($h);
        $r = R::fromStream($h);
        while ($r->read()) { echo $r->name, ' '; }
        echo get_class($r);
    "#;
    assert_eq!(run(code), "ctor root c root R");
}
