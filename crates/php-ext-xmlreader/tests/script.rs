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
    let xml = php_ext_xml::Xml;
    let reader = php_ext_xmlreader::XmlReaderExt;
    let exts: [&dyn Extension; 3] = [&streams, &xml, &reader];
    let saved = StdioGuard::save();
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    drop(saved);
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

#[test]
fn var_dump_prints_the_table_then_raises_the_pending_error() {
    // fromString_custom_constructor.phpt: `isEmptyElement` fails before
    // the first read(); `xmlreader_get_debug_info` skips it and leaves the
    // exception pending.
    let code = r#"
        $r = XMLReader::fromString('<root/>');
        try { var_dump($r); } catch (Error $e) { echo $e->getMessage(), "\n"; }
        var_dump($r->read(), $r->nodeType);
    "#;
    let out = run(code);
    assert!(out.starts_with("object(XMLReader)#"), "{out}");
    assert!(
        out.contains(" (13) {\n  [\"attributeCount\"]=>\n  int(0)\n"),
        "{out}"
    );
    assert!(!out.contains("isEmptyElement"), "{out}");
    assert!(
        out.ends_with("}\nFailed to read property because no XML data has been read yet\nbool(true)\nint(1)\n"),
        "{out}"
    );
}
