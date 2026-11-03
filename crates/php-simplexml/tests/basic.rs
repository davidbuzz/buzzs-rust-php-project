//! Ports of the numbered `ext/simplexml/tests/0xx.phpt` tests.

mod common;

use common::{fixture, s, trim, T};
use php_runtime::Value;

const SXE_XML_002: &str = r#"<?xml version='1.0'?>
<!DOCTYPE sxe SYSTEM "notfound.dtd">
<sxe id="elem1">
 <elem1 attr1='first'>
  <!-- comment -->
  <elem2>
   <elem3>
    <elem4>
     <?test processing instruction ?>
    </elem4>
   </elem3>
  </elem2>
 </elem1>
</sxe>"#;

#[test]
fn t001_load_file() {
    let mut t = T::new("001");
    let v = t.load_file(&fixture("sxe.xml"));
    t.var_dump(&v);
    t.expect("001.phpt");
}

#[test]
fn t002_clone() {
    let mut t = T::new("002");
    let sxe = t.load_string(SXE_XML_002);
    let copy = t.clone_obj(&sxe);
    t.var_dump(&copy);
    t.expect("002.phpt");
}

#[test]
fn t003_entities() {
    let mut t = T::new("003");
    let xml = r#"<?xml version='1.0'?>
<!DOCTYPE sxe SYSTEM "notfound.dtd" [
<!ENTITY included-entity "This is text included from an entity">
]>
<sxe id="elem1">
 Plain text.
 <elem1 attr1='first'>
  <!-- comment -->
  <elem2>
   <elem3>
    &included-entity;
    <elem4>
     <?test processing instruction ?>
    </elem4>
   </elem3>
  </elem2>
 </elem1>
</sxe>"#;
    let v = t.load_string(xml);
    t.var_dump(&v);
    t.expect("003.phpt");
}

#[test]
fn t004_cdata() {
    let mut t = T::new("004");
    let xml = r#"<?xml version='1.0'?>
<!DOCTYPE sxe SYSTEM "notfound.dtd">
<sxe id="elem1">
 Plain text.
 <elem1 attr1='first'>
  <!-- comment -->
  <elem2>
   <![CDATA[CDATA block]]>
   <elem3>
    <elem4>
     <?test processing instruction ?>
    </elem4>
   </elem3>
  </elem2>
 </elem1>
</sxe>"#;
    let sxe = t.load_string(xml);
    t.var_dump(&sxe);
    let elem2 = t.path(&sxe, &["elem1", "elem2"]);
    let st = trim(&t.str(&elem2));
    t.var_dump(&s(&st));
    t.expect("004.phpt");
}

const XML_005: &str = r#"<?xml version='1.0'?>
<!DOCTYPE sxe SYSTEM "notfound.dtd">
<sxe id="elem1">
 Plain text.
 <elem1 attr1='first'>
  <!-- comment -->
  <elem2>
   Here we have some text data.
   <elem3>
    And here some more.
    <elem4>
     Wow once again.
    </elem4>
   </elem3>
  </elem2>
 </elem1>
</sxe>"#;

#[test]
fn t005_text() {
    let mut t = T::new("005");
    let sxe = t.load_string(XML_005);
    for path in [&["elem1", "elem2"][..], &["elem1", "elem2", "elem3"], &["elem1", "elem2", "elem3", "elem4"]] {
        let v = t.path(&sxe, path);
        let st = trim(&t.str(&v));
        t.var_dump(&s(&st));
    }
    t.echo("---Done---\n");
    t.expect("005.phpt");
}

#[test]
fn t006_foreach() {
    let mut t = T::new("006");
    let xml = r#"<?xml version='1.0'?>
<!DOCTYPE sxe SYSTEM "notfound.dtd">
<sxe id="elem1">
 Plain text.
 <elem1 attr1='first'>
  Bla bla 1.
  <!-- comment -->
  <elem2>
   Here we have some text data.
   <elem3>
    And here some more.
    <elem4>
     Wow once again.
    </elem4>
   </elem3>
  </elem2>
 </elem1>
 <elem11 attr2='second'>
  Bla bla 2.
  <elem111>
   Foo Bar
  </elem111>
 </elem11>
</sxe>"#;
    let sxe = t.load_string(xml);
    let body = |t: &mut T, k: Value, v: Value| {
        t.var_dump(&k);
        let st = trim(&t.str(&v));
        t.var_dump(&s(&st));
    };
    t.foreach(&sxe, body);
    t.echo("===CLONE===\n");
    let c = t.clone_obj(&sxe);
    t.foreach(&c, body);
    t.echo("===ELEMENT===\n");
    let e = t.prop(&sxe, "elem11");
    t.foreach(&e, body);
    t.echo("===COMMENT===\n");
    let e = t.prop(&sxe, "elem1");
    t.foreach(&e, body);
    t.expect("006.phpt");
}

#[test]
fn t007_attributes_rw() {
    let mut t = T::new("007");
    let sxe = t.load_string(SXE_XML_002);
    t.echo("===Property===\n");
    let e = t.prop(&sxe, "elem1");
    t.var_dump(&e);
    t.echo("===Array===\n");
    let v = t.dim(&sxe, s("id"));
    t.var_dump(&v);
    let e = t.prop(&sxe, "elem1");
    let v = t.dim(&e, s("attr1"));
    t.var_dump(&v);
    t.echo("===Set===\n");
    t.set_dim(&sxe, Some(s("id")), s("Changed1"));
    let v = t.dim(&sxe, s("id"));
    t.var_dump(&v);
    let e = t.prop(&sxe, "elem1");
    t.set_dim(&e, Some(s("attr1")), Value::Int(12));
    let v = t.dim(&e, s("attr1"));
    t.var_dump(&v);
    t.echo("===Unset===\n");
    t.unset_dim(&sxe, s("id"));
    let v = t.dim(&sxe, s("id"));
    t.var_dump(&v);
    t.unset_dim(&e, s("attr1"));
    let v = t.dim(&e, s("attr1"));
    t.var_dump(&v);
    t.echo("===Misc.===\n");
    t.var_dump(&Value::Int(4));
    let _dummy = t.dim(&e, Value::Int(4));
    t.var_dump(&Value::Int(4));
    t.expect("007.phpt");
}

#[test]
#[should_panic(expected = "output mismatch")]
fn harness_detects_mismatch() {
    let mut t = T::new("001");
    t.echo("nope");
    t.expect("001.phpt");
}

#[test]
fn harness_fmatch() {
    assert!(common::fmatch(b"a%sb", b"axxb"));
    assert!(!common::fmatch(b"a%sb", b"ab"));
    assert!(common::fmatch(b"#%d (%d)", b"#12 (3)"));
    assert!(!common::fmatch(b"x", b"y"));
}

#[test]
fn t008_xpath() {
    let mut t = T::new("008");
    let sxe = t.load_string(SXE_XML_002);
    t.func("SimpleXMLElement::xpath");
    for q in ["elem1/elem2/elem3/elem4", "***", "**"] {
        let v = t.xpath(&sxe, q);
        t.var_dump(&v);
    }
    t.expect("008.phpt");
}

const XML_009: &str = r#"<?xml version='1.0'?>
<!DOCTYPE sxe SYSTEM "notfound.dtd">
<sxe id="elem1">
 Plain text.
 <elem1 attr1='first'>
  Bla bla 1.
  <!-- comment -->
  <elem2>
   Here we have some text data.
   <elem3>
    And here some more.
    <elem4>
     Wow once again.
    </elem4>
   </elem3>
  </elem2>
 </elem1>
 <elem11 attr2='second'>
  Bla bla 2.
 </elem11>
</sxe>"#;

#[test]
fn t009_children_foreach() {
    let mut t = T::new("009");
    let sxe = t.load_string(XML_009);
    let c = t.children(&sxe, None, false);
    t.foreach(&c, |t, k, v| {
        t.var_dump(&k);
        let class = match &v {
            Value::Object(o) => o.class_name().to_string_lossy(),
            _ => String::new(),
        };
        t.var_dump(&s(&class));
        let st = trim(&t.str(&v));
        t.var_dump(&s(&st));
    });
    t.expect("009.phpt");
}

#[test]
fn t009b_children_dump() {
    let mut t = T::new("009b");
    let sxe = t.load_string(r#"<?xml version='1.0'?>
<!DOCTYPE sxe SYSTEM "notfound.dtd">
<sxe id="elem1">
 Plain text.
 <elem1 attr1='first'>Bla bla 1.<!-- comment --><elem2>
   Here we have some text data.
  </elem2></elem1>
 <elem11 attr2='second'>Bla bla 2.</elem11>
</sxe>"#);
    let c = t.children(&sxe, None, false);
    t.var_dump(&c);
    t.expect("009b.phpt");
}

#[test]
fn t010_inherited_class() {
    let mut t = T::new("010");
    let v = t.load_string_ex(SXE_XML_002, Some("simplexml_inherited"), 0, "", false);
    t.var_dump(&v);
    t.expect("010.phpt");
}

#[test]
fn t011_echo() {
    let mut t = T::new("011");
    let sxe = t.load_string("<?xml version=\"1.0\" encoding=\"ISO-8859-1\" ?>\n<foo>\n  <bar>bar</bar>\n  <baz>baz1</baz>\n  <baz>baz2</baz>\n</foo>");
    t.echo("===BAR===\n");
    let v = t.prop(&sxe, "bar");
    let st = t.str(&v);
    t.echo(st + "\n");
    t.echo("===BAZ===\n");
    let v = t.prop(&sxe, "baz");
    let st = t.str(&v);
    t.echo(st + "\n");
    t.echo("===BAZ0===\n");
    let b = t.prop(&sxe, "baz");
    let v = t.dim(&b, Value::Int(0));
    let st = t.str(&v);
    t.echo(st + "\n");
    t.echo("===BAZ1===\n");
    let v = t.dim(&b, Value::Int(1));
    let st = t.str(&v);
    t.echo(st + "\n");
    t.expect("011.phpt");
}

#[test]
fn t012_attribute_write() {
    let mut t = T::new("012");
    let sxe = t.load_string("<?xml version=\"1.0\" encoding=\"ISO-8859-1\" ?>\n<foo/>");
    t.set_dim(&sxe, Some(s("")), s("value"));
    t.set_dim(&sxe, Some(s("attr")), s("value"));
    let x = t.as_xml(&sxe);
    let st = t.str(&x);
    t.echo(st);
    t.set_dim(&sxe, Some(s("attr")), s("new value"));
    let x = t.as_xml(&sxe);
    let st = t.str(&x);
    t.echo(st);
    t.set_dim(&sxe, None, s("error"));
    t.expect("012.phpt");
}

#[test]
fn t013_string_cast_mixed() {
    let mut t = T::new("013");
    let sxe = t.load_string("<?xml version=\"1.0\" encoding=\"ISO-8859-1\" ?>\n<foo>bar<baz/>bar</foo>");
    let st = t.str(&sxe);
    t.var_dump(&s(&st));
    t.expect("013.phpt");
}

#[test]
fn t014_attribute_ops() {
    let mut t = T::new("014");
    let people = t.load_string("<people>\n   <person name=\"Joe\"></person>\n</people>");
    let p = t.prop(&people, "person");
    let v = t.dim(&p, s("name"));
    t.var_dump(&v);
    let v = t.dim(&p, s("age"));
    t.var_dump(&v);
    let person = t.prop(&people, "person");
    t.set_dim(&person, Some(s("name")), s("XXX"));
    let p = t.prop(&people, "person");
    let v = t.dim(&p, s("name"));
    t.var_dump(&v);
    t.set_dim(&p, Some(s("age")), Value::Int(30));
    let v = t.dim(&p, s("age"));
    t.var_dump(&v);
    t.echo("---Unset:---\n");
    t.unset_dim(&p, s("age"));
    t.echo("---Unset?---\n");
    let v = t.dim(&p, s("age"));
    t.var_dump(&v);
    let b = t.isset_dim(&p, s("age"));
    t.var_dump(&Value::Bool(b));
    t.set_dim(&p, Some(s("age")), Value::Int(30));
    t.echo("---Unsupported---\n");
    let v = t.dim(&p, s("age"));
    t.var_dump(&v);
    // $people->person['age'] += 5;
    let cur = t.dim(&p, s("age"));
    let n = php_runtime::ops::add(&cur, &Value::Int(5), &mut t.h).unwrap();
    t.set_dim(&p, Some(s("age")), n);
    let v = t.dim(&p, s("age"));
    t.var_dump(&v);
    t.expect("014.phpt");
}

const PEOPLE1: &str = "<people>\n   <person name=\"Joe\"></person>\n</people>";
const PEOPLE2: &str = "<people>\n   <person name=\"Joe\"></person>\n   <person name=\"Boe\"></person>\n</people>";

fn add5(t: &mut T, v: Value) -> Value {
    php_runtime::ops::add(&v, &Value::Int(5), &mut t.h).unwrap()
}

#[test]
fn t014a_indexed_attributes() {
    let mut t = T::new("014a");
    let people = t.load_string(PEOPLE1);
    let p0 = |t: &mut T| {
        let p = t.prop(&people, "person");
        t.dim(&p, Value::Int(0))
    };
    let p = p0(&mut t);
    let v = t.dim(&p, s("name"));
    t.var_dump(&v);
    let v = t.dim(&p, s("age"));
    t.var_dump(&v);
    t.set_dim(&p, Some(s("name")), s("XXX"));
    let p = p0(&mut t);
    let v = t.dim(&p, s("name"));
    t.var_dump(&v);
    t.set_dim(&p, Some(s("age")), Value::Int(30));
    let v = t.dim(&p, s("age"));
    t.var_dump(&v);
    t.echo("---Unset:---\n");
    t.unset_dim(&p, s("age"));
    t.echo("---Unset?---\n");
    let v = t.dim(&p, s("age"));
    t.var_dump(&v);
    let b = t.isset_dim(&p, s("age"));
    t.var_dump(&Value::Bool(b));
    t.echo("---Unsupported---\n");
    let v = t.dim(&p, s("age"));
    t.var_dump(&v);
    let person = t.prop(&people, "person");
    t.dim_op(&person, s("age"), add5);
    let p = p0(&mut t);
    let v = t.dim(&p, s("age"));
    t.var_dump(&v);
    t.expect("014a.phpt");
}

#[test]
fn t014b_second_element_attributes() {
    let mut t = T::new("014b");
    let people = t.load_string(PEOPLE2);
    let pn = |t: &mut T, i: i64| {
        let p = t.prop(&people, "person");
        t.dim(&p, Value::Int(i))
    };
    let p = pn(&mut t, 0);
    let v = t.dim(&p, s("name"));
    t.var_dump(&v);
    let p = pn(&mut t, 1);
    let v = t.dim(&p, s("age"));
    t.var_dump(&v);
    t.set_dim(&p, Some(s("name")), s("XXX"));
    let p = pn(&mut t, 1);
    let v = t.dim(&p, s("name"));
    t.var_dump(&v);
    t.set_dim(&p, Some(s("age")), Value::Int(30));
    let v = t.dim(&p, s("age"));
    t.var_dump(&v);
    t.echo("---Unset:---\n");
    t.unset_dim(&p, s("age"));
    t.echo("---Unset?---\n");
    let v = t.dim(&p, s("age"));
    t.var_dump(&v);
    let b = t.isset_dim(&p, s("age"));
    t.var_dump(&Value::Bool(b));
    t.echo("---Unsupported---\n");
    t.dim_op(&p, s("age"), add5);
    let p = pn(&mut t, 1);
    let v = t.dim(&p, s("age"));
    t.var_dump(&v);
    t.expect("014b.phpt");
}

#[test]
fn t015_attribute_on_list() {
    let mut t = T::new("015");
    let people = t.load_string(PEOPLE1);
    let p = t.prop(&people, "person");
    let v = t.dim(&p, s("name"));
    t.var_dump(&v);
    let p0 = t.dim(&p, Value::Int(0));
    let v = t.dim(&p0, s("name"));
    t.var_dump(&v);
    let people = t.load_string(PEOPLE2);
    let p = t.prop(&people, "person");
    for i in 0..2 {
        let pi = t.dim(&p, Value::Int(i));
        let v = t.dim(&pi, s("name"));
        t.var_dump(&v);
    }
    t.expect("015.phpt");
}

fn concat(t: &mut T, v: Value, suffix: &str) -> Value {
    let st = t.str(&v);
    s(&(st + suffix))
}

#[test]
fn t016_concat_assign() {
    let mut t = T::new("016");
    for indexed in [false, true] {
        if indexed {
            t.echo("---[0]---\n");
        }
        let people = t.load_string(PEOPLE1);
        let get = |t: &mut T| {
            let p = t.prop(&people, "person");
            if indexed {
                t.dim(&p, Value::Int(0))
            } else {
                p
            }
        };
        let p = get(&mut t);
        let v = t.dim(&p, s("name"));
        t.var_dump(&v);
        let cur = t.dim(&p, s("name"));
        let nv = concat(&mut t, cur, "Foo");
        t.set_dim(&p, Some(s("name")), nv);
        let p = get(&mut t);
        let v = t.dim(&p, s("name"));
        t.var_dump(&v);
        t.dim_op(&p, s("name"), |t, v| concat(t, v, "Bar"));
        let p = get(&mut t);
        let v = t.dim(&p, s("name"));
        t.var_dump(&v);
    }
    t.expect("016.phpt");
}

#[test]
fn t016a_concat_assign() {
    let mut t = T::new("016a");
    let people = t.load_string("<people>\n   <person name=\"Foo\"></person>\n</people>");
    let p = t.prop(&people, "person");
    let v = t.dim(&p, s("name"));
    t.var_dump(&v);
    t.dim_op(&p, s("name"), |t, v| concat(t, v, "Bar"));
    let v = t.dim(&p, s("name"));
    t.var_dump(&v);
    t.expect("016a.phpt");
}

const XML_017: &str = "<people>\n   <person name=\"Joe\">\n     <child name=\"Ann\" />\n     <child name=\"Marray\" />\n   </person>\n   <person name=\"Boe\">\n     <child name=\"Joe\" />\n     <child name=\"Ann\" />\n   </person>\n</people>";
const XML1_017: &str = "<people>\n   <person name=\"Joe\">\n     <child name=\"Ann\" />\n   </person>\n</people>";

fn print_xml(t: &mut T, xml: &Value) {
    let c = t.children(xml, None, false);
    t.foreach(&c, |t, _, person| {
        let n = t.dim(&person, s("name"));
        let line = format!("person: {}\n", t.str(&n));
        t.echo(line);
        let cc = t.children(&person, None, false);
        t.foreach(&cc, |t, _, child| {
            let n = t.dim(&child, s("name"));
            let line = format!("  child: {}\n", t.str(&n));
            t.echo(line);
        });
    });
}

fn print_xml2(t: &mut T, xml: &Value) {
    let mut i = 0;
    loop {
        let persons = t.prop(xml, "person");
        if i >= t.count(&persons) {
            break;
        }
        let person = t.dim(&persons, Value::Int(i));
        let n = t.dim(&person, s("name"));
        let line = format!("person: {}\n", t.str(&n));
        t.echo(line);
        let mut j = 0;
        loop {
            let children = t.prop(&person, "child");
            if j >= t.count(&children) {
                break;
            }
            let c = t.dim(&children, Value::Int(j));
            let n = t.dim(&c, s("name"));
            let line = format!("  child: {}\n", t.str(&n));
            t.echo(line);
            j += 1;
        }
        i += 1;
    }
}

#[test]
fn t017_nested_iteration() {
    let mut t = T::new("017");
    t.echo("---11---\n");
    let x = t.load_string(XML_017);
    print_xml(&mut t, &x);
    t.echo("---12---\n");
    let x = t.load_string(XML1_017);
    print_xml(&mut t, &x);
    t.echo("---21---\n");
    let x = t.load_string(XML_017);
    print_xml2(&mut t, &x);
    t.echo("---22---\n");
    let x = t.load_string(XML1_017);
    print_xml2(&mut t, &x);
    t.expect("017.phpt");
}

fn traverse_xml(t: &mut T, pad: &str, xml: &Value) {
    let c = t.children(xml, None, false);
    t.foreach(&c, |t, name, node| {
        let line = format!("{pad}<{}", t.str(&name));
        t.echo(line);
        let attrs = t.attributes(&node, None, false);
        t.foreach(&attrs, |t, attr, value| {
            let line = format!(" {}=\"{}\"", t.str(&attr), t.str(&value));
            t.echo(line);
        });
        t.echo(">\n");
        traverse_xml(t, &format!("{pad}  "), &node);
        let line = format!("{pad}</{}>\n", t.str(&name));
        t.echo(line);
    });
}

#[test]
fn t018_traverse() {
    let mut t = T::new("018");
    let x = t.load_string("<people>\n   <person name=\"Joe\">\n     Text1\n     <child name=\"Ann\" />\n     Text2\n     <child name=\"Marray\" />\n     Text3\n   </person>\n   <person name=\"Boe\">\n     <child name=\"Joe\" />\n     <child name=\"Ann\" />\n   </person>\n</people>");
    traverse_xml(&mut t, "", &x);
    t.echo("----------\n");
    let x = t.load_string("<people>\n   <person name=\"Joe\">\n     <child />\n   </person>\n</people>");
    traverse_xml(&mut t, "", &x);
    t.echo("---Done---\n");
    t.expect("018.phpt");
}

#[test]
fn t019_children_foreach_clone() {
    let mut t = T::new("019");
    let xml = r#"<?xml version='1.0'?>
<!DOCTYPE sxe SYSTEM "notfound.dtd">
<sxe id="elem1">
 Plain text.
 <elem1 attr1='first'>
  Bla bla 1.
  <!-- comment -->
  <elem2>
   Here we have some text data.
   <elem3>
    And here some more.
    <elem4>
     Wow once again.
    </elem4>
   </elem3>
  </elem2>
 </elem1>
 <elem11 attr2='second'>
  Bla bla 2.
  <elem111>
   Foo Bar
  </elem111>
 </elem11>
</sxe>"#;
    let sxe = t.load_string(xml);
    let body = |t: &mut T, k: Value, v: Value| {
        t.var_dump(&k);
        let st = trim(&t.str(&v));
        t.var_dump(&s(&st));
    };
    let c = t.children(&sxe, None, false);
    t.foreach(&c, body);
    t.echo("===CLONE===\n");
    let c = t.children(&sxe, None, false);
    let cc = t.clone_obj(&c);
    t.foreach(&cc, body);
    t.echo("===ELEMENT===\n");
    let e = t.prop(&sxe, "elem11");
    let c = t.children(&e, None, false);
    t.foreach(&c, body);
    t.echo("===COMMENT===\n");
    let e = t.prop(&sxe, "elem1");
    let c = t.children(&e, None, false);
    t.foreach(&c, body);
    t.expect("019.phpt");
}

#[test]
fn t020_attribute_compare() {
    let mut t = T::new("020");
    let doc = t.load_string("<root><name attr=\"foo\">bar</name></root>");
    let n = t.prop(&doc, "name");
    let a = t.dim(&n, s("attr"));
    let st = t.str(&a);
    t.echo(st + "\n");
    let eq = php_runtime::ops::is_equal(&a, &s("foo"), &mut t.h).unwrap();
    t.echo(if eq { "Works\n" } else { "Error\n" });
    t.expect("020.phpt");
}

#[test]
fn t021_isset() {
    let mut t = T::new("021");
    let doc = t.load_string("<root><exists>foo</exists></root>");
    let ok = t.isset_prop(&doc, "exists") && !t.isset_prop(&doc, "doesnotexist");
    t.echo(if ok { "Works\n" } else { "Error\n" });
    t.expect("021.phpt");
}

#[test]
fn t022_empty_element() {
    let mut t = T::new("022");
    let sxe = t.load_string("<?xml version='1.0'?>\n<pres><content><file glob=\"slide_*.xml\"/></content></pres>");
    t.echo("===CONTENT===\n");
    let c = t.prop(&sxe, "content");
    t.var_dump(&c);
    t.echo("===FILE===\n");
    let f = t.prop(&c, "file");
    t.var_dump(&f);
    t.echo("===FOREACH===\n");
    t.foreach(&f, |t, _, file| {
        t.var_dump(&file);
        let g = t.dim(&file, s("glob"));
        t.var_dump(&g);
    });
    t.expect("022.phpt");
}

#[test]
fn t024_xpath_attributes() {
    let mut t = T::new("024");
    let sxe = t.load_string("<?xml version='1.0'?>\n<root>\n<elem attr1='11' attr2='12' attr3='13'/>\n<elem attr1='21' attr2='22' attr3='23'/>\n<elem attr1='31' attr2='32' attr3='33'/>\n</root>");
    for q in ["elem/@attr2", "//@attr2", "//@*", "elem[2]/@attr2"] {
        t.echo(format!("==={q}===\n"));
        let v = t.xpath(&sxe, q);
        t.var_dump(&v);
    }
    t.expect("024.phpt");
}

fn ns_dump(t: &mut T, sxe: &Value, calls: &[(bool, bool)]) {
    let s_ = t.sxe(sxe);
    for &(doc, rec) in calls {
        let v = if doc {
            php_simplexml::Sxe::get_doc_namespaces(&s_, rec, true).unwrap()
        } else {
            Value::Array(php_simplexml::Sxe::get_namespaces(&s_, rec).unwrap())
        };
        t.var_dump(&v);
    }
}

#[test]
fn t025_namespaces() {
    let mut t = T::new("025");
    let sxe = t.load_string(r#"<?xml version='1.0'?>
<xhtml:html xmlns:html='http://www.w3.org/1999/xhtml' xmlns:xhtml='http://www.w3.org/TR/REC-html40'>
<xhtml:head><xhtml:title xmlns:xhtml='http://www.w3.org/TR/REC-html401'>bla</xhtml:title></xhtml:head>
<xhtml:body html:title="b">
<html:h1>bla</html:h1>
<foo:bar xmlns:foo='foobar' xmlns:baz='foobarbaz'/>
</xhtml:body>
</xhtml:html>"#);
    ns_dump(&mut t, &sxe, &[(false, false), (false, true), (true, false), (true, true)]);
    let sxe = t.load_string("<?xml version='1.0'?>\n<html xmlns='http://www.w3.org/1999/xhtml'>\n<head><title xmlns='http://www.w3.org/TR/REC-html40'>bla</title></head>\n</html>");
    ns_dump(&mut t, &sxe, &[(false, false), (true, false)]);
    let sxe = t.load_string("<?xml version='1.0'?>\n<root/>");
    ns_dump(&mut t, &sxe, &[(false, false), (true, false)]);
    t.expect("025.phpt");
}

fn traverse2(t: &mut T, xml: &Value, pad: &str) {
    let name = t.get_name(xml);
    t.echo(format!("{pad}<{name}"));
    let attrs = t.attributes(xml, None, false);
    t.foreach(&attrs, |t, attr, value| {
        let line = format!(" {}=\"{}\"", t.str(&attr), t.str(&value));
        t.echo(line);
    });
    let body = trim(&t.str(xml));
    t.echo(format!(">{body}\n"));
    let c = t.children(xml, None, false);
    t.foreach(&c, |t, _, node| traverse2(t, &node, &format!("{pad}  ")));
    t.echo(format!("{pad}</{name}>\n"));
}

#[test]
fn t026_traverse() {
    let mut t = T::new("026");
    let p = t.load_string("<people>\n  <person>Jane</person>\n</people>");
    traverse2(&mut t, &p, "");
    t.expect("026.phpt");
}

#[test]
fn t027_create_elements() {
    let mut t = T::new("027");
    let people = t.load_string("<people></people>");
    traverse2(&mut t, &people, "");
    t.set_prop(&people, "person", s("Joe"));
    let p = t.prop_w(&people, "person");
    t.set_dim(&p, Some(s("gender")), s("male"));
    traverse2(&mut t, &people, "");
    t.set_prop(&people, "person", s("Jane"));
    traverse2(&mut t, &people, "");
    let p = t.prop_w(&people, "person");
    t.set_dim(&p, Some(s("gender")), s("female"));
    let p = t.prop_w(&people, "person");
    t.set_dim(&p, Some(Value::Int(1)), s("Joe"));
    let p = t.prop_w(&people, "person");
    let p1 = t.dim_mode(&p, Some(Value::Int(1)), php_simplexml::FetchMode::Write);
    t.set_dim(&p1, Some(s("gender")), s("male"));
    traverse2(&mut t, &people, "");
    let p = t.prop_w(&people, "person");
    t.set_dim(&p, Some(Value::Int(3)), s("Minni-me"));
    let p = t.prop_w(&people, "person");
    let p2 = t.dim_mode(&p, Some(Value::Int(2)), php_simplexml::FetchMode::Write);
    t.set_dim(&p2, Some(s("gender")), s("male"));
    traverse2(&mut t, &people, "");
    let p = t.prop_w(&people, "person");
    let p3 = t.dim_mode(&p, Some(Value::Int(3)), php_simplexml::FetchMode::Write);
    if matches!(p3, Value::Object(_)) {
        t.set_dim(&p3, Some(s("gender")), s("error"));
    }
    traverse2(&mut t, &people, "");
    t.expect("027.phpt");
}

#[test]
fn t028_create_with_attribute() {
    let mut t = T::new("028");
    let people = t.load_string("<people></people>");
    traverse2(&mut t, &people, "");
    let p = t.prop_w(&people, "person");
    t.set_dim(&p, Some(s("name")), s("John"));
    traverse2(&mut t, &people, "");
    t.expect("028.phpt");
}

const PEOPLE_029: &str = "<people>\n  <person name=\"Joe\"/>\n  <person name=\"John\">\n    <children>\n      <person name=\"Joe\"/>\n    </children>\n  </person>\n  <person name=\"Jane\"/>\n</people>";

#[test]
fn t029_count_in_foreach() {
    let mut t = T::new("029");
    let people = t.load_string(PEOPLE_029);
    let p2 = people.clone();
    t.foreach(&people, |t, _, person| {
        let n = t.dim(&person, s("name"));
        let st = t.str(&n);
        t.var_dump(&s(&st));
        let c = t.count(&p2);
        t.var_dump(&Value::Int(c));
        let c = t.count(&person);
        t.var_dump(&Value::Int(c));
    });
    t.expect("029.phpt");
}

const XML_030: &str = "<root s:att1=\"b\" att1=\"a\"\n      xmlns:s=\"urn::test\" xmlns:t=\"urn::test-t\">\n   <child1>test</child1>\n   <child1>test 2</child1>\n   <s:child3 />\n</root>";

#[test]
fn t030_isset_unset_offsets() {
    let mut t = T::new("030");
    let sxe = t.load_string(XML_030);
    let c = t.prop(&sxe, "child1");
    let v = t.dim(&c, Value::Int(0));
    let st = t.str(&v);
    t.echo(st + "\n");
    let v = t.dim(&c, Value::Int(1));
    let st = t.str(&v);
    t.echo(st + "\n\n");
    let b = t.isset_dim(&c, Value::Int(1));
    t.var_dump(&Value::Bool(b));
    t.unset_dim(&c, Value::Int(1));
    let b = t.isset_dim(&c, Value::Int(1));
    t.var_dump(&Value::Bool(b));
    t.echo("\n");
    let atts = t.attributes(&sxe, Some("urn::test"), false);
    let b = t.isset_dim(&atts, Value::Int(0));
    t.var_dump(&Value::Bool(b));
    t.unset_dim(&atts, Value::Int(0));
    let b = t.isset_dim(&atts, Value::Int(0));
    t.var_dump(&Value::Bool(b));
    let b = t.isset_dim(&atts, Value::Bool(true));
    t.var_dump(&Value::Bool(b));
    t.expect("030.phpt");
}

#[test]
fn t031_add_child_attribute() {
    let mut t = T::new("031");
    let sxe = t.load_string(XML_030);
    t.add_attribute(&sxe, "v:att11", "xxx", Some("urn::test-v"));
    t.add_attribute(&sxe, "v:att11", "xxx", Some("urn::test-v"));
    t.add_attribute(&sxe, "att2", "no-ns", None);
    let d = t.attributes(&sxe, None, false);
    t.add_child(&d, "m:test", Some("myval"), Some("urn::test"));
    t.add_child(&sxe, "m:test1", Some("myval"), Some("urn::test"));
    let _n = t.add_child(&sxe, "m:test2", Some("myval"), Some("urn::testnew"));
    t.add_child(&sxe, "test3", Some("myval"), Some("urn::testnew"));
    t.add_child(&sxe, "test4", Some("myval"), None);
    t.add_child(&sxe, "s:test5", Some("myval"), None);
    let x = t.as_xml(&sxe);
    let st = t.str(&x);
    t.echo(st);
    t.expect("031.phpt");
}

#[test]
fn t032_compare() {
    let mut t = T::new("032");
    let xml1 = "<people>\n  <person name=\"John\">\n    <children>\n      <person name=\"Joe\"/>\n    </children>\n  </person>\n  <person name=\"Jane\"/>\n</people>";
    let people = t.load_string(PEOPLE_029);
    let people1 = t.load_string(PEOPLE_029);
    let people2 = t.load_string(xml1);
    for (a, b) in [(&people1, &people), (&people2, &people), (&people2, &people1)] {
        let eq = php_runtime::ops::is_equal(a, b, &mut t.h).unwrap();
        t.var_dump(&Value::Bool(eq));
    }
    t.expect("032.phpt");
}

#[test]
fn t033_casts() {
    let mut t = T::new("033");
    let foo = t.load_string("<foo />");
    let people = t.load_string("<people>\ntest\n  <person name=\"Joe\"/>\n  <person name=\"John\">\n    <children>\n      <person name=\"Joe\"/>\n    </children>\n  </person>\n  <person name=\"Jane\"/>\n</people>");
    for v in [&foo, &people] {
        let b = t.bool(v);
        t.var_dump(&Value::Bool(b));
    }
    for v in [&foo, &people] {
        let i = v.to_long(&mut t.h).unwrap();
        t.var_dump(&Value::Int(i));
    }
    for v in [&foo, &people] {
        let f = v.to_double(&mut t.h).unwrap();
        t.var_dump(&Value::Float(f));
    }
    for v in [&foo, &people] {
        let st = t.str(v);
        t.var_dump(&s(&st));
    }
    for v in [&foo, &people] {
        let a = t.array(v);
        t.var_dump(&a);
    }
    for v in [&foo, &people] {
        t.var_dump(v);
    }
    t.expect("033.phpt");
}

const XML_034: &str = "<?xml version=\"1.0\"?>\n<foo><bar>\n   <p>Blah 1</p>\n   <p>Blah 2</p>\n   <p>Blah 3</p>\n   <tt>Blah 4</tt>\n</bar></foo>\n";

#[test]
fn t034_count_array_cast() {
    let mut t = T::new("034");
    let foo = t.load_string(XML_034);
    let p = t.path(&foo, &["bar", "p"]);
    let c = t.count(&p);
    t.echo(c.to_string());
    let a = t.array(&p);
    let n = a.as_array().map_or(0, |a| a.len());
    t.echo(n.to_string());
    t.expect("034.phpt");
}

#[test]
fn t035_to_string() {
    let mut t = T::new("035");
    let foo = t.load_string(XML_034);
    let p = t.path(&foo, &["bar", "p"]);
    for _ in 0..3 {
        let st = t.str(&p);
        t.echo(st + "\n");
    }
    t.expect("035.phpt");
}
