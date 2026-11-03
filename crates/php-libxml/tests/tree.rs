//! Tests of the safe document layer and of libxml error capture through
//! it (ports of ext/libxml tests that need a parsed document).

use php_libxml::error::{self, Diagnostic, DiagnosticLevel};
use php_libxml::tree::{Document, NodeType, ParseError, ParseOptions, ParserChannels, TreeError};

fn reset() {
    error::reset_request_state();
    error::set_diagnostic_handler(None);
    error::take_diagnostics();
}

const MOVIES: &str = "<?xml version='1.0' standalone='yes'?>
    <movies>
        <movie>
            <titles>PHP: Behind the Parser</title>
        </movie>
    </movies>";

#[test]
fn libxml_002_errors_are_collected() {
    reset();
    assert!(!error::use_internal_errors(Some(true)));
    let r = Document::parse_memory(MOVIES.as_bytes(), &ParseOptions::default());
    assert_eq!(r.unwrap_err(), ParseError::NotWellFormed);
    let errors = error::get_errors();
    assert!(!errors.is_empty());
    let first = &errors[0];
    assert_eq!(first.level, error::level::FATAL);
    assert_eq!(first.code, 76);
    assert_eq!(first.line, 4);
    assert_eq!(
        first.message,
        b"Opening and ending tag mismatch: titles line 4 and title\n"
    );
    assert_eq!(first.file_or_empty(), b"");
    assert!(error::get_last_error().is_some());
    assert!(error::take_diagnostics().is_empty());
    // libxml_use_internal_errors_clears_last_error.phpt
    assert!(error::use_internal_errors(Some(false)));
    assert!(error::get_errors().is_empty());
    assert_eq!(error::get_last_error(), None);
    reset();
}

#[test]
fn ctx_channel_displays_entity_line_warnings() {
    reset();
    let r = Document::parse_memory(b"<a><b></a>", &ParseOptions::default());
    assert!(r.is_err());
    let d = error::take_diagnostics();
    assert_eq!(
        d.first(),
        Some(&Diagnostic {
            level: DiagnosticLevel::Warning,
            message: b"Opening and ending tag mismatch: b line 1 and a in Entity, line: 1".to_vec()
        })
    );
    // Without internal errors the last error is libxml's own.
    assert!(error::get_last_error().is_some());
    error::clear_errors();
    assert_eq!(error::get_last_error(), None);
    reset();
}

#[test]
fn silent_channel_and_recover() {
    reset();
    let opts = ParseOptions {
        options: php_libxml::constants::LIBXML_RECOVER,
        channels: ParserChannels::SILENT,
        ..Default::default()
    };
    let doc = Document::parse_memory(b"<a><b></a>", &opts).expect("recovered");
    assert!(error::take_diagnostics().is_empty());
    assert_eq!(doc.root().expect("root").name(), b"a");
    reset();
}

#[test]
fn file_url_appears_in_messages() {
    reset();
    let opts = ParseOptions {
        url: Some(b"/tmp/x.xml".to_vec()),
        ..Default::default()
    };
    let _ = Document::parse_memory(b"<a>", &opts);
    let d = error::take_diagnostics();
    assert!(
        d.iter()
            .any(|d| d.message.ends_with(b" in /tmp/x.xml, line: 1")),
        "{d:?}"
    );
    reset();
}

#[test]
fn navigation_and_content() {
    reset();
    let doc = Document::parse_memory(
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><r xmlns:p=\"urn:p\" a=\"1\" p:b=\"2\"><p:c>x<!--c-->y</p:c><d/></r>",
        &ParseOptions::default(),
    )
    .expect("parse");
    assert_eq!(doc.version().as_deref(), Some(&b"1.0"[..]));
    assert_eq!(doc.encoding().as_deref(), Some(&b"UTF-8"[..]));
    let root = doc.root().expect("root");
    assert_eq!(root.node_type(), NodeType::Element);
    assert_eq!(root.parent().expect("doc").node_type(), NodeType::Document);
    assert_eq!(root.attribute(b"a").as_deref(), Some(&b"1"[..]));
    assert_eq!(
        root.attribute_ns(b"b", Some(b"urn:p")).as_deref(),
        Some(&b"2"[..])
    );
    let attrs = root.attributes();
    assert_eq!(attrs.len(), 2);
    assert_eq!(attrs[1].qualified_name(), b"p:b");
    assert_eq!(root.namespace_declarations()[0].href, b"urn:p");
    let kids = root.children();
    assert_eq!(kids.len(), 2);
    assert_eq!(kids[0].qualified_name(), b"p:c");
    assert_eq!(kids[0].namespace().expect("ns").href, b"urn:p");
    assert_eq!(kids[0].content(), b"xy");
    assert_eq!(kids[0].children().len(), 3);
    assert_eq!(kids[1].prev_sibling(), Some(kids[0].clone()));
    assert_eq!(
        root.lookup_namespace_uri(Some(b"p")).as_deref(),
        Some(&b"urn:p"[..])
    );
    assert_eq!(kids[1].path().as_deref(), Some(&b"/r/d"[..]));
    assert_eq!(kids[0].dump(false, None), b"<p:c>x<!--c-->y</p:c>");
    reset();
}

#[test]
fn build_and_serialize() {
    let doc = Document::new(b"1.0").expect("doc");
    let root = doc.create_element(b"root").expect("el");
    doc.set_root(&root).expect("root");
    let child = doc.create_element_ns(b"urn:x", b"x:child").expect("el");
    root.append_child(&child).expect("append");
    child.set_attribute(b"k", b"v&<").expect("attr");
    let t1 = doc.create_text(b"a").expect("text");
    let t2 = doc.create_text(b"b").expect("text");
    child.append_child(&t1).expect("append");
    child.append_child(&t2).expect("append");
    // No text merging: both handles stay valid and distinct.
    assert_eq!(child.children().len(), 2);
    let pi = doc
        .create_processing_instruction(b"php", b"echo 1;")
        .expect("pi");
    root.insert_before(&pi, Some(&child)).expect("insert");
    root.append_child(&doc.create_comment(b"c").expect("c"))
        .expect("append");
    root.append_child(&doc.create_cdata(b"<x>").expect("cd"))
        .expect("append");
    assert_eq!(
        doc.dump(false, None),
        b"<?xml version=\"1.0\"?>\n<root><?php echo 1;?><x:child xmlns:x=\"urn:x\" k=\"v&amp;&lt;\">ab</x:child><!--c--><![CDATA[<x>]]></root>\n"
    );
    assert_eq!(
        doc.save(php_libxml::ffi::XML_SAVE_NO_DECL, None).expect("save"),
        b"<root><?php echo 1;?><x:child xmlns:x=\"urn:x\" k=\"v&amp;&lt;\">ab</x:child><!--c--><![CDATA[<x>]]></root>\n"
    );
    assert_eq!(root.append_child(&root), Err(TreeError::Hierarchy));
    assert_eq!(child.append_child(&root), Err(TreeError::Hierarchy));
    let other = Document::new(b"1.0").expect("doc");
    assert_eq!(
        root.append_child(&other.create_element(b"z").expect("z")),
        Err(TreeError::WrongDocument)
    );
    let imported = other.import(&child, true).expect("import");
    assert_eq!(
        imported.dump(false, None),
        b"<x:child xmlns:x=\"urn:x\" k=\"v&amp;&lt;\">ab</x:child>"
    );
}

#[test]
fn detached_nodes_survive_their_parents() {
    let doc = Document::parse_memory(
        b"<r><a xmlns:n=\"urn:n\"><n:b n:at=\"1\">t</n:b></a></r>",
        &ParseOptions::default(),
    )
    .expect("parse");
    let root = doc.root().expect("root");
    let a = root.first_child().expect("a");
    let b = a.first_child().expect("b");
    let at = b.attributes().remove(0);
    let text = b.first_child().expect("t");
    a.unlink();
    drop(root);
    // Dropping `a` frees it, but `b` (referenced) is rescued with its
    // namespace reconciled; the attribute keeps a valid namespace.
    drop(a);
    assert!(!b.is_attached());
    assert_eq!(b.namespace().expect("ns").href, b"urn:n");
    assert_eq!(at.namespace().expect("ns").href, b"urn:n");
    assert_eq!(
        b.dump(false, None),
        b"<n:b xmlns:n=\"urn:n\" n:at=\"1\">t</n:b>"
    );
    // set_content replaces children, but the referenced text node survives.
    b.set_content(b"new");
    assert!(!text.is_attached());
    assert_eq!(text.content(), b"t");
    assert_eq!(b.content(), b"new");
    // Replacing an attribute value keeps the old value node alive.
    let old_value = at.first_child().expect("value");
    b.set_attribute(b"x", b"1").expect("set");
    assert!(b.remove_attribute(b"x"));
    assert!(!b.remove_attribute(b"x"));
    assert_eq!(old_value.content(), b"1");
    // Re-attaching a detached subtree.
    let root = doc.root().expect("root");
    root.append_child(&b).expect("append");
    assert_eq!(
        root.dump(false, None),
        b"<r><n:b xmlns:n=\"urn:n\" n:at=\"1\">new</n:b></r>"
    );
    drop(doc);
    // Handles keep the document alive.
    assert_eq!(root.children().len(), 1);
}

#[test]
fn handles_are_counted() {
    let doc = Document::parse_memory(b"<r><a/></r>", &ParseOptions::default()).expect("parse");
    assert_eq!(doc.live_handles(), 0);
    let r1 = doc.root().expect("root");
    let r2 = r1.clone();
    assert_eq!(doc.live_handles(), 1);
    assert_eq!(r1.id(), r2.id());
    drop(r1);
    drop(r2);
    assert_eq!(doc.live_handles(), 0);
    let replaced = doc.create_element(b"n").expect("n");
    let old = doc.root().expect("root");
    doc.set_root(&replaced).expect("set_root");
    assert!(!old.is_attached());
    assert_eq!(old.dump(false, None), b"<r><a/></r>");
    drop(old);
    assert_eq!(doc.dump(false, None), b"<?xml version=\"1.0\"?>\n<n/>\n");
}

#[test]
fn copies_and_lines() {
    let doc =
        Document::parse_memory(b"<r>\n<a>1</a>\n</r>", &ParseOptions::default()).expect("parse");
    let a = doc.root().expect("r").children()[1].clone();
    assert_eq!(a.line(), 2);
    let shallow = a.copy(false).expect("copy");
    assert_eq!(shallow.dump(false, None), b"<a/>");
    let deep = a.copy(true).expect("copy");
    assert_eq!(deep.content(), b"1");
}
