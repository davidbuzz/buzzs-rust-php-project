//! Ports of `ext/simplexml/tests/bug*.phpt`.

mod common;

use common::{fixture, s, T};
use php_runtime::Value;
use php_simplexml::FetchMode;

#[test]
fn bug12170_xpath_comments() {
    let mut t = T::new("bug12170");
    let sxe = t.load_string("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<foo>\n  <bar>text node</bar>\n  <bar><!-- baz --></bar>\n  <bar><!-- foo --></bar>\n</foo>");
    let v = t.xpath(&sxe, "//bar");
    t.var_dump(&v);
    let list = t.xpath(&sxe, "//comment()");
    for c in list.as_array().unwrap().values().cloned().collect::<Vec<_>>() {
        let n = t.get_name(&c);
        t.var_dump(&s(&n));
        let x = t.as_xml(&c);
        t.var_dump(&x);
    }
    t.expect("bug12170.phpt");
}

#[test]
fn bug24392_items() {
    let mut t = T::new("bug24392");
    let x = t.load_file(&fixture("bug24392.xml"));
    let items = t.prop(&x, "item");
    t.foreach(&items, |t, _, item| {
        let title = t.prop(&item, "title");
        let st = t.str(&title);
        t.echo(st + "\n");
    });
    t.expect("bug24392.phpt");
}

#[test]
fn bug26976_indexes() {
    let mut t = T::new("bug26976");
    let root = t.load_string("<?xml version=\"1.0\"?>\n<root>\n <child>a</child>\n <child>b</child>\n <child>c</child>\n <child>d</child>\n</root>\n");
    let c = t.prop(&root, "child");
    for i in 0..4 {
        let v = t.dim(&c, Value::Int(i));
        let st = t.str(&v);
        t.echo(st + "\n");
    }
    t.expect("bug26976.phpt");
}

#[test]
fn bug27010_namespaced_children() {
    let mut t = T::new("bug27010");
    let sxe = t.load_string("<drinks xmlns:hot=\"http://www.example.com/hot\">\n <hot:drink><hot:name>Coffee</hot:name></hot:drink>\n <hot:drink><hot:name>Tea</hot:name></hot:drink>\n <drink><name>Cola</name></drink>\n <drink><name>Juice</name></drink>\n</drinks>");
    let body = |t: &mut T, k: Value, e: Value| {
        let n = t.prop(&e, "name");
        let line = format!("{} is {}\n", t.str(&k), t.str(&n));
        t.echo(line);
    };
    t.foreach(&sxe, body);
    let c = t.children(&sxe, Some("http://www.example.com/hot"), false);
    t.foreach(&c, body);
    t.expect("bug27010.phpt");
}

#[test]
fn bug35785_autovivification() {
    let mut t = T::new("bug35785");
    let xml = t.load_string("<root></root>");
    let bla = t.prop_w(&xml, "bla");
    let posts = t.prop_w(&bla, "posts");
    t.set_prop(&posts, "name", s("FooBar"));
    let x = t.as_xml(&xml);
    let st = t.str(&x);
    t.echo(st);
    let xml = t.load_string("<root></root>");
    let bla = t.prop_mode(&xml, "bla", FetchMode::IsSet);
    let isset = matches!(bla, Value::Object(_)) && t.isset_prop(&bla, "posts");
    t.var_dump(&Value::Bool(isset));
    let bla = t.prop_w(&xml, "bla");
    let posts = t.prop_w(&bla, "posts");
    let p0 = t.dim_mode(&posts, Some(Value::Int(0)), FetchMode::Write);
    t.set_prop(&p0, "name", s("FooBar"));
    let x = t.as_xml(&xml);
    let st = t.str(&x);
    t.echo(st);
    let xml = t.load_string("<root></root>");
    let bla = t.prop_w(&xml, "bla");
    let posts = t.prop_w(&bla, "posts");
    let pn = t.dim_mode(&posts, None, FetchMode::Write);
    t.set_prop(&pn, "name", s("FooBar"));
    let x = t.as_xml(&xml);
    let st = t.str(&x);
    t.echo(st);
    t.expect("bug35785.phpt");
}

#[test]
fn bug36611_value_not_modified() {
    let mut t = T::new("bug36611");
    let xml = t.load_string("<?xml version=\"1.0\" encoding=\"ISO-8859-1\" ?>\n<c_fpobel >\n  <pos >\n    <pos/>\n  </pos>\n</c_fpobel>");
    t.var_dump(&Value::Int(1));
    let pos = t.prop_w(&xml, "pos");
    t.set_dim(&pos, Some(s("act_idx")), Value::Int(1));
    t.var_dump(&Value::Int(1));
    t.expect("bug36611.phpt");
}

#[test]
fn bug37076_concat_property() {
    let mut t = T::new("bug37076");
    let xml = t.load_string("<root><foo /></root>");
    t.set_prop(&xml, "foo", s("foo"));
    let cur = t.prop(&xml, "foo");
    let st = t.str(&cur) + "bar";
    t.set_prop(&xml, "foo", s(&st));
    let x = t.as_xml(&xml);
    let st = t.str(&x);
    t.echo(st);
    t.expect("bug37076.phpt");
}

#[test]
fn bug37076_1_empty_name() {
    let mut t = T::new("bug37076_1");
    let xml = t.load_string("<root><foo /></root>");
    let _ = t.prop_w(&xml, "");
    let x = t.as_xml(&xml);
    let st = t.str(&x);
    t.echo(st);
    t.expect("bug37076_1.phpt");
}

#[test]
fn bug37386_index_assign() {
    let mut t = T::new("bug37386");
    let sx1 = t.new_sxe("<root />");
    let n = t.prop_w(&sx1, "node");
    t.set_dim(&n, Some(Value::Int(0)), s("node1"));
    let n = t.prop_w(&sx1, "node");
    t.set_dim(&n, Some(Value::Int(1)), s("node2"));
    let x = t.as_xml(&sx1);
    let st = t.str(&x);
    t.echo(st + "\n");
    let n = t.prop(&sx1, "node");
    let node = t.dim(&n, Value::Int(0));
    t.set_dim(&node, Some(Value::Int(0)), s("New Value"));
    let x = t.as_xml(&sx1);
    let st = t.str(&x);
    t.echo(st);
    t.expect("bug37386.phpt");
}

#[test]
fn bug38347_unknown_element() {
    let mut t = T::new("bug38347");
    let xml = t.load_string("<xml><item>Item 1</item><item>Item 2</item></xml>");
    let unknown = t.prop(&xml, "unknown");
    t.func("iterate");
    t.print_r(&unknown);
    let items = t.prop(&unknown, "item");
    if items.is_null() {
        t.echo("\nWarning: foreach() argument must be of type array|object, null given in /t/bug38347.php on line 6\n");
    }
    t.echo("Done\n");
    t.expect("bug38347.phpt");
}
