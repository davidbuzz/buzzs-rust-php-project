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
