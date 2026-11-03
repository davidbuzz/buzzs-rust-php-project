//! Ports of `ext/simplexml/tests/gh*.phpt` and other named tests.

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
use php_simplexml::Sxe;

fn first(t: &mut T, v: &Value, q: &str) -> Value {
    let r = t.xpath(v, q);
    let x = r
        .as_array()
        .and_then(|a| a.values().next().cloned())
        .unwrap_or(Value::Null);
    x
}

#[test]
fn gh10200_get_object_vars() {
    let mut t = T::new("gh10200");
    let xml = t.load_string("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<document>https://github.com/php/php-src/issues/10200 not encountered</document>");
    let o = t.array(&xml);
    t.var_dump(&o);
    t.expect("gh10200.phpt");
}

#[test]
fn gh10983_attribute_dump() {
    let mut t = T::new("gh10983");
    let x = t.load_string("<form name=\"test\"></form>");
    let n = t.dim(&x, s("name"));
    t.var_dump(&n);
    t.expect("gh10983.phpt");
}

#[test]
fn gh12167_pi_name_and_string() {
    let mut t = T::new("gh12167");
    let sxe =
        t.load_string("<?xml version=\"1.0\"?>\n<container>\n  <?foo pi contents ?>\n</container>");
    let p = first(&mut t, &sxe, "//processing-instruction()");
    let n = t.get_name(&p);
    t.var_dump(&s(&n));
    let p = first(&mut t, &sxe, "//processing-instruction()");
    let st = t.str(&p);
    t.var_dump(&s(&st));
    t.expect("gh12167.phpt");
}

#[test]
fn gh12169_comment_name_and_string() {
    let mut t = T::new("gh12169");
    let sxe = t.load_string(
        "<?xml version=\"1.0\"?>\n<container>\n    <!-- comment contents -->\n</container>",
    );
    let p = first(&mut t, &sxe, "//comment()");
    let n = t.get_name(&p);
    t.var_dump(&s(&n));
    let p = first(&mut t, &sxe, "//comment()");
    let st = t.str(&p);
    t.var_dump(&s(&st));
    t.expect("gh12169.phpt");
}

fn eq(t: &mut T, a: &Value, b: &Value) -> Value {
    Value::Bool(php_runtime::ops::is_equal(a, b, &mut t.h).unwrap())
}

#[test]
fn gh12171_comparisons() {
    let mut t = T::new("gh12171");
    let xml = t.load_string("<root><first/><second/><third/></root>");
    t.echo("first==second: ");
    let (a, b) = (t.prop(&xml, "first"), t.prop(&xml, "second"));
    let r = eq(&mut t, &a, &b);
    t.var_dump(&r);
    t.echo("first==first: ");
    let (a, b) = (t.prop(&xml, "first"), t.prop(&xml, "first"));
    let r = eq(&mut t, &a, &b);
    t.var_dump(&r);
    t.echo("first==root: ");
    let a = t.prop(&xml, "first");
    let r = eq(&mut t, &a, &xml);
    t.var_dump(&r);
    let (a, b) = (t.prop(&xml, "first"), t.prop(&xml, "first"));
    t.echo("proxies equal: ");
    let r = eq(&mut t, &a, &b);
    t.var_dump(&r);
    let x = t.load_string("<root><empty/></root>");
    t.echo("missing==missing: ");
    let (a, b) = (t.prop(&x, "missing"), t.prop(&x, "missing"));
    let r = eq(&mut t, &a, &b);
    t.var_dump(&r);
    t.echo("children==children: ");
    let e1 = t.prop(&x, "empty");
    let e2 = t.prop(&x, "empty");
    let (a, b) = (t.children(&e1, None, false), t.children(&e2, None, false));
    let r = eq(&mut t, &a, &b);
    t.var_dump(&r);
    t.echo("missing==empty->missing: ");
    let a = t.prop(&x, "missing");
    let b = t.path(&x, &["empty", "missing"]);
    let r = eq(&mut t, &a, &b);
    t.var_dump(&r);
    t.echo("missing==empty: ");
    let a = t.prop(&x, "missing");
    let b = t.prop(&x, "empty");
    let r = eq(&mut t, &a, &b);
    t.var_dump(&r);
    t.expect("gh12171.phpt");
}

#[test]
fn gh12192_key_during_foreach() {
    let mut t = T::new("gh12192");
    let xml = t.load_string("<root><a>1</a><a>2</a></root>");
    let a = t.prop(&xml, "a");
    let a2 = a.clone();
    t.foreach(&a, |t, _, test| {
        t.echo("Iteration\n");
        let sx = t.sxe(&a2);
        let k = Sxe::key(&sx, &mut t.h);
        match k {
            Ok(k) => t.var_dump(&Value::Str(k.into())),
            Err(e) => t.exception(&e),
        }
        let n = t.get_name(&a2);
        t.var_dump(&s(&n));
        let st = t.str(&test);
        t.var_dump(&s(&st));
    });
    t.var_dump(&a);
    t.expect("gh12192.phpt");
}

#[test]
fn gh12208_current_during_foreach() {
    let mut t = T::new("gh12208");
    let xml = t.load_string("<root><a>1</a><a>2</a></root>");
    let a = t.prop(&xml, "a");
    let a2 = a.clone();
    t.foreach(&a, |t, _, _test| {
        let sx = t.sxe(&a2);
        let c = Sxe::current(&sx).unwrap();
        let st = t.str(&c);
        t.var_dump(&s(&st));
        let st = t.str(&a2);
        t.var_dump(&s(&st));
        let b = t.bool(&a2);
        t.var_dump(&Value::Bool(b));
    });
    t.expect("gh12208.phpt");
}

#[test]
fn gh12231_xpath_non_nodeset() {
    let mut t = T::new("gh12231");
    let sxe = t.load_string("<container><foo/><foo/></container>");
    t.func("SimpleXMLElement::xpath");
    for q in ["count(//foo)", "string(//foo)", "boolean(//foo)"] {
        let v = t.xpath(&sxe, q);
        t.var_dump(&v);
    }
    let v = t.xpath(&sxe, "//foo");
    let n = v.as_array().map_or(0, |a| a.len());
    t.var_dump(&Value::Int(i64::try_from(n).unwrap()));
    t.expect("gh12231.phpt");
}

#[test]
fn gh14638_reconstruct_failure_keeps_state() {
    let mut t = T::new("gh14638");
    let root = t.load_string("<?xml version=\"1.0\" encoding=\"utf-8\" ?>\n<test>\n</test>");
    let sx = t.sxe(&root);
    t.func("SimpleXMLElement::__construct");
    let r = Sxe::construct(&sx, b"malformed", 0, false, b"", false);
    assert!(r.is_err());
    t.func("main");
    let st = t.str(&root);
    t.echo(st);
    t.expect("gh14638.phpt");
}

#[test]
fn gh17040_unset_self_keeps_dom_node() {
    let mut t = T::new("gh17040");
    let dom = php_libxml::tree::Document::new(b"1.0").unwrap();
    let tag = dom.create_element(b"style").unwrap();
    dom.as_node().append_child(&tag).unwrap();
    let html = Sxe::import_dom(&mut t.h, Some(tag.clone()), None).unwrap();
    t.unset_dim(&html, Value::Int(0));
    let text = dom.create_text(b"foo").unwrap();
    tag.append_child(&text).unwrap();
    let d = dom.dump(false, None);
    t.echo(d);
    t.echo("\n");
    let d = tag.dump(false, None);
    t.echo(d);
    t.echo("\n");
    t.var_dump(&html);
    let out = t.output();
    assert!(out.contains("<style>foo</style>"), "{out}");
    assert!(out.ends_with("[0]=>\n  string(3) \"foo\"\n}\n"), "{out}");
}

#[test]
fn gh18304_concat_dynamic_name() {
    let mut t = T::new("gh18304");
    let sxe = t.load_string("<root><abc/></root>");
    let cur = t.prop(&sxe, "abc");
    let v = t.str(&cur) + "hello";
    drop(cur);
    t.set_prop(&sxe, "abc", s(&v));
    let v = t.prop(&sxe, "abc");
    t.var_dump(&v);
    t.expect("gh18304.phpt");
}

#[test]
fn gh18597_invalid_utf8_value() {
    let mut t = T::new("gh18597");
    let sx1 = t.new_sxe("<root />");
    let n = t.prop_w(&sx1, "node");
    t.set_dim(&n, Some(Value::Int(0)), s("node1"));
    let n = t.prop(&sx1, "node");
    let node = t.dim(&n, Value::Int(0));
    t.set_dim(
        &node,
        Some(Value::Int(0)),
        Value::Str(php_runtime::ZStr::new(b"\xc1\xc1c".to_vec())),
    );
    let _ = t.as_xml(&sx1);
    t.echo("Done\n");
    t.expect("gh18597.phpt");
}
