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

#[test]
fn bug42369_string_conversion() {
    let mut t = T::new("bug42369");
    let x = t.load_string("<?xml version=\"1.0\" encoding=\"utf-8\"?><q><x>foo</x></q>");
    t.echo("explicit conversion\n");
    for _ in 0..1000 {
        let v = t.prop(&x, "x");
        assert_eq!(t.str(&v), "foo");
    }
    t.echo("no conversion\n");
    t.expect("bug42369.phpt");
}

#[test]
fn bug43221_attribute_namespaces() {
    let mut t = T::new("bug43221");
    let xml = t.load_string("<?xml version=\"1.0\" encoding=\"utf-8\"?><root />");
    let n = t.add_child(&xml, "node", Some("value"), None);
    t.add_attribute(&n, "a", "b", None);
    t.add_attribute(&n, "c", "d", Some("http://bar.com"));
    t.add_attribute(&n, "foo:e", "f", Some("http://bar.com"));
    let x = t.as_xml(&xml);
    let o = t.str(&x);
    t.echo(o);
    t.expect("bug43221.phpt");
}

#[test]
fn bug44478_entities_in_values() {
    let mut t = T::new("bug44478");
    let e = t.new_sxe("<root></root>");
    for _ in 0..2 {
        t.set_prop(&e, "node1", s("a &#38; b"));
        let v = t.prop(&e, "node1");
        let o = t.str(&v);
        t.echo(o + "\n");
    }
    t.add_child(&e, "node2", Some("a &#38; b"), None);
    let v = t.prop(&e, "node2");
    let o = t.str(&v);
    t.echo(o + "\n");
    t.set_prop(&e, "node2", s("a & b"));
    let v = t.prop(&e, "node2");
    let o = t.str(&v);
    t.echo(o + "\n");
    let x = t.as_xml(&e);
    let o = t.str(&x);
    t.echo(o);
    t.expect("bug44478.phpt");
}

#[test]
fn bug45553_xpath_namespaced_attributes() {
    let mut t = T::new("bug45553");
    let x = t.load_string("<xml xmlns:a=\"http://a\">\n    <data a:label=\"I am A\" label=\"I am Nothing\">test1</data>\n    <a:data a:label=\"I am a:A\" label=\"I am a:Nothing\">test2</a:data>\n</xml>");
    let sx = t.sxe(&x);
    php_simplexml::Sxe::register_xpath_namespace(&sx, b"a", b"http://a").unwrap();
    let first = |t: &mut T, q: &str| {
        let r = t.xpath(&x, q);
        let v = r.as_array().unwrap().values().next().cloned().unwrap();
        v
    };
    let a = first(&mut t, "/xml/data/@a:label");
    let o = t.str(&a);
    t.echo(o + "\n");
    let a = first(&mut t, "/xml/a:data");
    let at = t.attributes(&a, None, false);
    let o = t.str(&at);
    t.echo(o + "\n");
    for q in ["/xml/a:data/@a:label", "/xml/a:data/@label", "/xml/data/@label"] {
        let a = first(&mut t, q);
        let o = t.str(&a);
        t.echo(o + "\n");
    }
    t.expect("bug45553.phpt");
}

#[test]
fn bug46003_isset_nested() {
    let mut t = T::new("bug46003");
    let x = t.load_string("<r>\n  <p>Test</p>\n  <o d='h'>\n    <xx rr='info' />\n    <yy rr='data' />\n  </o>\n</r>");
    let b = t.isset_prop(&x, "p");
    t.var_dump(&Value::Bool(b));
    for (a, b) in [("p", "o"), ("o", "yy"), ("o", "zz"), ("o", "text"), ("o", "xx")] {
        let first = t.prop_mode(&x, a, FetchMode::IsSet);
        let r = matches!(first, Value::Object(_)) && t.isset_prop(&first, b);
        t.var_dump(&Value::Bool(r));
    }
    t.expect("bug46003.phpt");
}

#[test]
fn bug46047_empty_children() {
    let mut t = T::new("bug46047");
    let xml = t
        .new_sxe_ex("SimpleXMLElement", "<foo><bar><![CDATA[]]></bar><baz/></foo>", 16384, false, "", false)
        .unwrap();
    t.print_r(&xml);
    let xml = t.new_sxe("<foo><bar></bar><baz/></foo>");
    t.print_r(&xml);
    let xml = t.new_sxe("<foo><bar/><baz/></foo>");
    t.print_r(&xml);
    t.expect("bug46047.phpt");
}

#[test]
fn bug46048_get_object_vars() {
    let mut t = T::new("bug46048");
    let obj = t.load_string("\n<data id=\"1\">\n    <key>value</key>\n</data>\n");
    let vars = t.array(&obj);
    t.print_r(&vars);
    t.expect("bug46048.phpt");
}

#[test]
fn bug48601_xpath_empty_result() {
    let mut t = T::new("bug48601");
    let sxe = t.load_string("<root><node1>1</node1></root>");
    let nodes = t.xpath(&sxe, "/root/node2/@test");
    match nodes.as_array() {
        Some(a) => t.echo(format!("Result Count: {}\n", a.len())),
        None => t.echo("An error occurred\n"),
    }
    t.expect("bug48601.phpt");
}

#[test]
fn bug52751_processing_instructions() {
    let mut t = T::new("bug52751");
    let sxe = t.load_string("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<foo>\n  <bar>text node</bar>\n  <bar><?baz href=\"ftw\" ?></bar>\n  <bar><?foo bar ?></bar>\n</foo>");
    let v = t.xpath(&sxe, "//bar");
    t.var_dump(&v);
    drop(v);
    let v = t.xpath(&sxe, "//processing-instruction('baz')");
    t.var_dump(&v);
    drop(v);
    let list = t.xpath(&sxe, "//processing-instruction()");
    for pi in list.as_array().unwrap().values().cloned().collect::<Vec<_>>() {
        let n = t.get_name(&pi);
        t.var_dump(&s(&n));
    }
    t.expect("bug52751.phpt");
}

#[test]
fn bug53033_arithmetic() {
    let mut t = T::new("bug53033");
    let x = t.load_string("<x>2.5</x>");
    let v = php_runtime::ops::mul(&x, &Value::Int(1), &mut t.h).unwrap();
    t.var_dump(&v);
    let v = php_runtime::ops::mul(&x, &Value::Float(1.0), &mut t.h).unwrap();
    t.var_dump(&v);
    let v = php_runtime::ops::mul(&s("2.5"), &Value::Int(1), &mut t.h).unwrap();
    t.var_dump(&v);
    let st = t.str(&x);
    let v = php_runtime::ops::mul(&s(&st), &Value::Int(1), &mut t.h).unwrap();
    t.var_dump(&v);
    t.expect("bug53033.phpt");
}

#[test]
fn bug54973_big_numbers() {
    let mut t = T::new("bug54973");
    let xml = t.load_string("<xml><number>9223372036854775808</number></xml>");
    let n = t.prop(&xml, "number");
    t.var_dump(&n);
    let mut v = n.clone();
    for _ in 0..3 {
        v = php_runtime::ops::div(&v, &Value::Int(1024), &mut t.h).unwrap();
    }
    t.var_dump(&v);
    let mut f = Value::Float(n.to_double(&mut t.h).unwrap());
    for _ in 0..3 {
        f = php_runtime::ops::div(&f, &Value::Int(1024), &mut t.h).unwrap();
    }
    t.var_dump(&f);
    t.expect("bug54973.phpt");
}

#[test]
fn bug55098_iteration_with_side_calls() {
    let mut t = T::new("bug55098");
    let xml = t.load_string("<root><a><b>1</b><b>2</b><b>3</b></a></root>");
    let nodes = t.path(&xml, &["a", "b"]);
    type Cb = fn(&mut T, &Value);
    let tests: [(&str, Cb); 7] = [
        ("asXml", |t, n| {
            let _ = t.as_xml(n);
        }),
        ("attributes", |t, n| {
            let _ = t.attributes(n, None, false);
        }),
        ("children", |t, n| {
            let _ = t.children(n, None, false);
        }),
        ("getNamespaces", |t, n| {
            let sx = t.sxe(n);
            let _ = php_simplexml::Sxe::get_namespaces(&sx, false);
        }),
        ("xpath", |t, n| {
            let _ = t.xpath(n, "/root/a/b");
        }),
        ("var_dump", |t, n| t.var_dump(n)),
        ("manipulation combined with querying", |t, n| {
            t.add_attribute(n, "attr", "value", None);
            let a = t.dim(n, s("attr"));
            let _ = t.bool(&a);
            t.add_child(n, "child", Some("value"), None);
            let outer = t.prop_w(n, "outer");
            let o = t.dim_mode(&outer, None, FetchMode::Write);
            t.set_prop(&o, "inner", s("foo"));
            let o = t.prop(n, "outer");
            let _ = t.bool(&o);
            let _ = t.bool(n);
            let _ = t.isset_prop(n, "outer");
            let _ = t.isset_dim(n, s("attr"));
            t.unset_prop(n, "outer");
            t.unset_dim(n, s("attr"));
            t.unset_prop(n, "child");
        }),
    ];
    for (name, cb) in tests {
        t.echo(format!("--- {name} ---\n"));
        let n2 = nodes.clone();
        t.foreach(&nodes, |t, _, data| {
            let line = format!("nodeData: {}\n", t.str(&data));
            t.echo(line);
            cb(t, &n2);
        });
    }
    t.expect("bug55098.phpt");
}

#[test]
fn bug61335_bool_of_element() {
    let mut t = T::new("bug61335");
    let rec1 = t.load_string("<foo><bar>aa</bar>\n</foo>");
    let rec2 = t.load_string("<foo><bar>aa</bar></foo>");
    let b = t.prop(&rec1, "bar");
    let b0 = t.dim(&b, Value::Int(0));
    if t.bool(&b0) {
        t.echo("NONEMPTY1\n");
    }
    if !t.str(&b0).is_empty() {
        t.echo("NONEMPTY2\n");
    }
    let b = t.prop(&rec2, "bar");
    let b0 = t.dim(&b, Value::Int(0));
    if t.bool(&b0) {
        t.echo("NONEMPTY3\n");
    }
    t.expect("bug61335.phpt");
}

#[test]
fn bug62639_children_dump() {
    let mut t = T::new("bug62639");
    let mut keep = Vec::new();
    for xml in [
        "<?xml version=\"1.0\"?>\n<a>\n    <b>\n        <c>\n            <value attr=\"Some Attr\">Some Value</value>\n        </c>\n    </b>\n</a>",
        "<?xml version=\"1.0\"?>\n<a>\n    <b>\n        <c><value attr=\"Some Attr\">Some Value</value></c>\n    </b>\n</a>",
    ] {
        let a = t.new_sxe_ex("A", xml, 0, false, "", false).unwrap();
        let c = t.path(&a, &["b", "c"]);
        let ch = t.children(&c, None, false);
        drop(c);
        let mut last = Value::Null;
        t.foreach(&ch, |t, _, v| {
            t.var_dump(&v);
            last = v;
        });
        keep.push(a);
        keep.push(last);
    }
    t.expect("bug62639.phpt");
}

#[test]
fn bug63575_clone_then_add() {
    let mut t = T::new("bug63575");
    let o1 = t.new_sxe("<a><b></b></a>");
    let o2 = t.clone_obj(&o1);
    let r = t.xpath(&o2, "/a");
    let r0 = r.as_array().unwrap().values().next().cloned().unwrap();
    let c = t.new_sxe("<c></c>");
    let cv = t.str(&c);
    t.add_child(&r0, "c", Some(&cv), None);
    let x1 = t.as_xml(&o1);
    let x2 = t.as_xml(&o2);
    let line = format!("{}\n{}", t.str(&x1), t.str(&x2));
    t.echo(line);
    t.expect("bug63575.phpt");
}

#[test]
fn bug66084_0_dumps() {
    let mut t = T::new("bug66084_0");
    for xml in [
        "<a><b/><c><x/></c></a>",
        "<a><b/><d/><c><x/></c></a>",
        "<a><b/><c><d/><x/></c></a>",
        "<a><b/><c><d><x/></d></c></a>",
    ] {
        let v = t.load_string(xml);
        t.var_dump(&v);
        drop(v);
        t.echo("\n");
    }
    t.expect("bug66084_0.phpt");
}

#[test]
fn bug66084_1_json() {
    let mut t = T::new("bug66084_1");
    let v = t.load_string("<a><b/><c><x/></c></a>");
    let c = t.prop(&v, "c");
    let j = t.json(&c);
    t.echo(j + "\n");
    for xml in [
        "<a><b/><c><x/></c></a>",
        "<a><b/><d/><c><x/></c></a>",
        "<a><b/><c><d/><x/></c></a>",
        "<a><b/><c><d><x/></d></c></a>",
    ] {
        let v = t.load_string(xml);
        let j = t.json(&v);
        t.echo(j + "\n");
    }
    t.expect("bug66084_1.phpt");
}

#[test]
fn bug67116_print_r() {
    let mut t = T::new("bug67116");
    let sxe = t.load_string("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<aa>\n    <bs>\n        <b>b</b>\n    </bs>\n    <cs><c>b</c></cs>\n    <ds><d id=\"d\"></d></ds>\n    <es>\n        <e id=\"e\"></e>\n    </es>\n    <fs><f id=\"f\"></f><f id=\"f\"></f></fs>\n</aa>");
    t.print_r(&sxe);
    t.expect("bug67116.phpt");
}

#[test]
fn bug67572_empty() {
    let mut t = T::new("bug67572");
    t.echo("regular string ... ");
    t.var_dump(&Value::Bool(false));
    let xml = t.load_string("<xml><something>somevalue</something></xml>");
    let xml2 = t.load_string("<xml>\n<something>somevalue</something>\n</xml>");
    for x in [&xml, &xml2] {
        t.foreach(x, |t, k, v| {
            let line = format!("{} = {} ... ", t.str(&k), t.str(&v));
            t.echo(line);
            let b = t.bool(&v);
            t.var_dump(&Value::Bool(!b));
            let eq = php_runtime::ops::is_equal(&v, &Value::Bool(false), &mut t.h).unwrap();
            t.var_dump(&Value::Bool(eq));
        });
    }
    t.expect("bug67572.phpt");
}

#[test]
fn bug69169_print_r() {
    let mut t = T::new("bug69169");
    let a = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<root a=\"b\">\n    <row b=\"y\">\n        <item s=\"t\" />\n    </row>\n    <row p=\"c\">\n        <item y=\"n\" />\n    </row>\n</root>";
    let b: String = a.chars().filter(|c| !matches!(c, '\n' | '\r' | '\t')).collect();
    let x = t.load_string(&b);
    t.print_r(&x);
    t.expect("bug69169.phpt");
}

#[test]
fn bug69491_dump() {
    let mut t = T::new("bug69491");
    let v = t.load_string("<a>\n  <b><c/></b>\n</a>");
    t.var_dump(&v);
    t.expect("bug69491.phpt");
}
