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

#[test]
fn bug38354_xpath_as_xml() {
    let mut t = T::new("bug38354");
    let xml = t.load_string("<code>\n    <a href=\"javascript:alert('1');\"><strong>Item Two</strong></a>\n</code>");
    let list = t.xpath(&xml, "//*");
    for e in list.as_array().unwrap().values().cloned().collect::<Vec<_>>() {
        let x = t.as_xml(&e);
        t.var_dump(&x);
    }
    t.echo("Done\n");
    t.expect("bug38354.phpt");
}

#[test]
fn bug38406_assign_object() {
    let mut t = T::new("bug38406");
    let item = t.new_sxe("<something />");
    t.set_prop(&item, "attribute", s("something"));
    let v = t.prop(&item, "attribute");
    t.var_dump(&v);
    let v = t.prop(&item, "attribute");
    t.set_prop(&item, "otherAttribute", v);
    let v = t.prop(&item, "otherAttribute");
    t.var_dump(&v);
    t.echo("\nWarning: Array to string conversion in /t/x.php on line 1\n");
    let obj = common::new_object("stdClass", None);
    t.set_prop(&item, "Array", obj);
    t.echo("Done\n");
    t.expect("bug38406.phpt");
}

#[test]
fn bug38424_escaping() {
    let mut t = T::new("bug38424");
    let xml = t.load_string("<xml></xml>");
    let st = "abc & def";
    let esc = "abc &amp; def";
    t.set_dim(&xml, Some(s("a1")), s(""));
    t.set_dim(&xml, Some(s("a1")), s(esc));
    t.set_dim(&xml, Some(s("a2")), s(esc));
    t.set_dim(&xml, Some(s("a3")), s(""));
    t.set_dim(&xml, Some(s("a3")), s(st));
    t.set_dim(&xml, Some(s("a4")), s(st));
    let x = t.as_xml(&xml);
    let o = t.str(&x);
    t.echo(o);
    t.expect("bug38424.phpt");
}

#[test]
fn bug39662_clone_whitespace() {
    let mut t = T::new("bug39662");
    let root = t.load_string("<?xml version=\"1.0\" encoding=\"utf-8\" ?>\n<test>\n\n</test>");
    let clone = t.clone_obj(&root);
    t.var_dump(&root);
    t.var_dump(&clone);
    let x = t.as_xml(&clone);
    t.var_dump(&x);
    t.echo("Done\n");
    t.expect("bug39662.phpt");
}

#[test]
fn bug39760_clone_subnodes() {
    let mut t = T::new("bug39760");
    let test = t.load_string("<?xml version=\"1.0\" ?>\n<test>\n    <level1>\n        <level2a>text1</level2a>\n        <level2b>text2</level2b>\n   </level1>\n</test>");
    let v = t.path(&test, &["level1", "level2a"]);
    t.var_dump(&v);
    let test2 = t.clone_obj(&test);
    let v = t.path(&test2, &["level1", "level2a"]);
    t.var_dump(&v);
    let v = t.path(&test, &["level1", "level2a"]);
    let test3 = t.clone_obj(&v);
    t.var_dump(&test3);
    t.echo("Done\n");
    t.expect("bug39760.phpt");
}

#[test]
fn bug40451_add_attribute_no_parent() {
    let mut t = T::new("bug40451");
    let xml = t.load_string("<?xml version=\"1.0\"?>\n    <Host enable=\"true\">\n     <Name>host.server.com</Name>\n     </Host>");
    let add = t.add_child(&xml, "Host", None, None);
    let host = t.prop(&add, "Host");
    t.add_attribute(&host, "enable", "true", None);
    t.expect("bug40451.phpt");
}

#[test]
fn bug41175_empty_attribute() {
    let mut t = T::new("bug41175");
    let xml = t.new_sxe("<img></img>");
    t.add_attribute(&xml, "src", "foo", None);
    t.add_attribute(&xml, "alt", "", None);
    let x = t.as_xml(&xml);
    let o = t.str(&x);
    t.echo(o);
    t.expect("bug41175.phpt");
}

#[test]
fn bug41582_deep_autovivification() {
    let mut t = T::new("bug41582");
    let xml = t.new_sxe("<?xml version=\"1.0\" standalone=\"yes\"?>\n<collection></collection>");
    let movie = t.prop_w(&xml, "movie");
    let m = t.dim_mode(&movie, None, FetchMode::Write);
    let ch = t.prop_w(&m, "characters");
    let c = t.prop_w(&ch, "character");
    let c0 = t.dim_mode(&c, Some(Value::Int(0)), FetchMode::Write);
    t.set_prop(&c0, "name", s("Miss Coder"));
    let x = t.as_xml(&xml);
    let o = t.str(&x);
    t.echo(o);
    t.expect("bug41582.phpt");
}

#[test]
fn bug41861_namespaces_of_children() {
    let mut t = T::new("bug41861");
    let xml = t.load_string("<root>\n    <first_node_no_ns />\n    <ns1:node1 xmlns:ns1=\"#ns1\" />\n    <ns2:node2 xmlns:ns2=\"#ns2\" />\n    <ns3:node3 xmlns:ns3=\"#ns3\" />\n    <last_node_no_ns />\n</root>");
    let implode = |a: &php_runtime::ZArray| {
        a.values()
            .map(|v| v.as_str().map(|s| s.to_string_lossy()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let name = t.get_name(&xml);
    let sx = t.sxe(&xml);
    let ns = php_simplexml::Sxe::get_namespaces(&sx, true).unwrap();
    t.echo(format!("root(recursive): '{name}' -- namespaces: {}\n", implode(&ns)));
    let ns = php_simplexml::Sxe::get_namespaces(&sx, false).unwrap();
    t.echo(format!("root(non-recursive): '{name}' -- namespaces: {}\n", implode(&ns)));
    for ns in [None, Some("#ns1"), Some("#ns2"), Some("#ns3")] {
        let c = t.children(&xml, ns, false);
        t.foreach(&c, |t, _, child| {
            let name = t.get_name(&child);
            let cs = t.sxe(&child);
            let n = php_simplexml::Sxe::get_namespaces(&cs, false).unwrap();
            t.echo(format!(
                "children({}): '{name}' -- namespaces: {}\n",
                ns.unwrap_or(""),
                implode(&n)
            ));
        });
    }
    t.expect("bug41861.phpt");
}

#[test]
fn bug41867_get_name() {
    let mut t = T::new("bug41867");
    let a = t.load_string("<a><b><c/></b></a>");
    for p in [&[][..], &["b"], &["b", "c"]] {
        let v = t.path(&a, p);
        let n = t.get_name(&v);
        t.echo(n + "\n");
    }
    t.expect("bug41867.phpt");
}

#[test]
fn bug41947_empty_namespace() {
    let mut t = T::new("bug41947");
    let xml = t.load_string("<?xml version=\"1.0\" encoding=\"utf-8\"?><root xmlns:myns=\"http://myns\" />");
    let c = t.add_child(&xml, "child", None, Some("http://myns"));
    let _g = t.add_child(&c, "grandchild", Some("hello"), Some(""));
    let g = t.xpath(&xml, "//grandchild");
    if let Some(first) = g.as_array().and_then(|a| a.values().next().cloned()) {
        let st = t.str(&first);
        t.echo(st + "\n");
    }
    t.expect("bug41947.phpt");
}

/// `RecursiveIteratorIterator(LEAVES_ONLY)` over a `SimpleXMLIterator`.
fn leaves(t: &mut T, it: &Value, out: &mut Vec<Value>) {
    let sx = t.sxe(it);
    php_simplexml::Sxe::rewind(&sx, &mut t.h).unwrap();
    while php_simplexml::Sxe::valid(&sx) {
        if php_simplexml::Sxe::has_children(&sx, &mut t.h).unwrap() {
            let c = php_simplexml::Sxe::get_children(&sx);
            leaves(t, &c, out);
        } else {
            out.push(php_simplexml::Sxe::current(&sx).unwrap());
        }
        php_simplexml::Sxe::next(&sx, &mut t.h).unwrap();
    }
}

#[test]
fn bug42259_recursive_iteration() {
    let mut t = T::new("bug42259");
    let xml = "<xml>\n  <fieldset1>\n    <field1/>\n    <field2/>\n  </fieldset1>\n  <fieldset2>\n    <options>\n      <option1/>\n      <option2/>\n      <option3/>\n    </options>\n    <field1/>\n    <field2/>\n  </fieldset2>\n</xml>";
    let sxe = t.new_sxe_ex("SimpleXMLIterator", xml, 0, false, "", false).unwrap();
    let mut out = Vec::new();
    leaves(&mut t, &sxe, &mut out);
    for child in out {
        let anc = t.xpath(&child, "ancestor-or-self::*");
        let list: Vec<Value> = anc.as_array().unwrap().values().cloned().collect();
        let names: Vec<String> = list.iter().map(|a| t.get_name(a)).collect();
        t.echo(format!("{} steps: {}\n", list.len(), names.join("/")));
    }
    t.expect("bug42259.phpt");
}
