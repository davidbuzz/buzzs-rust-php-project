//! Documents owned by another object model (the DOM binding), shared with
//! `SimpleXML` without copying — what `simplexml_import_dom()` does in PHP
//! (both extensions reference the same `xmlDoc`).
//!
//! `php-libxml`'s [`Document`] frees its `xmlDoc` with its last handle, so
//! a foreign document is wrapped once and one handle is leaked: the tree
//! stays owned by its object model. In exchange every `SimpleXML` object
//! on such a document holds that model's keep-alive (see
//! [`keepalive_for`]), so the tree outlives the `SimpleXML` objects using
//! it.

use std::any::Any;
use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::{Rc, Weak};

use php_libxml::ffi as lx;
use php_libxml::tree::{Document, Node};

thread_local! {
    /// Foreign documents wrapped so far, with their owner's keep-alive.
    static FOREIGN: RefCell<Vec<(Document, Weak<dyn Any>)>> = const { RefCell::new(Vec::new()) };
}

/// The [`Document`] sharing the foreign `xmlDoc` at `ptr`, kept alive by
/// `keepalive`.
///
/// # Safety
/// `ptr` must be a live `xmlDoc` that stays alive while `keepalive` (or a
/// clone of it) exists.
pub unsafe fn document(ptr: NonNull<lx::xmlDoc>, keepalive: &Rc<dyn Any>) -> Document {
    FOREIGN.with(|f| {
        let mut f = f.borrow_mut();
        f.retain(|(_, keep)| keep.strong_count() > 0);
        if let Some((d, _)) = f.iter().find(|(d, _)| d.as_ptr() == ptr.as_ptr()) {
            return d.clone();
        }
        // SAFETY: the document is live (caller); the leaked handle below
        // keeps php-libxml from ever freeing it.
        let d = unsafe { Document::from_raw(ptr) };
        std::mem::forget(d.clone());
        f.push((d.clone(), Rc::downgrade(keepalive)));
        d
    })
}

/// The owner keep-alive of `doc` when it is a foreign document.
pub fn keepalive_for(doc: &Document) -> Option<Rc<dyn Any>> {
    FOREIGN.with(|f| {
        f.borrow()
            .iter()
            .find(|(d, _)| d == doc)
            .and_then(|(_, keep)| keep.upgrade())
    })
}

/// The node of `doc` whose `xmlGetNodePath()` is `path` (how a foreign
/// object model's node is located when only its document is exposed).
pub fn node_by_path(doc: &Document, path: &[u8]) -> Option<Node> {
    if path == b"/" {
        return Some(doc.as_node());
    }
    let mut stack: Vec<Node> = doc.as_node().children();
    while let Some(n) = stack.pop() {
        let Some(p) = n.path() else { continue };
        if p == path {
            return Some(n);
        }
        let mut prefix = p.clone();
        prefix.push(b'/');
        if path.starts_with(&prefix) {
            stack.extend(n.attributes());
            stack.extend(n.children());
        }
    }
    None
}

/// Before `SimpleXML` unlinks nodes of a foreign document (`unset()`):
/// leaks one handle on every node of the subtree so `php-libxml` never
/// frees a node the owning object model may still reference (PHP shares
/// one node registry between the extensions; here the other model keeps
/// owning the detached nodes).
pub fn pin_for_unset(sxe: &php_simplexml::Sxe) {
    let Some(doc) = sxe.document() else { return };
    if keepalive_for(&doc).is_none() {
        return;
    }
    let start = sxe
        .node()
        .and_then(|x| x.node().cloned())
        .unwrap_or_else(|| doc.as_node());
    let mut stack = vec![start];
    while let Some(n) = stack.pop() {
        stack.extend(n.attributes());
        stack.extend(n.children());
        std::mem::forget(n);
    }
}
