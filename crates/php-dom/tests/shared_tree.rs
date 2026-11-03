//! Trees shared with php-libxml's document layer (the `SimpleXML` side of
//! `simplexml_import_dom()` / `dom_import_simplexml()`).

use php_dom::dispatch::{call, read_property, DomObject, PropRead};
use php_dom::{DomValue, NoHost};

fn s(v: &str) -> DomValue {
    DomValue::Str(v.as_bytes().to_vec())
}

fn obj(v: DomValue) -> DomObject {
    match v {
        DomValue::Node(n) => DomObject::Node(n),
        other => panic!("expected a node, got {other:?}"),
    }
}

fn prop(o: &DomObject, name: &str) -> DomValue {
    match read_property(o, name).expect("read") {
        PropRead::Value(v) => v,
        _ => DomValue::Null,
    }
}

fn legacy_doc(xml: &str) -> DomObject {
    let d = DomObject::Node(php_dom::Node::new_unbound("DOMDocument", "DOMDocument"));
    let d = php_dom::dispatch::construct(&d, &[], &NoHost)
        .expect("construct")
        .unwrap_or(d);
    call(&d, "loadXML", &[s(xml)], &NoHost).expect("loadXML");
    d
}

#[test]
fn tree_outlives_dom_objects_through_shared_pins() {
    let doc = legacy_doc("<r><a>x</a></r>");
    let DomObject::Node(d) = &doc else {
        unreachable!()
    };
    let shared = d.shared_document().expect("shared");
    let root = obj(prop(&doc, "documentElement"));
    let DomObject::Node(r) = &root else {
        unreachable!()
    };
    // SAFETY: the root is a live node of the shared document.
    let pin = unsafe { shared.pin(r.raw_node().expect("bound").as_ptr().cast()) }.expect("pin");
    drop(root);
    drop(doc);
    // The DOM objects are gone; the tree is still alive through the pin.
    assert_eq!(pin.name(), b"r");
    assert_eq!(pin.first_child().expect("a").content(), b"x");
    // Re-entering DOM from the shared tree (dom_import_simplexml()).
    let back = php_dom::interop::adopt_shared_document(shared.clone(), false, None);
    assert_eq!(back.class_name(), "DOMDocument");
    // SAFETY: a live node of the shared document.
    let a = unsafe {
        php_dom::interop::wrap_shared_node(&shared, pin.first_child().expect("a").as_ptr().cast())
    }
    .expect("node");
    assert_eq!(a.class_name(), "DOMElement");
}

#[test]
fn removed_node_survives_while_other_model_holds_it() {
    let doc = legacy_doc("<r><a><b>t</b></a></r>");
    let DomObject::Node(d) = &doc else {
        unreachable!()
    };
    let shared = d.shared_document().expect("shared");
    let root = obj(prop(&doc, "documentElement"));
    let a = prop(&root, "firstChild");
    let DomValue::Node(an) = &a else { panic!("a") };
    // The other model pins <b> inside <a>.
    let b_ptr = an.raw_node().map(std::ptr::NonNull::as_ptr).expect("a");
    // SAFETY: <a>'s first child is a live element of the document.
    let b = unsafe { shared.pin((*b_ptr.cast::<php_libxml::ffi::xmlNode>()).children) }.expect("b");
    // DOM removes <a> and drops its object: <a> is freed, <b> is rescued.
    call(&root, "removeChild", std::slice::from_ref(&a), &NoHost).expect("removeChild");
    drop(a);
    // <a> had no other holder and was freed; <b> was unlinked and survives.
    assert!(!b.is_attached());
    assert_eq!(b.content(), b"t");
    drop(b);
}

#[test]
fn node_lists_see_mutations_of_the_other_model() {
    let doc = legacy_doc("<r><a/></r>");
    let DomObject::Node(d) = &doc else {
        unreachable!()
    };
    let shared = d.shared_document().expect("shared");
    let list = call(&doc, "getElementsByTagName", &[s("a")], &NoHost).expect("list");
    let DomValue::NodeList(list) = list else {
        panic!("list")
    };
    assert_eq!(list.length(), 1);
    let tdoc = shared.document();
    let root = tdoc.root().expect("root");
    let extra = tdoc.create_element(b"a").expect("a");
    root.append_child(&extra).expect("append");
    assert_eq!(list.length(), 2);
}
