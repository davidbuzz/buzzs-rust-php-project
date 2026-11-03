//! End-to-end tests: PHP scripts run on a VM with the `dom` extension
//! (plus `libxml`, which provides the shared libxml state).

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
    let xml = php_ext_xml::Xml;
    let dom = php_ext_dom::Dom;
    let exts: [&dyn Extension; 2] = [&xml, &dom];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn load_and_save() {
    assert_eq!(
        run(r#"$d = new DOMDocument(); $d->loadXML('<r a="1"><c>x</c></r>'); echo $d->saveXML($d->documentElement);"#),
        r#"<r a="1"><c>x</c></r>"#
    );
}

#[test]
fn node_identity() {
    assert_eq!(
        run(r"$d = new DOMDocument(); $d->loadXML('<r><c/></r>');
              var_dump($d->documentElement === $d->documentElement);
              $c = $d->documentElement->firstChild; $c->foo = 1;
              echo $d->documentElement->firstChild->foo;"),
        "bool(true)\n1"
    );
}

#[test]
fn live_lists_and_iteration() {
    assert_eq!(
        run(r"$d = new DOMDocument(); $d->loadXML('<r><c>1</c><c>2</c></r>');
              $l = $d->getElementsByTagName('c');
              foreach ($l as $k => $n) echo $k, '=', $n->textContent, ' ';
              echo count($l), ' ', $l[1]->textContent, ' ', isset($l[5]) ? 'y' : 'n';
              $d->documentElement->appendChild($d->createElement('c', '3'));
              echo ' ', $l->length;"),
        "0=1 1=2 2 2 n 3"
    );
}

#[test]
fn readonly_properties_and_exceptions() {
    assert_eq!(
        run(r"$d = new DOMDocument();
              try { $d->nodeType = 3; } catch (Error $e) { echo $e->getMessage(), \"\n\"; }
              try { $d->createElement('1x'); } catch (DOMException $e) { echo $e->getCode(), ' ', $e->getMessage(); }"),
        "Cannot modify private(set) property DOMNode::$nodeType from global scope\n5 Invalid Character Error"
    );
}

#[test]
fn xpath_with_callbacks() {
    assert_eq!(
        run(r#"$d = new DOMDocument(); $d->loadXML('<r><c>a</c><c>b</c></r>');
              $x = new DOMXPath($d);
              $x->registerNamespace('php', 'http://php.net/xpath');
              $x->registerPhpFunctions('strtoupper');
              foreach ($x->query('//c[php:functionString("strtoupper", .) = "B"]') as $n) echo $n->textContent;"#),
        "b"
    );
}

#[test]
fn register_node_class() {
    assert_eq!(
        run(r"class MyEl extends DOMElement { function hi() { return 'hi ' . $this->tagName; } }
              $d = new DOMDocument(); $d->registerNodeClass('DOMElement', 'MyEl');
              $d->loadXML('<r/>'); echo get_class($d->documentElement), ' ', $d->documentElement->hi();"),
        "MyEl hi r"
    );
}

#[test]
fn modern_html_document() {
    assert_eq!(
        run(r#"$d = Dom\HTMLDocument::createFromString('<p class="a b">x</p>', LIBXML_NOERROR);
              $p = $d->querySelector('p');
              echo $p->classList->length, ' ', $p->classList === $p->classList ? 'same' : 'diff', ' ', $d->saveHTML($p);"#),
        r#"2 same <p class="a b">x</p>"#
    );
}

#[test]
fn serialization_is_refused() {
    assert_eq!(
        run(r"try { serialize(new DOMDocument()); } catch (Exception $e) { echo $e->getMessage(); }"),
        "Serialization of 'DOMDocument' is not allowed, unless serialization methods are implemented in a subclass"
    );
}
