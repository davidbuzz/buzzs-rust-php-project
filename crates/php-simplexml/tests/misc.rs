//! Ports of the remaining named `ext/simplexml/tests/*.phpt`.

mod common;

use common::{fixture, s, T};
use php_runtime::Value;
use php_simplexml::{CheckKind, FetchMode, Sxe};

#[derive(Clone)]
enum P {
    Prop(&'static str),
    Dim(Value),
}

fn fetch(t: &mut T, base: &Value, path: &[P], mode: FetchMode) -> Value {
    let mut cur = base.clone();
    for seg in path {
        if !matches!(cur, Value::Object(_)) {
            return Value::Null;
        }
        cur = match seg {
            P::Prop(n) => t.prop_mode(&cur, n, mode),
            P::Dim(k) => t.dim_mode(&cur, Some(k.clone()), mode),
        };
    }
    cur
}

fn isset_path(t: &mut T, base: &Value, path: &[P]) -> bool {
    let Some((last, init)) = path.split_last() else {
        return true;
    };
    let parent = fetch(t, base, init, FetchMode::IsSet);
    if !matches!(parent, Value::Object(_)) {
        return false;
    }
    let sx = t.sxe(&parent);
    let r = match last {
        P::Prop(n) => Sxe::has_property(&sx, &mut t.h, n.as_bytes(), CheckKind::Isset),
        P::Dim(k) => Sxe::has_dimension(&sx, &mut t.h, k, CheckKind::Isset),
    };
    t.catch(r)
}

#[test]
fn t000_isset_bool_count_dump() {
    let mut t = T::new("000");
    let sxe = t.load_file(&fixture("000.xml"));
    let i = |n: i64| P::Dim(Value::Int(n));
    let cases: Vec<(&str, Vec<P>)> = vec![
        ("sxe", vec![]),
        ("sxe->elem1", vec![P::Prop("elem1")]),
        ("sxe->elem1[0]", vec![P::Prop("elem1"), i(0)]),
        ("sxe->elem1[0]->elem2", vec![P::Prop("elem1"), i(0), P::Prop("elem2")]),
        ("sxe->elem1[0]->elem2->bla", vec![P::Prop("elem1"), i(0), P::Prop("elem2"), P::Prop("bla")]),
        ("sxe->elem1[0][\"attr1\"]", vec![P::Prop("elem1"), i(0), P::Dim(s("attr1"))]),
        ("sxe->elem1[0]->attr1", vec![P::Prop("elem1"), i(0), P::Prop("attr1")]),
        ("sxe->elem1[1]", vec![P::Prop("elem1"), i(1)]),
        ("sxe->elem1[2]", vec![P::Prop("elem1"), i(2)]),
        ("sxe->elem11", vec![P::Prop("elem11")]),
        ("sxe->elem11->elem111", vec![P::Prop("elem11"), P::Prop("elem111")]),
        ("sxe->elem11->elem111->elem1111", vec![P::Prop("elem11"), P::Prop("elem111"), P::Prop("elem1111")]),
        ("sxe->elem22", vec![P::Prop("elem22")]),
        ("sxe->elem22->elem222", vec![P::Prop("elem22"), P::Prop("elem222")]),
        ("sxe->elem22->attr22", vec![P::Prop("elem22"), P::Prop("attr22")]),
        ("sxe->elem22[\"attr22\"]", vec![P::Prop("elem22"), P::Dim(s("attr22"))]),
    ];
    for (what, path) in cases {
        t.echo(format!("==={what}\n"));
        let isset = isset_path(&mut t, &sxe, &path);
        t.var_dump(&Value::Bool(isset));
        let v = fetch(&mut t, &sxe, &path, FetchMode::Read);
        let b = t.bool(&v);
        t.var_dump(&Value::Bool(b));
        drop(v);
        if isset {
            let v = fetch(&mut t, &sxe, &path, FetchMode::Read);
            let c = match &v {
                Value::Object(_) => t.count(&v),
                Value::Null => 0,
                _ => 1,
            };
            drop(v);
            t.var_dump(&Value::Int(c));
        }
        let v = fetch(&mut t, &sxe, &path, FetchMode::Read);
        t.var_dump(&v);
    }
    t.expect("000.phpt");
}

#[test]
fn t001_mb_filename() {
    let mut t = T::new("001-mb");
    let v = t.load_file(&fixture("sxe私はガラスを食べられます.xml"));
    t.var_dump(&v);
    t.expect("001-mb.phpt");
}

#[test]
fn add_child_ns_filter_returned_element() {
    let mut t = T::new("addChild_ns_filter_returned_element");
    let state = |t: &mut T, v: &Value| {
        let x = t.as_xml(v);
        let st = t.str(&x);
        t.echo(st.trim().to_string() + "\n");
    };
    let x = t.new_sxe("<r xmlns:a=\"http://example.com\"/>");
    let c = t.add_child(&x, "a:kid", None, Some("http://example.com"));
    t.add_child(&c, "inner", Some("v"), None);
    state(&mut t, &x);
    let i = t.prop(&c, "inner");
    let st = t.str(&i);
    t.echo(st + "\n");
    let b = t.isset_prop(&c, "inner");
    t.var_dump(&Value::Bool(b));

    let y = t.new_sxe("<r xmlns:a=\"http://example.com\"/>");
    let d = t.add_child(&y, "kid", None, Some("http://example.com"));
    t.add_child(&d, "inner", Some("w"), None);
    state(&mut t, &y);
    let i = t.prop(&d, "inner");
    let st = t.str(&i);
    t.echo(st + "\n");

    let z = t.new_sxe("<r/>");
    let e = t.add_child(&z, "a:kid", None, None);
    t.add_child(&e, "inner", Some("z"), None);
    state(&mut t, &z);
    let i = t.prop(&e, "inner");
    let st = t.str(&i);
    t.echo(st + "\n");
    let b = t.isset_prop(&e, "inner");
    t.var_dump(&Value::Bool(b));

    let q = t.new_sxe("<p:r xmlns:p=\"http://example.com/p\"/>");
    let f = t.add_child(&q, "kid", None, None);
    t.add_attribute(&f, "id", "7", None);
    state(&mut t, &q);
    let i = t.dim(&f, s("id"));
    let st = t.str(&i);
    t.echo(st + "\n");

    let m = t.new_sxe("<r xmlns:a=\"http://example.com\"/>");
    let g = t.add_child(&m, "kid", None, Some("http://example.com"));
    t.add_attribute(&g, "id", "8", None);
    state(&mut t, &m);
    let b = t.isset_dim(&g, s("id"));
    t.var_dump(&Value::Bool(b));
    t.expect("addChild_ns_filter_returned_element.phpt");
}

#[test]
fn attributes_dimension_write() {
    let mut t = T::new("attributes_dimension_write");
    let out = |t: &mut T, v: &Value| {
        let x = t.as_xml(v);
        let st = t.str(&x);
        t.echo(st);
    };
    let x = t.load_string("<r a=\"1\"/>");
    let a = t.attributes(&x, None, false);
    t.set_dim(&a, Some(s("new")), s("v"));
    out(&mut t, &x);
    let x = t.load_string("<r/>");
    let a = t.attributes(&x, None, false);
    t.set_dim(&a, Some(s("created")), s("yes"));
    out(&mut t, &x);
    let x = t.load_string("<r a=\"1\"/>");
    let a = t.attributes(&x, None, false);
    t.set_prop(&a, "other", Value::Int(2));
    out(&mut t, &x);
    let x = t.load_string("<r a=\"1\"/>");
    let a = t.attributes(&x, None, false);
    t.set_dim(&a, Some(s("a")), s("2"));
    out(&mut t, &x);
    t.expect("attributes_dimension_write.phpt");
}

#[test]
fn foreach_by_reference() {
    let e = Sxe::get_iterator(true).unwrap_err();
    assert_eq!(e.message(), Some("An iterator cannot be used with foreach by reference"));
    assert!(Sxe::get_iterator(false).is_ok());
}

#[test]
fn get_doc_namespaces_no_internal_declaration() {
    let mut t = T::new("getDocNamespaces_no_internal_declaration");
    let xml = "<root>\n    <child xmlns=\"urn:a\">\n        <a/>\n        <b xmlns=\"\"/>\n        <c xmlns:a=\"urn:a\" xmlns=\"urn:c\"/>\n    </child>\n    <child2 xmlns:d=\"urn:d\"/>\n</root>";
    let sxe = t.load_string(xml);
    let sx = t.sxe(&sxe);
    let v = Sxe::get_doc_namespaces(&sx, true, true).unwrap();
    t.var_dump(&v);
    let out = t.output();
    let first_block = out.split("}\n").next().unwrap_or_default().to_string() + "}";
    let expected = "array(3) {\n  [\"\"]=>\n  string(5) \"urn:a\"\n  [\"a\"]=>\n  string(5) \"urn:a\"\n  [\"d\"]=>\n  string(5) \"urn:d\"\n}";
    assert_eq!(first_block, expected);
}

#[test]
fn iterator_interaction_empty_and_var_dump() {
    let mut t = T::new("iterator_interaction_empty_and_var_dump");
    let sxe = t.load_string("<container>\n    <first><foo/></first>\n</container>");
    let name_of_current = |t: &mut T, v: &Value| {
        let sx = t.sxe(v);
        let c = Sxe::current(&sx).unwrap();
        let n = t.get_name(&c);
        t.var_dump(&s(&n));
    };
    t.echo("--- var_dump ---\n");
    let first = t.prop(&sxe, "first");
    let sx = t.sxe(&first);
    Sxe::rewind(&sx, &mut t.h).unwrap();
    name_of_current(&mut t, &first);
    t.var_dump(&first);
    name_of_current(&mut t, &first);
    t.echo("--- empty ---\n");
    let first = t.prop(&sxe, "first");
    let sx = t.sxe(&first);
    Sxe::rewind(&sx, &mut t.h).unwrap();
    name_of_current(&mut t, &first);
    let b = t.bool(&first);
    t.var_dump(&Value::Bool(!b));
    name_of_current(&mut t, &first);
    t.expect("iterator_interaction_empty_and_var_dump.phpt");
}

#[test]
fn get_doc_namespaces_invalid_xml() {
    let mut t = T::new("SimpleXMLElement_getDocNamespaces");
    let r = t.new_sxe_ex("SimpleXMLElement", "X", 1, false, "", false);
    t.echo(format!("{}\n", r.is_ok()));
    let _ = php_libxml::error::take_diagnostics();
    let out = t.output();
    // With LIBXML_RECOVER (1) "X" yields a document without root.
    if let Ok(v) = r {
        let sx = t.sxe(&v);
        let ns = Sxe::get_doc_namespaces(&sx, false, true).unwrap();
        assert!(matches!(ns, Value::Bool(false)), "{out}");
    }
}

#[test]
fn xpath_on_rootless_document() {
    let mut t = T::new("SimpleXMLElement_xpath");
    let x = t.load_string_ex("XXXXXXX^", Some("SimpleXMLElement"), 1, "", false);
    let sx = t.sxe(&x);
    let e = Sxe::xpath(&sx, &mut t.h, b"BBBB").unwrap_err();
    assert_eq!(e.message(), Some("SimpleXMLElement is not properly initialized"));
}

#[test]
fn load_string_options_too_large() {
    let mut t = T::new("SimpleXMLElement_xpath_4");
    let e = Sxe::load_string(&mut t.h, b"XXXXXXX^", None, 0x6000_0000_0000_0001, b"", false)
        .unwrap_err();
    assert_eq!(
        e.message(),
        Some("simplexml_load_string(): Argument #3 ($options) is too large")
    );
}

#[test]
fn simplexml_load_file_book() {
    let mut t = T::new("simplexml_load_file");
    let v = t.load_file(&fixture("book.xml"));
    t.var_dump(&v);
    t.echo("Done");
    t.expect("simplexml_load_file.phpt");
}

#[test]
fn xml_parsing_no_xxe() {
    let mut t = T::new("xml_parsing_LIBXML_NO_XXE");
    let xml = "<?xml version='1.0' encoding='utf-8'?>\n<!DOCTYPE set [\n    <!ENTITY foo '<foo>bar</foo>'>\n    <!ENTITY xxe SYSTEM \"file:///etc/passwd\">\n]>\n<set>&foo;&xxe;</set>";
    let opts = i64::from(php_libxml::constants::LIBXML_NOENT) | (1 << 23);
    let v = t.load_string_ex(xml, None, opts, "", false);
    t.func("simplexml_load_string");
    t.var_dump(&v);
    let out = t.output();
    assert!(out.contains("[\"foo\"]=>"), "{out}");
}

#[test]
fn xml_parsing_recover() {
    let mut t = T::new("xml_parsing_LIBXML_RECOVER");
    t.func("simplexml_load_string");
    let v = t.load_string_ex("<root><child/>", None, 1, "", false);
    t.var_dump(&v);
    t.expect("xml_parsing_LIBXML_RECOVER.phpt");
}

#[test]
fn current_and_key_errors() {
    for name in ["current_error", "key_error"] {
        let mut t = T::new(name);
        let sxe = t.load_string("<?xml version='1.0'?>\n<root>\n<elem/>\n</root>");
        let sx = t.sxe(&sxe);
        let call = |t: &mut T| -> Result<Value, php_runtime::RuntimeError> {
            if name == "current_error" {
                Sxe::current(&sx)
            } else {
                Sxe::key(&sx, &mut t.h).map(|k| Value::Str(k.into()))
            }
        };
        // The caught exception object stays alive in `$exception`.
        let _exception = common::new_object("Error", None);
        if let Err(e) = call(&mut t) {
            t.exception(&e);
        }
        Sxe::rewind(&sx, &mut t.h).unwrap();
        while Sxe::valid(&sx) {
            let k = Sxe::key(&sx, &mut t.h).unwrap();
            let c = Sxe::current(&sx).unwrap();
            t.var_dump(&Value::Str(k.into()));
            t.var_dump(&c);
            drop(c);
            Sxe::next(&sx, &mut t.h).unwrap();
        }
        if let Err(e) = call(&mut t) {
            t.exception(&e);
        }
        t.expect(&format!("{name}.phpt"));
    }
}

#[test]
fn reconstruct_with_retained_child() {
    let mut t = T::new("reconstruct_with_retained_child");
    let xml = t.load_string("<root><child>old</child></root>");
    let child = t.prop(&xml, "child");
    let sx = t.sxe(&xml);
    Sxe::construct(&sx, b"<root><new>new</new></root>", 0, false, b"", false).unwrap();
    let n = t.prop(&xml, "new");
    let st = t.str(&n);
    t.var_dump(&s(&st));
    let st = t.str(&child);
    t.var_dump(&s(&st));
    let x = t.as_xml(&child);
    t.var_dump(&x);
    t.expect("reconstruct_with_retained_child.phpt");
}

#[test]
fn simplexml_uninitialized() {
    let mut t = T::new("simplexml_uninitialized");
    let sx = Sxe::new("MySXE");
    let v = common::new_object("MySXE", Some(sx.clone()));
    let r = Sxe::count(&sx).map(Value::Int);
    if let Err(e) = r {
        t.exception(&e);
    }
    if let Err(e) = Sxe::xpath(&sx, &mut t.h, b"") {
        t.exception(&e);
    }
    if let Err(e) = Sxe::get_doc_namespaces(&sx, false, true) {
        t.exception(&e);
    }
    if let Err(e) = Sxe::children(&sx, &mut t.h, None, false) {
        t.exception(&e);
    }
    if let Err(e) = Sxe::attributes(&sx, &mut t.h, None, false) {
        t.exception(&e);
    }
    if let Err(e) = Sxe::register_xpath_namespace(&sx, b"", b"") {
        t.exception(&e);
    }
    if let Err(e) = Sxe::read_property(&sx, &mut t.h, b"foo", FetchMode::Read) {
        t.exception(&e);
    }
    drop(v);
    t.expect("simplexml_uninitialized.phpt");
}

#[test]
fn compare_uninitialized() {
    let mut t = T::new("bug_sxe_compare_uninitialized");
    let a = Sxe::new("MySXE");
    let b = Sxe::new("MySXE");
    let av = common::new_object("MySXE", Some(a.clone()));
    let bv = common::new_object("MySXE", Some(b.clone()));
    t.echo("self: ");
    let r = php_runtime::ops::is_equal(&av, &av, &mut t.h).unwrap();
    t.var_dump(&Value::Bool(r));
    t.echo("equal: ");
    let r = php_runtime::ops::is_equal(&av, &bv, &mut t.h).unwrap();
    t.var_dump(&Value::Bool(r));
    t.echo("identical: ");
    t.var_dump(&Value::Bool(false));
    let c = t.load_string("<r/>");
    t.echo("uninit vs init: ");
    let r = php_runtime::ops::is_equal(&av, &c, &mut t.h).unwrap();
    t.var_dump(&Value::Bool(r));
    t.echo("done\n");
    t.expect("bug_sxe_compare_uninitialized.phpt");
}

#[test]
fn get_prop_address_not_initialized() {
    let mut t = T::new("get_prop_address_not_initialized");
    let sx = Sxe::new("SimpleXMLElement");
    let e = Sxe::property_ptr(&sx, &mut t.h, b"a").unwrap_err();
    assert_eq!(e.message(), Some("SimpleXMLElement is not properly initialized"));
}

#[test]
fn libxml_global_state_entity_loader_bypass() {
    let mut t = T::new("libxml_global_state_entity_loader_bypass");
    php_libxml::error::use_internal_errors(Some(true));
    let xml = "<?xml version='1.0'?><!DOCTYPE root [<!ENTITY % bork SYSTEM \"php://nope\"> %bork;]><nothing/>";
    let v = t.load_string(xml);
    t.var_dump(&v);
    let out = t.output();
    assert!(out.starts_with("object(SimpleXMLElement)"), "{out}");
}

#[test]
fn t023_entity_in_attribute() {
    let mut t = T::new("023");
    let xml = "<?xml version='1.0'?>\n<!DOCTYPE talks SYSTEM \"nbsp.dtd\" [\n<!ELEMENT root  EMPTY>\n<!ATTLIST root  attr1 CDATA #IMPLIED>\n<!ENTITY  nbsp   \"&#38;#x00A0;\">\n]>\n<root attr='foo&nbsp;bar&nbsp;baz'></root>";
    let sxe = t.load_string(xml);
    t.var_dump(&sxe);
    let a = t.dim(&sxe, s("attr"));
    t.var_dump(&a);
    t.expect("023.phpt");
}

#[test]
fn gh12223_nested_entities() {
    let mut t = T::new("gh12223");
    let xml = "<?xml version=\"1.0\"?>\n<!DOCTYPE somedoc [\n  <!ENTITY a \"something\">\n  <!ENTITY b \"&a;\">\n  <!ENTITY c \"&b;\">\n]>\n<somedoc>&c;</somedoc>";
    let sxe = t.load_string(xml);
    t.var_dump(&sxe);
    t.print_r(&sxe);
    t.expect("gh12223.phpt");
}

#[test]
fn gh15837_leaves_with_xpath() {
    let mut t = T::new("gh15837");
    let xml = "<xml>\n<fieldset1>\n</fieldset1>\n<fieldset2>\n<options>\n</options>\n</fieldset2>\n</xml>";
    let sxe = t.new_sxe_ex("SimpleXMLIterator", xml, 0, false, "", false).unwrap();
    for (_, child) in t.rii(&sxe, 0) {
        let _ = t.xpath(&child, "ancestor-or-self::*");
    }
    let sx = t.sxe(&sxe);
    assert!(!Sxe::valid(&sx));
}

#[test]
fn gh16808_current_before_rewind() {
    let mut t = T::new("gh16808");
    let sxe = t.new_sxe("<root />");
    let sx = t.sxe(&sxe);
    assert!(Sxe::iter_current(&sx).is_none());
}

#[test]
fn simplexml_import_old_dom() {
    let mut t = T::new("simplexml_import_old_dom");
    let doc = php_libxml::tree::Document::parse_file(
        fixture("book.xml").as_bytes(),
        &php_libxml::tree::ParseOptions::default(),
    )
    .unwrap();
    let sv = Sxe::import_dom(&mut t.h, Some(doc.as_node()), None).unwrap();
    let books = t.prop(&sv, "book");
    t.foreach(&books, |t, _, book| {
        let title = t.prop(&book, "title");
        let author = t.prop(&book, "author");
        let line = format!("{} was written by {}\n", t.str(&title), t.str(&author));
        t.echo(line);
    });
    t.expect("simplexml_import_old_dom.phpt");
}

#[test]
fn bug79971_1_nul_in_uri() {
    let mut t = T::new("bug79971_1");
    php_libxml::io::set_stream_hooks(Some(php_libxml::io::StreamHooks {
        open_read: Some(Box::new(|p: &[u8]| {
            let path = String::from_utf8_lossy(p).into_owned();
            std::fs::File::open(path).ok().map(|f| php_libxml::io::InputStream {
                reader: Box::new(f),
                charset: None,
            })
        })),
        open_write: None,
    }));
    t.func("simplexml_load_file");
    let uri = format!("file://{}%00foo", fixture("bug79971_1.xml"));
    let v = t.load_file(&uri);
    t.var_dump(&v);
    php_libxml::io::set_stream_hooks(None);
    let out = t.output();
    assert!(out.ends_with("bool(false)\n"), "{out}");
    assert!(out.contains("URI must not contain percent-encoded NUL bytes"), "{out}");
}
