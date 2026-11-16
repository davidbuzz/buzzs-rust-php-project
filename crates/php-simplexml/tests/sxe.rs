// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Ports of `ext/simplexml/tests/sxe_*.phpt` and `SimpleXMLElement_*.phpt`.

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

use common::{fixture, s, trim, T};
use php_runtime::Value;
use php_simplexml::{FetchMode, Sxe};

const SXE_XML: &str = r#"<?xml version='1.0'?>
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

#[test]
fn sxe_001_iterator_class() {
    let mut t = T::new("sxe_001");
    let xml = r#"<?xml version='1.0'?>
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
    let v = t.load_string_ex(xml, Some("SimpleXMLIterator"), 0, "", false);
    t.var_dump(&v);
    t.expect("sxe_001.phpt");
}

fn dump_entry(t: &mut T, name: &Value, data: &Value) {
    t.var_dump(name);
    t.var_dump(&s(&T::class_of(data)));
    let st = trim(&t.str(data));
    t.var_dump(&s(&st));
}

#[test]
fn sxe_002_recursive_self_first() {
    let mut t = T::new("sxe_002");
    let sxe = t.load_string_ex(SXE_XML, Some("SimpleXMLIterator"), 0, "", false);
    let _iterator = common::new_object("RecursiveIteratorIterator", None);
    for (k, v) in t.rii(&sxe, 1) {
        dump_entry(&mut t, &k, &v);
    }
    t.echo("===DUMP===\n");
    t.var_dump(&s(&T::class_of(&sxe)));
    let e = t.prop(&sxe, "elem1");
    let st = trim(&t.str(&e));
    t.var_dump(&s(&st));
    t.expect("sxe_002.phpt");
}

#[test]
fn sxe_003_manual_iteration() {
    let mut t = T::new("sxe_003");
    let sxe = t.load_string_ex(SXE_XML, Some("SimpleXMLIterator"), 0, "", false);
    let sx = t.sxe(&sxe);
    let ch = Sxe::get_children(&sx);
    if let Value::Object(_) = ch {
        t.foreach(&ch, |t, k, v| dump_entry(t, &k, &v));
    } else {
        t.echo("\nWarning: foreach() argument must be of type array|object, null given in /t/sxe_003.php on line 1\n");
    }
    t.echo("===RESET===\n");
    Sxe::rewind(&sx, &mut t.h).unwrap();
    while Sxe::valid(&sx) {
        let h = Sxe::has_children(&sx, &mut t.h).unwrap();
        t.var_dump(&Value::Bool(h));
        let k = Sxe::key(&sx, &mut t.h).unwrap();
        t.var_dump(&s(&trim(&String::from_utf8_lossy(&k))));
        let c = Sxe::current(&sx).unwrap();
        let st = trim(&t.str(&c));
        t.var_dump(&s(&st));
        let ch = Sxe::get_children(&sx);
        t.foreach(&ch, |t, k, v| dump_entry(t, &k, &v));
        Sxe::next(&sx, &mut t.h).unwrap();
    }
    t.expect("sxe_003.phpt");
}

#[test]
fn sxe_ctor_nul_path() {
    let mut t = T::new("sxe_ctor_nul_path");
    for (xml, url) in [("/tmp/x\0evil", true), ("<r/>\0evil", false)] {
        match t.new_sxe_ex("SimpleXMLElement", xml, 0, url, "", false) {
            Ok(_) => t.echo("loaded\n"),
            Err(e) => t.exception(&e),
        }
    }
    let r = Sxe::load_file(&mut t.h, b"/tmp/x\0evil", None, 0, b"", false);
    t.catch(r);
    t.expect("sxe_ctor_nul_path.phpt");
}

fn state(t: &mut T, x: &Value) -> String {
    let xml = t.as_xml(x);
    let st = t.str(&xml);
    let i = st.find("<r").unwrap();
    trim(&st[i..])
}

#[test]
fn sxe_int_offset_no_alias() {
    let mut t = T::new("sxe_int_offset_no_alias");
    let src = "<r a=\"1\" b=\"2\"><item>a</item><item>b</item><item>c</item></r>";
    t.echo("== element list ==\n");
    let x = t.load_string(src);
    let item = t.prop(&x, "item");
    let b = t.isset_dim(&item, Value::Int(-1));
    t.var_dump(&Value::Bool(b));
    let v = t.dim(&item, Value::Int(-1));
    t.var_dump(&v);
    let v = t.dim(&item, Value::Int(0));
    let st = t.str(&v);
    t.var_dump(&s(&st));
    let item = t.prop_w(&x, "item");
    t.set_dim(&item, Some(Value::Int(-1)), s("Z"));
    let st = state(&mut t, &x);
    t.echo(st + "\n");
    t.unset_dim(&item, Value::Int(-1));
    let st = state(&mut t, &x);
    t.echo(st + "\n");
    t.set_dim(&item, Some(Value::Int(5)), s("P"));
    let st = state(&mut t, &x);
    t.echo(st + "\n");
    t.echo("== single element ==\n");
    let x = t.load_string(src);
    let item = t.prop(&x, "item");
    let n = t.dim(&item, Value::Int(0));
    let b = t.isset_dim(&n, Value::Int(-1));
    t.var_dump(&Value::Bool(b));
    let v = t.dim(&n, Value::Int(-1));
    t.var_dump(&v);
    let v = t.dim(&n, Value::Int(5));
    t.var_dump(&v);
    t.set_dim(&n, Some(Value::Int(-1)), s("Z"));
    let st = state(&mut t, &x);
    t.echo(st + "\n");
    t.set_dim(&n, Some(Value::Int(5)), s("Y"));
    let st = state(&mut t, &x);
    t.echo(st + "\n");
    t.unset_dim(&n, Value::Int(-1));
    let st = state(&mut t, &x);
    t.echo(st + "\n");
    let v = t.dim(&n, Value::Int(0));
    let st = t.str(&v);
    t.var_dump(&s(&st));
    t.echo("== nested write ==\n");
    let x = t.load_string(src);
    let item = t.prop_w(&x, "item");
    let v = t.dim_mode(&item, Some(Value::Int(-1)), FetchMode::Write);
    if v.is_null() {
        t.echo("\nNotice: Indirect modification of overloaded element of SimpleXMLElement has no effect in /t/x.php on line 1\n");
        t.echo("Error: Attempt to assign property \"kid\" on null\n");
    }
    let st = state(&mut t, &x);
    t.echo(st + "\n");
    t.echo("== attributes ==\n");
    let x = t.load_string(src);
    let at = t.attributes(&x, None, false);
    let b = t.isset_dim(&at, Value::Int(-1));
    t.var_dump(&Value::Bool(b));
    let v = t.dim(&at, Value::Int(-1));
    t.var_dump(&v);
    t.set_dim(&at, Some(Value::Int(-1)), s("z"));
    let st = state(&mut t, &x);
    t.echo(st + "\n");
    t.expect("sxe_int_offset_no_alias.phpt");
}

#[test]
fn add_attribute_basic() {
    let mut t = T::new("SimpleXMLElement_addAttribute_basic");
    let simple = t.load_file(&fixture("book.xml"));
    t.add_attribute(&simple, "type", "novels", None);
    let a = t.attributes(&simple, None, false);
    t.var_dump(&a);
    t.echo("Done");
    t.expect("SimpleXMLElement_addAttribute_basic.phpt");
}

#[test]
fn add_attribute_required_name() {
    let mut t = T::new("SimpleXMLElement_addAttribute_required_attribute_name");
    let a = t.new_sxe("<php>testfest</php>");
    t.add_attribute(&a, "", "", None);
    let x = t.as_xml(&a);
    let st = t.str(&x);
    t.echo(st);
    t.expect("SimpleXMLElement_addAttribute_required_attribute_name.phpt");
}

#[test]
fn as_xml_fragment_filename() {
    let mut t = T::new("SimpleXMLElement_asXML_fragment_filename");
    let sxe = t.load_string("<?xml version=\"1.0\"?>\n<container>\n    <container2>\n        <child id=\"foo\">bar</child>\n    </container2>\n</container>");
    let dir = std::env::temp_dir().join(format!("php-simplexml-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("fragment.tmp");
    let c2 = t.prop(&sxe, "container2");
    let sx = t.sxe(&c2);
    let r = Sxe::as_xml(&sx, Some(out.to_string_lossy().as_bytes())).unwrap();
    assert!(matches!(r, Value::Bool(true)));
    let content = std::fs::read_to_string(&out).unwrap();
    t.echo(content);
    let _ = std::fs::remove_file(&out);
    t.expect("SimpleXMLElement_asXML_fragment_filename.phpt");
}
