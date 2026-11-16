// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Ports of `ext/simplexml/tests/profile*.phpt`.

#![allow(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    clippy::needless_pass_by_value,
    clippy::doc_markdown,
    clippy::format_push_string,
    clippy::items_after_statements,
    clippy::redundant_closure,
    clippy::type_complexity
)]

mod common;

use common::{s, T};
use php_runtime::Value;

const NS_CHILD: &str = "<?xml version=\"1.0\"?>\n<root xmlns:reserved=\"reserved-ns\">\n <reserved:child>Hello</reserved:child>\n</root>\n";
const NS_ATTR: &str = "<?xml version=\"1.0\"?>\n<root xmlns:reserved=\"reserved-ns\">\n <child reserved:attribute=\"Sample\" />\n</root>\n";

fn echo_value(t: &mut T, v: &Value) {
    let st = t.str(v);
    t.echo(st);
}

#[test]
fn profile01() {
    let mut t = T::new("profile01");
    let root = t.load_string("<?xml version=\"1.0\"?>\n<root>\n <child>Hello</child>\n</root>\n");
    let c = t.prop(&root, "child");
    echo_value(&mut t, &c);
    t.echo("\n---Done---\n");
    t.expect("profile01.phpt");
}

#[test]
fn profile02() {
    let mut t = T::new("profile02");
    let root = t.load_string(
        "<?xml version=\"1.0\"?>\n<root>\n <child>Hello</child>\n <child>World</child>\n</root>\n",
    );
    let c = t.prop(&root, "child");
    t.foreach(&c, |t, _, child| {
        let st = t.str(&child);
        t.echo(st + " ");
    });
    t.echo("\n---Done---\n");
    t.expect("profile02.phpt");
}

#[test]
fn profile03() {
    let mut t = T::new("profile03");
    let root = t
        .load_string("<?xml version=\"1.0\"?>\n<root>\n <child attribute=\"Sample\" />\n</root>\n");
    let c = t.prop(&root, "child");
    let a = t.dim(&c, s("attribute"));
    echo_value(&mut t, &a);
    t.echo("\n---Done---\n");
    t.expect("profile03.phpt");
}

#[test]
fn profile04_05_09() {
    for (name, ns) in [
        ("profile04", Some("reserved-ns")),
        ("profile05", Some("reserved")),
        ("profile09", None),
    ] {
        let mut t = T::new(name);
        let root = t.load_string(NS_CHILD);
        let c = match ns {
            Some(ns) => t.children(&root, Some(ns), false),
            None => root.clone(),
        };
        let v = t.prop(&c, "child");
        echo_value(&mut t, &v);
        t.echo("\n---Done---\n");
        t.expect(&format!("{name}.phpt"));
    }
}

#[test]
fn profile06_07_08() {
    let mut t = T::new("profile06");
    let root = t.load_string(NS_ATTR);
    let c = t.prop(&root, "child");
    let attr = t.attributes(&c, Some("reserved-ns"), false);
    let a = t.dim(&attr, s("attribute"));
    echo_value(&mut t, &a);
    t.echo("\n---Done---\n");
    t.expect("profile06.phpt");

    let mut t = T::new("profile07");
    let root = t.load_string(NS_ATTR);
    let c = t.prop(&root, "child");
    let rs = t.attributes(&c, Some("reserved"), false);
    let my = t.attributes(&c, Some("reserved-ns"), false);
    for x in [&rs, &my] {
        let a = t.dim(x, s("attribute"));
        echo_value(&mut t, &a);
    }
    t.echo("\n---Done---\n");
    t.expect("profile07.phpt");

    let mut t = T::new("profile08");
    let root = t.load_string(NS_ATTR);
    let c = t.prop(&root, "child");
    let a = t.dim(&c, s("attribute"));
    echo_value(&mut t, &a);
    t.echo("\n---Done---\n");
    t.expect("profile08.phpt");
}

#[test]
fn profile10() {
    let mut t = T::new("profile10");
    let root = t.load_string("<?xml version=\"1.0\"?>\n<root xmlns:reserved=\"reserved-ns\" xmlns:special=\"special-ns\">\n <child reserved:attribute=\"Sample\" special:attribute=\"Test\" />\n</root>\n");
    let c = t.prop(&root, "child");
    let rs = t.attributes(&c, Some("reserved-ns"), false);
    let sp = t.attributes(&c, Some("special-ns"), false);
    let a = t.dim(&rs, s("attribute"));
    echo_value(&mut t, &a);
    t.echo("\n");
    let a = t.dim(&sp, s("attribute"));
    echo_value(&mut t, &a);
    t.echo("\n---Done---\n");
    t.expect("profile10.phpt");
}

#[test]
fn profile11() {
    let mut t = T::new("profile11");
    let root = t.load_string("<?xml version=\"1.0\"?>\n<root xmlns:reserved=\"reserved-ns\" xmlns:special=\"special-ns\">\n <reserved:child>Hello</reserved:child>\n <special:child>World</special:child>\n</root>\n");
    for ns in ["reserved-ns", "special-ns"] {
        let c = t.children(&root, Some(ns), false);
        let v = t.prop(&c, "child");
        drop(c);
        t.var_dump(&v);
    }
    for ns in ["reserved-ns", "special-ns"] {
        let c = t.children(&root, Some(ns), false);
        let v = t.prop(&c, "child");
        let st = t.str(&v);
        t.var_dump(&s(&st));
    }
    let v = t.prop(&root, "child");
    t.var_dump(&v);
    t.expect("profile11.phpt");
}

const SOAP: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<soap:Envelope\nxmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\"\nxmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\nxmlns:xsd=\"http://www.w3.org/2001/XMLSchema\"\n>\n<soap:Body>\n<businessList foo=\"bar\">\n<businessInfo businessKey=\"bla\"/>\n</businessList>\n</soap:Body>\n</soap:Envelope>";

fn soap_tail(t: &mut T, sxe: &Value) {
    let b = t.prop(sxe, "Body");
    t.var_dump(&b);
    drop(b);
    let b = t.prop(sxe, "Body");
    let c = t.children(&b, Some(""), false);
    drop(b);
    t.var_dump(&c);
    drop(c);
    let b = t.prop(sxe, "Body");
    let c = t.children(&b, Some(""), false);
    drop(b);
    let l = t.prop(&c, "businessList");
    drop(c);
    t.var_dump(&l);
}

#[test]
fn profile12() {
    let mut t = T::new("profile12");
    let sxe = t.load_string(SOAP);
    let sx = t.sxe(&sxe);
    let nsl = php_simplexml::Sxe::get_namespaces(&sx, false).unwrap();
    drop(sx);
    t.var_dump(&Value::Array(nsl.clone()));
    let soap = nsl
        .get_str(b"soap")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string_lossy();
    drop(sxe);
    let sxe = t.load_string_ex(SOAP, None, 0, &soap, false);
    soap_tail(&mut t, &sxe);
    t.expect("profile12.phpt");
}

#[test]
fn profile13() {
    let mut t = T::new("profile13");
    let sxe = t.load_string(SOAP);
    let c = t.children(&sxe, Some("soap"), true);
    t.var_dump(&c);
    drop(c);
    drop(sxe);
    let sxe = t.load_string_ex(SOAP, None, 0, "soap", true);
    soap_tail(&mut t, &sxe);
    t.expect("profile13.phpt");
}
