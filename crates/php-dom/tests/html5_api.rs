//! API-level tests of the lexbor-backed HTML5 parser, the HTML5 and XML
//! serializers and CSS selectors, through the generic dispatch layer, plus
//! a deterministic fuzz pass over the parsers/serializers.

use php_dom::dispatch::{call, call_static, read_property, write_property, DomObject, PropRead};
use php_dom::{DomValue, NoHost};

fn s(v: &str) -> DomValue {
    DomValue::Str(v.as_bytes().to_vec())
}

fn node_obj(v: DomValue) -> DomObject {
    match v {
        DomValue::Node(n) => DomObject::Node(n),
        other => panic!("expected a node, got {other:?}"),
    }
}

fn html(src: &str, options: i64) -> DomObject {
    let errors = php_dom::error::take_diagnostics();
    drop(errors);
    node_obj(call_static("Dom\\HTMLDocument", "createFromString", &[s(src), DomValue::Int(options)], &NoHost).expect("parse"))
}

fn xml(src: &str) -> DomObject {
    node_obj(call_static("Dom\\XMLDocument", "createFromString", &[s(src)], &NoHost).expect("parse"))
}

fn string(v: DomValue) -> String {
    match v {
        DomValue::Str(b) => String::from_utf8(b).expect("utf-8"),
        other => panic!("expected a string, got {other:?}"),
    }
}

fn save_html(doc: &DomObject) -> String {
    string(call(doc, "saveHtml", &[], &NoHost).expect("saveHtml"))
}

fn prop(obj: &DomObject, name: &str) -> DomValue {
    match read_property(obj, name).expect("read") {
        PropRead::Value(v) => v,
        _ => DomValue::Null,
    }
}

const NOERROR: i64 = 1 << 5;
const NOIMPLIED: i64 = 1 << 13;

#[test]
fn parses_and_serializes_html5() {
    let doc = html("<!doctype html><title>T</title><p class=a>x&amp;y<br>z", NOERROR);
    assert_eq!(
        save_html(&doc),
        "<!DOCTYPE html><html><head><title>T</title></head><body><p class=\"a\">x&amp;y<br>z</p></body></html>"
    );
    assert_eq!(string(prop(&doc, "title")), "T");
    assert_eq!(string(prop(&doc, "charset")), "UTF-8");
}

#[test]
fn noimplied_removes_implied_elements() {
    let doc = html("<p>hi</p>", NOERROR | NOIMPLIED);
    assert_eq!(save_html(&doc), "<p>hi</p>");
}

#[test]
fn template_contents_round_trip() {
    let doc = html("<template><b>x</b></template>", NOERROR);
    assert_eq!(
        save_html(&doc),
        "<html><head><template><b>x</b></template></head><body></body></html>"
    );
}

#[test]
fn encoding_detection_and_output_encoding() {
    let doc = html("<meta charset=\"windows-1252\"><p>\u{e9}", NOERROR);
    assert_eq!(string(prop(&doc, "charset")), "windows-1252");
    let doc = html("\u{feff}<p>\u{e9}</p>", NOERROR);
    assert_eq!(string(prop(&doc, "charset")), "UTF-8");
    assert!(save_html(&doc).contains("<p>\u{e9}</p>"));
}

#[test]
fn css_selectors() {
    let doc = html("<div id=a><p class=x>1</p><p>2</p><span class=x>3</span></div>", NOERROR);
    let list = call(&doc, "querySelectorAll", &[s(".x")], &NoHost).expect("qsa");
    let DomValue::NodeList(list) = list else { panic!("list") };
    assert_eq!(list.length(), 2);
    let first = call(&doc, "querySelector", &[s("div > p:nth-child(2)")], &NoHost).expect("qs");
    let DomValue::Node(p) = first else { panic!("node") };
    assert_eq!(string(prop(&DomObject::Node(p), "textContent")), "2");
    let err = call(&doc, "querySelector", &[s("p[")], &NoHost).expect_err("invalid selector");
    assert!(err.message().starts_with("Invalid selector"), "{}", err.message());
}

#[test]
fn inner_and_outer_html() {
    let doc = html("<div id=a></div>", NOERROR);
    let div = node_obj(call(&doc, "getElementById", &[s("a")], &NoHost).expect("byid"));
    write_property(&div, "innerHTML", &s("<i>x</i><!--c-->")).expect("innerHTML");
    assert_eq!(string(prop(&div, "innerHTML")), "<i>x</i><!--c-->");
    assert_eq!(string(prop(&div, "outerHTML")), "<div id=\"a\"><i>x</i><!--c--></div>");
}

#[test]
fn xml_serializer_namespaces() {
    let doc = xml("<root xmlns=\"urn:a\" xmlns:b=\"urn:b\"><b:c b:attr=\"1\"/><d/></root>");
    let out = string(call(&doc, "saveXml", &[], &NoHost).expect("saveXml"));
    assert_eq!(
        out,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<root xmlns=\"urn:a\" xmlns:b=\"urn:b\"><b:c b:attr=\"1\"/><d/></root>"
    );
}

#[test]
fn xml_inner_html_requires_well_formedness() {
    let doc = xml("<root/>");
    let root = node_obj(prop(&doc, "documentElement"));
    write_property(&root, "innerHTML", &s("<a>x</a>")).expect("innerHTML");
    assert_eq!(string(prop(&root, "innerHTML")), "<a>x</a>");
    let err = write_property(&root, "innerHTML", &s("<a>")).expect_err("not well-formed");
    assert_eq!(err.message(), "XML fragment is not well-formed");
}

/// A tiny deterministic PRNG for the fuzz pass.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

#[test]
fn fuzz_html_and_css() {
    const PIECES: &[&str] = &[
        "<", ">", "</", "p", "div", "template", "svg", "math", "table", "tr", "td", "<!--", "-->", "&amp;", "&", "\"",
        "'", "=", " ", "\n", "a", "<!DOCTYPE html>", "\u{e9}", "\u{fffd}", "\0", "\u{c2}", "script", "<![CDATA[", "]]>",
        "xmlns:x=\"urn:x\"", "x:y", "id", "class", "<br/>", "&#x110000;", "\u{a0}",
    ];
    const SEL: &[&str] = &["p", "div", "*", " ", ">", "+", "~", ".a", "#b", "[id]", "[class~=x]", ":first-child", ":not(", ")", ",", ":nth-child(2n+1)", "svg|a", "::before", ":has(", "\\", "\""];
    let mut rng = Lcg(0x5eed);
    for _ in 0..300 {
        let n = rng.next() % 40;
        let mut src = String::new();
        for _ in 0..n {
            src.push_str(PIECES[(rng.next() % PIECES.len() as u64) as usize]);
        }
        let options = [0, NOERROR, NOERROR | NOIMPLIED][(rng.next() % 3) as usize];
        let Ok(DomValue::Node(d)) = call_static("Dom\\HTMLDocument", "createFromString", &[s(&src), DomValue::Int(options)], &NoHost) else {
            continue;
        };
        let doc = DomObject::Node(d);
        let _ = call(&doc, "saveHtml", &[], &NoHost);
        let _ = call(&doc, "saveXml", &[], &NoHost);
        let mut sel = String::new();
        for _ in 0..(rng.next() % 6) {
            sel.push_str(SEL[(rng.next() % SEL.len() as u64) as usize]);
        }
        let _ = call(&doc, "querySelectorAll", &[s(&sel)], &NoHost);
        if let DomValue::Node(body) = prop(&doc, "body") {
            let body = DomObject::Node(body);
            let _ = write_property(&body, "innerHTML", &s(&src));
            let _ = read_property(&body, "outerHTML");
        }
        let _ = php_dom::error::take_diagnostics();
        let _ = php_libxml::error::take_diagnostics();
    }
}
