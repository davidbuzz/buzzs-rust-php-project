// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Shared ownership of a tree between this layer and another object model
//! (simulated here by setting `node->_private`, PHP's convention).

use std::ptr::NonNull;

use php_libxml::ffi;
use php_libxml::tree::{Document, ParseOptions, SharedDocument};

fn parse(xml: &str) -> Document {
    Document::parse_memory(xml.as_bytes(), &ParseOptions::default()).expect("parse")
}

/// Marks a node as referenced by the "other model".
fn set_foreign(node: *mut ffi::xmlNode, on: bool) {
    // SAFETY: test nodes are live; `_private` is otherwise unused.
    unsafe {
        (*node)._private = if on {
            NonNull::<u8>::dangling().as_ptr().cast()
        } else {
            std::ptr::null_mut()
        };
    }
}

#[test]
fn document_lives_while_any_holder_exists() {
    let doc = parse("<r><a/></r>");
    let shared = doc.to_shared();
    assert!(doc.is_shared());
    assert_eq!(shared.as_ptr(), doc.as_ptr());
    drop(doc);
    // The tree is still usable through the shared handle.
    let root = shared.document().root().expect("root");
    assert_eq!(root.name(), b"r");
    let other = shared.clone();
    drop(shared);
    drop(root);
    assert_eq!(other.document().root().expect("root").name(), b"r");
}

#[test]
fn foreign_referenced_detached_nodes_are_not_freed() {
    let doc = parse("<r><a><b/></a></r>");
    let shared = doc.to_shared();
    let root = doc.root().expect("root");
    let a = root.first_child().expect("a");
    let b_ptr = a.first_child().expect("b").as_ptr();
    // The other model holds <b>.
    set_foreign(b_ptr, true);
    a.unlink();
    // Dropping the last handle of the detached <a> frees <a> but rescues <b>.
    drop(a);
    // SAFETY: <b> is alive (rescued); read its name and parent.
    unsafe {
        assert!((*b_ptr).parent.is_null());
        assert_eq!(
            std::ffi::CStr::from_ptr((*b_ptr).name.cast()).to_bytes(),
            b"b"
        );
    }
    // A pin of the rescued node, then the foreign reference goes away:
    // the last holder (the pin) frees it.
    // SAFETY: b is a live detached node of this document.
    let pin = unsafe { shared.pin(b_ptr) }.expect("pin");
    assert!(shared.is_pinned(b_ptr));
    set_foreign(b_ptr, false);
    drop(pin);
    assert!(!shared.is_pinned(b_ptr));
}

#[test]
fn foreign_model_frees_after_last_pin() {
    let doc = parse("<r><a/></r>");
    let shared = doc.to_shared();
    let a = doc.root().expect("root").first_child().expect("a");
    let p = a.as_ptr();
    // The other model also references <a>, and detaches it.
    set_foreign(p, true);
    // SAFETY: live node of this document.
    unsafe { shared.detach(p) };
    // Our handle goes first: the foreign reference keeps the node.
    drop(a);
    // SAFETY: still alive.
    assert!(unsafe { (*p).parent }.is_null());
    // The other model releases it: nothing pins it any more.
    set_foreign(p, false);
    assert!(!shared.is_pinned(p));
    // SAFETY: live detached node no one references.
    unsafe { shared.free_if_unreferenced(p) };
}

#[test]
fn generation_tracks_mutations() {
    let doc = parse("<r/>");
    let shared = doc.to_shared();
    let g0 = shared.generation();
    let root = doc.root().expect("root");
    let c = doc.create_element(b"c").expect("c");
    root.append_child(&c).expect("append");
    assert_ne!(shared.generation(), g0);
    let g1 = shared.generation();
    shared.mark_modified();
    assert_ne!(shared.generation(), g1);
    root.set_attribute(b"x", b"1").expect("attr");
    assert!(doc.generation() > g1);
}

#[test]
fn from_raw_takes_ownership() {
    // SAFETY: a fresh document owned by the shared handle only.
    let shared = unsafe {
        let d = ffi::xmlNewDoc(c"1.0".as_ptr().cast());
        SharedDocument::from_raw(NonNull::new(d).expect("doc"))
    };
    let doc = shared.document();
    let el = doc.create_element(b"root").expect("el");
    doc.set_root(&el).expect("root");
    assert!(doc.is_shared());
    assert_eq!(doc.dump(false, None), b"<?xml version=\"1.0\"?>\n<root/>\n");
}
