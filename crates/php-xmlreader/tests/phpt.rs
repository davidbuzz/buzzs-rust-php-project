//! Hand ports of `ext/xmlreader/tests/*.phpt`.

#![allow(clippy::format_push_string)]

use php_libxml::error as lxerr;
use php_libxml::tree::{Document, ParseOptions};
use php_xmlreader::{
    node_type, parser_prop, CollectingHost, ErrorClass, PropValue, Property, ReaderError, XmlReader,
};
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src/ext/xmlreader/tests")
}

fn fixture(name: &str) -> Vec<u8> {
    fixtures()
        .join(name)
        .to_string_lossy()
        .into_owned()
        .into_bytes()
}

fn tmp_file(name: &str, contents: &str) -> Vec<u8> {
    let dir = std::env::temp_dir().join(format!("php-xmlreader-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, contents).unwrap();
    p.to_string_lossy().into_owned().into_bytes()
}

fn s(v: PropValue) -> String {
    match v {
        PropValue::Str(s) => String::from_utf8_lossy(&s).into_owned(),
        PropValue::Int(i) => i.to_string(),
        PropValue::Bool(b) => {
            if b {
                "1".into()
            } else {
                String::new()
            }
        }
    }
}

fn name(r: &XmlReader) -> String {
    s(r.read_property(Property::Name).unwrap())
}

fn value(r: &XmlReader) -> String {
    s(r.read_property(Property::Value).unwrap())
}

fn int(r: &XmlReader, p: Property) -> i64 {
    match r.read_property(p).unwrap() {
        PropValue::Int(i) => i,
        PropValue::Bool(b) => i64::from(b),
        PropValue::Str(_) => panic!("string property"),
    }
}

fn loaded(xml: &str) -> XmlReader {
    let mut r = XmlReader::new();
    let mut h = CollectingHost::default();
    assert!(r.xml(xml.as_bytes(), None, 0, &mut h).unwrap());
    r
}

fn reset_libxml() {
    lxerr::reset_request_state();
    lxerr::set_diagnostic_handler(None);
    lxerr::take_diagnostics();
}

fn diag_messages() -> Vec<String> {
    lxerr::take_diagnostics()
        .into_iter()
        .map(|d| String::from_utf8_lossy(&d.message).into_owned())
        .collect()
}

const BOOKS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<books></books>";
const BOOK_ATTRS: &str =
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<books><book num=\"1\" idx=\"2\">book1</book></books>";

#[test]
fn t001_xml_and_empty_source() {
    let mut r = loaded(BOOKS);
    let mut names = vec![];
    while r.read().unwrap() {
        names.push(name(&r));
    }
    assert_eq!(names, ["books", "books"]);
    let mut r = XmlReader::new();
    let mut h = CollectingHost::default();
    let e = r.xml(b"", None, 0, &mut h).unwrap_err();
    assert_eq!(e.class, ErrorClass::ValueError);
    assert_eq!(
        e.message,
        "XMLReader::XML(): Argument #1 ($source) must not be empty"
    );
}

#[test]
fn t002_open_file() {
    let mut r = XmlReader::new();
    let mut h = CollectingHost::default();
    let e = r.open(b"", None, 0, &mut h).unwrap_err();
    assert_eq!(
        e.message,
        "XMLReader::open(): Argument #1 ($uri) must not be empty"
    );
    let f = tmp_file("_002.xml", BOOKS);
    assert!(r.open(&f, None, 0, &mut h).unwrap());
    let mut names = vec![];
    while r.read().unwrap() {
        names.push(name(&r));
    }
    assert_eq!(names, ["books", "books"]);
    assert!(r.close());
    // An empty file opens fine (errors surface on read).
    let empty = tmp_file("_002_empty.xml", "");
    assert!(r.open(&empty, None, 0, &mut h).unwrap());
    assert!(h.messages.is_empty());
}

#[test]
fn t003_attributes() {
    let f = tmp_file("_003.xml", BOOK_ATTRS);
    let mut r = XmlReader::new();
    let mut h = CollectingHost::default();
    assert!(r.open(&f, None, 0, &mut h).unwrap());
    let mut out = String::new();
    while r.read().unwrap() {
        if int(&r, Property::NodeType) == node_type::ELEMENT
            && r.read_property(Property::HasAttributes).unwrap() == PropValue::Bool(true)
        {
            assert!(r.move_to_first_attribute());
            out += &format!("{}: {}\n", name(&r), value(&r));
            assert!(r.move_to_next_attribute());
            out += &format!("{}: {}\n", name(&r), value(&r));
            assert!(r.move_to_attribute(b"num").unwrap());
            out += &format!("{}: {}\n", name(&r), value(&r));
            assert_eq!(r.get_attribute(b"num").unwrap(), None); // on an attribute node
            assert!(r.move_to_attribute(b"idx").unwrap());
            out += &format!("{}: {}\n", name(&r), value(&r));
            assert!(r.move_to_attribute_no(0));
            out += &format!("{}: {}\n", name(&r), value(&r));
            assert!(r.move_to_attribute_no(1));
            out += &format!("{}: {}\n", name(&r), value(&r));
            assert!(!r.move_to_attribute_no(20));
            assert!(!r.move_to_attribute(b"missing-attribute").unwrap());
            let e = r.move_to_attribute(b"").unwrap_err();
            assert_eq!(
                e.message,
                "XMLReader::moveToAttribute(): Argument #1 ($name) must not be empty"
            );
        }
    }
    assert_eq!(out, "num: 1\nidx: 2\nnum: 1\nidx: 2\nnum: 1\nidx: 2\n");
}

#[test]
fn t003_get_errors() {
    let mut r = loaded(BOOK_ATTRS);
    while r.read().unwrap() {
        if int(&r, Property::NodeType) == node_type::ELEMENT && name(&r) == "book" {
            assert!(r.move_to_next_attribute());
            assert_eq!(format!("{}: {}", name(&r), value(&r)), "num: 1");
            let e = r.get_attribute(b"").unwrap_err();
            assert_eq!(
                e.to_string(),
                "ValueError: XMLReader::getAttribute(): Argument #1 ($name) must not be empty"
            );
            assert_eq!(r.get_attribute(b"isbn").unwrap(), None);
            assert_eq!(r.get_attribute_no(911), None);
            assert_eq!(format!("{}: {}", name(&r), value(&r)), "num: 1");
        }
    }
}

#[test]
fn t004_iterate_attributes() {
    let mut r = loaded(BOOK_ATTRS);
    let mut out = String::new();
    while r.read().unwrap() {
        if int(&r, Property::NodeType) != node_type::END_ELEMENT {
            out += &format!("{}\n", name(&r));
            if int(&r, Property::NodeType) == node_type::ELEMENT
                && int(&r, Property::HasAttributes) == 1
            {
                let mut attr = r.move_to_first_attribute();
                while attr {
                    out += &format!("   Attribute Name: {}\n", name(&r));
                    out += &format!("   Attribute Value: {}\n", value(&r));
                    attr = r.move_to_next_attribute();
                }
            }
        }
    }
    assert_eq!(
        out,
        "books\nbook\n   Attribute Name: num\n   Attribute Value: 1\n   Attribute Name: idx\n   Attribute Value: 2\n#text\n"
    );
}

#[test]
fn t005_parser_properties() {
    let mut r = loaded(BOOKS);
    assert!(r.set_parser_property(parser_prop::LOADDTD, false).unwrap());
    assert!(!r.get_parser_property(parser_prop::LOADDTD).unwrap());
    assert!(r
        .set_parser_property(parser_prop::SUBST_ENTITIES, true)
        .unwrap());
    assert!(r.get_parser_property(parser_prop::SUBST_ENTITIES).unwrap());
    while r.read().unwrap() {}
    let e = r.get_parser_property(99).unwrap_err();
    assert_eq!(
        e.message,
        "XMLReader::getParserProperty(): Argument #1 ($property) must be a valid parser property"
    );
}

#[test]
fn t006_move_to_element() {
    let mut r = loaded(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<books><book num=\"1\"></book><test /></books>",
    );
    r.read().unwrap();
    r.read().unwrap();
    assert!(r.move_to_first_attribute());
    assert!(r.move_to_element());
    assert_eq!(name(&r), "book");
}

#[test]
fn t007_relaxng() {
    let xml = "<TEI.2>hello</TEI.2>";
    let rng = fixture("relaxNG.rng");
    let mut h = CollectingHost::default();
    let f = tmp_file("_007.xml", xml);
    let mut r = XmlReader::new();
    r.open(&f, None, 0, &mut h).unwrap();
    assert!(r.set_relaxng_schema(Some(&rng), &mut h).unwrap());
    while r.read().unwrap() {}
    assert!(r.is_valid());
    r.close();

    let mut r = loaded(xml);
    assert!(r.set_relaxng_schema(Some(&rng), &mut h).unwrap());
    while r.read().unwrap() {}
    assert!(r.is_valid());

    let mut r = loaded(xml);
    let e = r.set_relaxng_schema(Some(b""), &mut h).unwrap_err();
    assert_eq!(
        e.message,
        "XMLReader::setRelaxNGSchema(): Argument #1 ($filename) must not be empty"
    );
    assert!(h.messages.is_empty());
}

#[test]
fn t007_relaxng_source() {
    let rng =
        br#"<element name="TEI.2" xmlns="http://relaxng.org/ns/structure/1.0"><text/></element>"#;
    let mut h = CollectingHost::default();
    let mut r = loaded("<TEI.2>hello</TEI.2>");
    assert!(r.set_relaxng_schema_source(Some(rng), &mut h).unwrap());
    while r.read().unwrap() {}
    assert!(r.is_valid());
    // Unsetting is allowed before reading only.
    let mut r = loaded("<TEI.2>hello</TEI.2>");
    assert!(r.set_relaxng_schema_source(None, &mut h).unwrap());
    // Without data: Error.
    let mut r = XmlReader::new();
    let e = r.set_relaxng_schema_source(Some(rng), &mut h).unwrap_err();
    assert_eq!(e.message, "Schema must be set prior to reading");
}

#[test]
fn t008_dtd_validation() {
    let dtd_dir = fixtures().canonicalize().unwrap();
    let body = r#"<LIST>
<MOVIE ID="x200338360">
<TITLE>Move Title 1</TITLE>
<ORGTITLE/><LOC>Location 1</LOC>
<INFO/>
</MOVIE>
<MOVIE ID="m200338361">
<TITLE>Move Title 2</TITLE>
<ORGTITLE/>
<LOC>Location 2</LOC>
<INFO/>
</MOVIE>
</LIST>"#;
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n<!DOCTYPE LIST SYSTEM \"file://{}/dtdexample.dtd\">\n{body}",
        dtd_dir.display()
    );
    let mut r = loaded(&xml);
    r.set_parser_property(parser_prop::LOADDTD, true).unwrap();
    r.set_parser_property(parser_prop::VALIDATE, true).unwrap();
    while r.read().unwrap() {}
    assert!(r.is_valid());
}

#[test]
fn t009_t010_next() {
    let mut r = loaded(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<books><book num=\"1\"><test /></book><book num=\"2\" /></books>",
    );
    r.read().unwrap();
    r.read().unwrap();
    r.next(None).unwrap();
    assert_eq!(name(&r), "book");
    assert_eq!(r.get_attribute(b"num").unwrap().unwrap(), b"2");

    let mut r = loaded(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<prefix:books xmlns:prefix=\"uri\" isbn=\"\" prefix:isbn=\"12isbn\">book1</prefix:books>",
    );
    r.read().unwrap();
    r.read().unwrap();
    r.next(None).unwrap();
    assert_eq!(name(&r), "prefix:books");
    assert_eq!(
        r.get_attribute_ns(b"isbn", b"uri").unwrap().unwrap(),
        b"12isbn"
    );
}

#[test]
fn t011_inner_outer_xml() {
    let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<books><book>test</book></books>";
    let mut r = loaded(xml);
    r.read().unwrap();
    assert_eq!(r.read_inner_xml(), b"<book>test</book>");
    let mut r = loaded(xml);
    r.read().unwrap();
    assert_eq!(r.read_outer_xml(), b"<books><book>test</book></books>");
}

#[test]
fn t012_default_attributes() {
    let mut r = loaded("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<foo bar=\"\"/>");
    r.read().unwrap();
    assert_eq!(r.get_attribute(b"bar").unwrap().unwrap(), b"");
    assert_eq!(r.get_attribute(b"baz").unwrap(), None);

    let dtd = fixtures().join("012.dtd").canonicalize().unwrap();
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE foo SYSTEM \"{}\">\n<foo bar=\"\"/>",
        dtd.display()
    );
    let mut r = loaded(&xml);
    r.set_parser_property(parser_prop::DEFAULTATTRS, true)
        .unwrap();
    while r.read().unwrap() && int(&r, Property::NodeType) != node_type::ELEMENT {}
    assert_eq!(r.get_attribute(b"bar").unwrap().unwrap(), b"");
    assert_eq!(r.get_attribute(b"baz").unwrap().unwrap(), b"");

    let mut h = CollectingHost::default();
    let mut r = XmlReader::new();
    r.open(&fixture("012.xml"), None, 0, &mut h).unwrap();
    while r.read().unwrap() && int(&r, Property::NodeType) != node_type::ELEMENT {}
    assert_eq!(r.get_attribute(b"bar").unwrap().unwrap(), b"");
    assert_eq!(r.get_attribute(b"baz").unwrap(), None);

    let mut r = XmlReader::new();
    r.open(&fixture("012.xml"), None, 0, &mut h).unwrap();
    r.set_parser_property(parser_prop::DEFAULTATTRS, true)
        .unwrap();
    while r.read().unwrap() && int(&r, Property::NodeType) != node_type::ELEMENT {}
    assert_eq!(r.get_attribute(b"bar").unwrap().unwrap(), b"");
    assert_eq!(r.get_attribute(b"baz").unwrap().unwrap(), b"");
}

#[test]
fn t013_schema_validation() {
    reset_libxml();
    let xsd = fixture("013.xsd");
    let mut h = CollectingHost::default();
    let mut r = loaded(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<items>\n  <item>123</item>\n  <item>456</item>\n</items>",
    );
    assert!(r.set_schema(Some(&xsd), &mut h).unwrap());
    let mut values = vec![];
    while r.read().unwrap() {
        if int(&r, Property::NodeType) == node_type::ELEMENT && name(&r) == "item" {
            r.read().unwrap();
            values.push(value(&r));
        }
    }
    assert_eq!(values, ["123", "456"]);
    assert!(diag_messages().is_empty());

    let mut r = loaded("<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<foo/>");
    assert!(r.set_schema(Some(&xsd), &mut h).unwrap());
    while r.read().unwrap() && int(&r, Property::NodeType) != node_type::ELEMENT {}
    let msgs = diag_messages();
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(msgs[0].starts_with("Element 'foo': "), "{msgs:?}");
    reset_libxml();
}

#[test]
fn t014_readonly_properties() {
    assert_eq!(
        XmlReader::write_property_error("XMLReader", Property::Value).to_string(),
        "Error: Cannot modify readonly property XMLReader::$value"
    );
    assert_eq!(
        XmlReader::unset_property_error("MyXMLReader", Property::AttributeCount).message,
        "Cannot unset MyXMLReader::$attributeCount"
    );
}

#[test]
fn t015_get_attribute_ns_errors() {
    let r = loaded("<a/>");
    let e = r.get_attribute_ns(b"idx", b"").unwrap_err();
    assert_eq!(
        e.message,
        "XMLReader::getAttributeNs(): Argument #2 ($namespace) must not be empty"
    );
    let e = r.get_attribute_ns(b"", b"x").unwrap_err();
    assert_eq!(
        e.message,
        "XMLReader::getAttributeNs(): Argument #1 ($name) must not be empty"
    );
}

#[test]
fn t015_namespaced_attributes() {
    let mut r = loaded(
        r#"<books xmlns:ns1="http://www.ns1.namespace.org/" xmlns:ns2="http://www.ns2.namespace.org/"><book ns1:num="1" ns2:idx="2" ns1:idx="3" ns2:isbn="4">book1</book></books>"#,
    );
    while r.read().unwrap() {
        if name(&r) == "book" && int(&r, Property::NodeType) == node_type::ELEMENT {
            assert_eq!(
                r.get_attribute_ns(b"idx", b"http://www.ns1.namespace.org/")
                    .unwrap()
                    .unwrap(),
                b"3"
            );
            assert!(r
                .move_to_attribute_ns(b"isbn", b"http://www.ns2.namespace.org/")
                .unwrap());
            assert_eq!(name(&r), "ns2:isbn");
            assert_eq!(value(&r), "4");
            assert_eq!(s(r.read_property(Property::Prefix).unwrap()), "ns2");
            assert_eq!(s(r.read_property(Property::LocalName).unwrap()), "isbn");
            assert_eq!(
                s(r.read_property(Property::NamespaceUri).unwrap()),
                "http://www.ns2.namespace.org/"
            );
            assert_eq!(
                r.lookup_namespace(b"ns1").unwrap().unwrap(),
                b"http://www.ns1.namespace.org/"
            );
            break;
        }
    }
}

#[test]
fn bug42139_entities() {
    let xml = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE root [\n<!ELEMENT root ANY>\n<!ENTITY x \"y\">\n]>\n<root>&x;</root>";
    let mut r = XmlReader::new();
    let mut h = CollectingHost::default();
    r.xml(
        xml.as_bytes(),
        None,
        i64::from(php_libxml::constants::LIBXML_NOENT),
        &mut h,
    )
    .unwrap();
    let mut out = String::new();
    while r.read().unwrap() {
        out += &format!(
            "{}, {}, {}\n",
            int(&r, Property::NodeType),
            name(&r),
            value(&r)
        );
    }
    assert_eq!(out, "10, root, \n1, root, \n3, #text, y\n15, root, \n");
}

#[test]
fn bug64230_errors() {
    reset_libxml();
    lxerr::use_internal_errors(Some(true));
    let mut r = loaded("<root att/>");
    r.read().unwrap();
    let errors = lxerr::get_errors();
    assert!(!errors.is_empty());
    assert!(String::from_utf8_lossy(&errors[0].message).starts_with("Specification mandate"));
    lxerr::clear_errors();
    lxerr::use_internal_errors(Some(false));
    assert!(diag_messages().is_empty());

    let mut r = loaded("<root att/>");
    r.read().unwrap();
    let msgs = diag_messages();
    assert_eq!(msgs.len(), 3, "{msgs:?}");
    assert!(
        msgs[0].contains(": parser error : Specification mandate"),
        "{msgs:?}"
    );
    assert!(msgs[0].ends_with("value for attribute att"), "{msgs:?}");
    assert_eq!(msgs[1], "<root att/>");
    assert_eq!(msgs[2], "         ^");
    reset_libxml();
}

#[test]
fn bug71805_noblanks() {
    for f in ["XMLReaderGood_bug71805.xml", "XMLReaderBad_bug71805.xml"] {
        let mut r = XmlReader::new();
        let mut h = CollectingHost::default();
        r.open(
            &fixture(f),
            None,
            i64::from(php_libxml::constants::LIBXML_NOBLANKS),
            &mut h,
        )
        .unwrap();
        let mut last = r.read().unwrap();
        while last && name(&r) != "records" {
            last = r.read().unwrap();
        }
        last = r.read().unwrap();
        while last && name(&r) != "record" {
            last = r.read().unwrap();
        }
        while last {
            assert!(!r.read_outer_xml().is_empty(), "{f}");
            last = r.next(None).unwrap();
            while last && name(&r) != "record" {
                last = r.next(None).unwrap();
            }
        }
    }
}

#[test]
fn bug73053_schema_with_huge() {
    let mut r = XmlReader::new();
    let mut h = CollectingHost::default();
    assert!(r
        .open(
            &fixture("bug73053.xml"),
            None,
            i64::from(php_libxml::constants::LIBXML_PARSEHUGE),
            &mut h
        )
        .unwrap());
    r.set_schema(Some(&fixture("bug73053.xsd")), &mut h)
        .unwrap();
    while r.read().unwrap() {}
    assert!(h.messages.is_empty());
}

#[test]
fn bug73246_null_bytes() {
    let mut r = XmlReader::new();
    let mut h = CollectingHost::default();
    assert_eq!(
        r.open(b"x.xml", Some(b"UTF\0-8"), 0, &mut h)
            .unwrap_err()
            .message,
        "XMLReader::open(): Argument #2 ($encoding) must not contain any null bytes"
    );
    assert_eq!(
        r.xml(b"<root/>", Some(b"UTF\0-8"), 0, &mut h)
            .unwrap_err()
            .message,
        "XMLReader::XML(): Argument #2 ($encoding) must not contain any null bytes"
    );
    assert_eq!(
        XmlReader::from_stream(Box::new(std::io::empty()), Some(b"UTF\0-8"), 0, None)
            .unwrap_err()
            .message,
        "XMLReader::fromStream(): Argument #2 ($encoding) must not contain any null bytes"
    );
}

#[test]
fn bug81521_parser_props_unloaded() {
    let mut r = XmlReader::new();
    let msg = "Cannot access parser properties before loading data";
    assert_eq!(
        r.set_parser_property(parser_prop::LOADDTD, true)
            .unwrap_err()
            .message,
        msg
    );
    assert_eq!(
        r.get_parser_property(parser_prop::LOADDTD)
            .unwrap_err()
            .message,
        msg
    );
}

#[test]
fn invalid_encoding() {
    let mut r = XmlReader::new();
    let mut h = CollectingHost::default();
    assert_eq!(
        r.open(b"x.xml", Some(b"does not exist"), 0, &mut h)
            .unwrap_err()
            .message,
        "XMLReader::open(): Argument #2 ($encoding) must be a valid character encoding"
    );
    assert_eq!(
        XmlReader::from_stream(Box::new(std::io::empty()), Some(b"does not exist"), 0, None)
            .unwrap_err()
            .message,
        "XMLReader::fromStream(): Argument #2 ($encoding) must be a valid character encoding"
    );
    assert_eq!(
        r.xml(
            b"<?xml version=\"1.0\"?><root/>",
            Some(b"does not exist"),
            0,
            &mut h
        )
        .unwrap_err()
        .message,
        "XMLReader::XML(): Argument #2 ($encoding) must be a valid character encoding"
    );
}

#[test]
fn open_error_missing_file() {
    reset_libxml();
    let mut r = XmlReader::new();
    let mut h = CollectingHost::default();
    let missing = fixture("missing-file.xml");
    assert!(!r.open(&missing, None, 0, &mut h).unwrap());
    assert_eq!(
        h.messages,
        [(true, "Unable to open source data".to_owned())]
    );
    assert!(r.close());
    assert_eq!(
        XmlReader::from_uri(b"nonexistent", None, 0).unwrap_err(),
        ReaderError::error("Unable to open source data")
    );
    assert!(XmlReader::open_static(&missing, None, 0, &mut h)
        .unwrap()
        .is_none());
    reset_libxml();
}

#[test]
fn expand_into_document() {
    let base = Document::parse_memory(
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<books><book>base book</book></books>",
        &ParseOptions::default(),
    )
    .unwrap();
    let mut r =
        loaded("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<books><book>new book</book></books>");
    let mut h = CollectingHost::default();
    while r.read().unwrap() {
        if s(r.read_property(Property::LocalName).unwrap()) == "book" {
            let node = r.expand(Some(&base), &mut h).unwrap().unwrap();
            assert_eq!(node.document(), base);
            assert!(!node.is_attached());
            assert_eq!(node.dump(false, None), b"<book>new book</book>");
            let root = node.document().root().unwrap();
            assert_eq!(root.first_child().unwrap().content(), b"base book");
            break;
        }
    }
}

#[test]
fn expand_errors() {
    let mut r = XmlReader::new();
    let mut h = CollectingHost::default();
    assert_eq!(
        r.expand(None, &mut h).unwrap_err().message,
        "Data must be loaded before expanding"
    );
    let mut r =
        loaded("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<books><book>new book</book></books>");
    assert!(r.expand(None, &mut h).unwrap().is_none());
    assert_eq!(
        h.messages,
        [(true, "An Error Occurred while expanding".to_owned())]
    );
}

#[test]
fn gh19098_expand_namespaces() {
    let mut r = loaded(
        r#"
<sparql xmlns="http://www.w3.org/2005/sparql-results#">
 <results>
  <result><binding xml:id="foo" xmlns:custom="urn:custom" custom:foo="bar" name="s"><uri/></binding></result>
 </results>
</sparql>"#,
    );
    let mut h = CollectingHost::default();
    r.next(Some(b"sparql")).unwrap();
    r.read().unwrap();
    r.next(Some(b"results")).unwrap();
    while r.read().unwrap() {
        if r.next(Some(b"result")).unwrap() {
            let node = r.expand(None, &mut h).unwrap().unwrap();
            let child = node.first_child().unwrap();
            drop(node);
            assert_eq!(
                child.namespace().unwrap().href,
                b"http://www.w3.org/2005/sparql-results#"
            );
            let attrs: Vec<_> = child
                .attributes()
                .iter()
                .map(|a| a.namespace().map(|n| n.href))
                .collect();
            assert_eq!(
                attrs,
                [
                    Some(b"http://www.w3.org/XML/1998/namespace".to_vec()),
                    Some(b"urn:custom".to_vec()),
                    None
                ]
            );
            break;
        }
    }
}

#[test]
fn next_basic() {
    let mut r = XmlReader::new();
    assert_eq!(
        r.read().unwrap_err().message,
        "Data must be loaded before reading"
    );
    assert_eq!(
        r.next(None).unwrap_err().message,
        "Data must be loaded before reading"
    );
    r.close();
    let mut h = CollectingHost::default();
    r.xml(
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><nodes><node1><sub /></node1>\n<node2><sub /></node2><node3><sub /></node3><node4><sub /></node4></nodes>",
        None,
        0,
        &mut h,
    )
    .unwrap();
    r.read().unwrap();
    r.read().unwrap();
    assert_eq!(name(&r), "node1");
    assert!(r.next(Some(b"node3")).unwrap());
    assert_eq!(name(&r), "node3");
    assert!(r.next(None).unwrap());
    assert_eq!(name(&r), "node4");
    assert!(!r.next(Some(b"node5")).unwrap());
    assert_eq!(name(&r), "");
    assert!(!r.next(None).unwrap());
}

#[test]
fn read_string_basic() {
    let mut r = loaded("<?xml version=\"1.0\" encoding=\"UTF-8\"?><books><book>Book1</book><book>Book2</book></books>");
    r.read().unwrap();
    assert_eq!(r.read_string(), b"Book1Book2");
}

#[test]
fn static_constructors() {
    let f = tmp_file("static.xml", BOOKS);
    let mut h = CollectingHost::default();
    let mut r = XmlReader::new();
    r.attach(
        XmlReader::open_static(&f, None, 0, &mut h)
            .unwrap()
            .unwrap(),
    );
    let mut n = 0;
    while r.read().unwrap() {
        assert_eq!(name(&r), "books");
        n += 1;
    }
    assert_eq!(n, 2);
    let mut r = XmlReader::new();
    r.attach(
        XmlReader::xml_static(BOOKS.as_bytes(), None, 0, &mut h)
            .unwrap()
            .unwrap(),
    );
    let mut n = 0;
    while r.read().unwrap() {
        n += 1;
    }
    assert_eq!(n, 2);
    let mut r = XmlReader::new();
    r.attach(XmlReader::from_string(b"<root/>", None, 0).unwrap());
    assert!(r.read().unwrap());
    let mut r = XmlReader::new();
    r.attach(XmlReader::from_uri(&f, None, 0).unwrap());
    assert!(r.read().unwrap());
}

#[test]
fn from_stream_legit_usage() {
    let data = b"<root><!--my comment--><child/></root>".to_vec();
    let mut r = XmlReader::new();
    r.attach(
        XmlReader::from_stream(
            Box::new(std::io::Cursor::new(data)),
            Some(b"UTF-8"),
            0,
            None,
        )
        .unwrap(),
    );
    let mut out = String::new();
    while r.read().unwrap() {
        match int(&r, Property::NodeType) {
            node_type::ELEMENT => out += &format!("Element: {}\n", name(&r)),
            node_type::COMMENT => out += &format!("Comment: {}\n", value(&r)),
            _ => {}
        }
    }
    assert_eq!(out, "Element: root\nComment: my comment\nElement: child\n");
}

/// A stream that fails after its first chunk (`fromStream_broken_stream`).
struct Broken(Option<Vec<u8>>);

impl std::io::Read for Broken {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.0.as_mut() {
            Some(d) if !d.is_empty() => {
                let n = d.len().min(buf.len());
                buf[..n].copy_from_slice(&d[..n]);
                d.drain(..n);
                Ok(n)
            }
            _ => Err(std::io::Error::other("closed")),
        }
    }
}

#[test]
fn from_stream_broken_stream() {
    reset_libxml();
    lxerr::use_internal_errors(Some(true));
    let mut r = XmlReader::new();
    r.attach(
        XmlReader::from_stream(
            Box::new(Broken(Some(b"<root><!--my comment-->".to_vec()))),
            Some(b"UTF-8"),
            0,
            None,
        )
        .unwrap(),
    );
    let mut out = String::new();
    while r.read().unwrap() {
        match int(&r, Property::NodeType) {
            node_type::ELEMENT => out += &format!("Element: {}\n", name(&r)),
            node_type::COMMENT => out += &format!("Comment: {}\n", value(&r)),
            _ => {}
        }
    }
    assert_eq!(out, "Element: root\nComment: my comment\n");
    assert_eq!(int(&r, Property::Depth), 1);
    reset_libxml();
}

#[test]
fn var_dump_properties() {
    let mut r = XmlReader::new();
    r.attach(XmlReader::from_string(b"<root>hi</root>", None, 0).unwrap());
    // fromStream_custom_constructor: before reading, isEmptyElement fails.
    let (props, err) = r.debug_properties();
    assert_eq!(props.len(), 13);
    assert!(!props.iter().any(|(n, _)| *n == "isEmptyElement"));
    assert_eq!(
        err.unwrap().message,
        "Failed to read property because no XML data has been read yet"
    );
    assert!(r.read().unwrap());
    let (props, err) = r.debug_properties();
    assert!(err.is_none());
    let names: Vec<_> = props.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        names,
        [
            "attributeCount",
            "baseURI",
            "depth",
            "hasAttributes",
            "hasValue",
            "isDefault",
            "isEmptyElement",
            "localName",
            "name",
            "namespaceURI",
            "nodeType",
            "prefix",
            "value",
            "xmlLang"
        ]
    );
    assert_eq!(props[7].1, PropValue::Str(b"root".to_vec()));
    assert_eq!(props[10].1, PropValue::Int(1));
    assert_eq!(props[6].1, PropValue::Bool(false));
    if let PropValue::Str(base) = &props[1].1 {
        assert!(base.ends_with(b"/"));
    } else {
        panic!();
    }
}

#[test]
fn virtual_properties_unloaded() {
    use php_xmlreader::IssetKind;
    let r = XmlReader::new();
    for p in [
        Property::AttributeCount,
        Property::BaseUri,
        Property::Depth,
        Property::HasAttributes,
        Property::HasValue,
    ] {
        assert!(r.has_property(p, IssetKind::Isset).unwrap());
        assert!(!r.has_property(p, IssetKind::NotEmpty).unwrap());
        assert!(r.has_property(p, IssetKind::Exists).unwrap());
    }
    assert_eq!(
        r.read_property(Property::NodeType).unwrap(),
        PropValue::Int(0)
    );
}

#[test]
fn set_schema_errors() {
    let mut h = CollectingHost::default();
    let mut r = XmlReader::new();
    assert_eq!(
        r.set_schema(Some(b""), &mut h).unwrap_err().message,
        "XMLReader::setSchema(): Argument #1 ($filename) must not be empty"
    );
    assert_eq!(
        r.set_schema(Some(b"schema-missing-file.xsd"), &mut h)
            .unwrap_err()
            .message,
        "Schema must be set prior to reading"
    );
    reset_libxml();
    lxerr::use_internal_errors(Some(true));
    let mut r = loaded("<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<foo/>");
    assert!(!r
        .set_schema(Some(&fixture("schema-bad.xsd")), &mut h)
        .unwrap());
    assert_eq!(h.messages, [(true, "Schema contains errors".to_owned())]);
    reset_libxml();
}

#[test]
fn libxml_entity_loader_bypass() {
    reset_libxml();
    lxerr::use_internal_errors(Some(true));
    let xml = "<?xml version='1.0'?><!DOCTYPE root [<!ENTITY % bork SYSTEM \"php://nope\"> %bork;]><nothing/>";
    let mut r = loaded(xml);
    let _ = r.read();
    assert!(diag_messages().is_empty());
    reset_libxml();
}
