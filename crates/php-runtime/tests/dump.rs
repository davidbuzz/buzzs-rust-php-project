//! `var_dump` / `print_r` / `var_export` output compared with the PHP oracle
//! (`tests/data/dump.php` → `tests/data/dump_expected.txt`).

use php_runtime::dump::{print_r, var_dump, var_export};
use php_runtime::{ArrayKey, CollectingContext, HashTable, PhpRef, Value, ZArray};

fn sample() -> Value {
    let mut t = HashTable::new();
    for v in [
        Value::Int(1),
        Value::Float(1.5),
        Value::from(&b"x\0y"[..]),
        Value::Bool(true),
        Value::Bool(false),
        Value::Null,
    ] {
        t.push(v).unwrap();
    }
    let mut inner = HashTable::new();
    inner.insert("a", Value::empty_array());
    inner.insert("b'\\", Value::from("q'\\"));
    t.push(Value::Array(inner.into())).unwrap();
    for v in [
        Value::Float(-0.0),
        Value::Float(1e100),
        Value::Float(f64::NAN),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NEG_INFINITY),
        Value::Int(i64::MIN),
        Value::Float(0.1 + 0.2),
        Value::Float(1e15),
    ] {
        t.push(v).unwrap();
    }
    t.insert("07", Value::Int(7));
    t.insert(-3, Value::from("neg"));
    Value::Array(t.into())
}

#[test]
fn dumps_match_php() {
    let mut ctx = CollectingContext::new();
    let mut out = Vec::new();
    let v = sample();
    var_dump(&mut out, &v, &mut ctx).unwrap();
    out.extend_from_slice(b"--\n");
    out.extend(print_r(&v, &mut ctx).unwrap());
    out.extend_from_slice(b"--\n");
    out.extend(var_export(&v, &mut ctx).unwrap());
    out.extend_from_slice(b"\n--\n");

    // $x = 1; $r = [&$x, 2];
    let x = PhpRef::new(Value::Int(1));
    let r = ZArray::from_values([Value::Reference(x.clone()), Value::Int(2)]);
    var_dump(&mut out, &Value::Array(r), &mut ctx).unwrap();

    // $a = []; $a[] = &$a; $a["k"] = 1;
    let aref = PhpRef::new(Value::empty_array());
    {
        let mut inner = aref.get();
        if let Value::Array(arr) = &mut inner {
            arr.make_mut().push(Value::Reference(aref.clone())).unwrap();
            arr.make_mut().insert(ArrayKey::from("k"), Value::Int(1));
        }
        aref.set(inner);
    }
    let a = aref.get();
    var_dump(&mut out, &a, &mut ctx).unwrap();
    out.extend(print_r(&a, &mut ctx).unwrap());
    out.push(b'\n');
    out.extend(var_export(&a, &mut ctx).unwrap());
    out.push(b'\n');
    assert_eq!(
        ctx.messages(),
        ["Warning: var_export does not handle circular references"]
    );

    let expected = include_bytes!("data/dump_expected.txt");
    if out != expected {
        let got = String::from_utf8_lossy(&out);
        let want = String::from_utf8_lossy(expected);
        for (i, (g, w)) in got.lines().zip(want.lines()).enumerate() {
            assert_eq!(g, w, "line {}", i + 1);
        }
        assert_eq!(got, want);
    }
}

mod objects {
    use php_runtime::dump::{print_r, var_dump, var_export};
    use php_runtime::{
        ArrayKey, ErrorLevel, Obj, ObjectDump, ObjectKind, PhpObject, PropertyPurpose,
        PropertySlot, RtResult, RuntimeContext, Value, ZArray, ZStr,
    };
    use std::any::Any;
    use std::cell::RefCell;

    struct TestObj {
        id: u32,
        class: &'static str,
        kind: ObjectKind,
        props: RefCell<Vec<(ArrayKey, PropertySlot)>>,
    }

    impl PhpObject for TestObj {
        fn handle(&self) -> u32 {
            self.id
        }
        fn class_name(&self) -> ZStr {
            ZStr::from(self.class)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[derive(Default)]
    struct Ctx(Vec<String>);

    impl RuntimeContext for Ctx {
        fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
            self.0.push(format!("{}: {message}", level.label()));
            Ok(())
        }
        fn object_dump(&mut self, obj: &Obj, _purpose: PropertyPurpose) -> RtResult<ObjectDump> {
            let o = obj.downcast_ref::<TestObj>().expect("test object");
            Ok(ObjectDump {
                kind: o.kind.clone(),
                properties: Some(o.props.borrow().clone()),
            })
        }
    }

    fn key(s: &[u8]) -> ArrayKey {
        ArrayKey::Str(ZStr::from(s))
    }

    fn class_a() -> Value {
        Value::Object(Obj::new(TestObj {
            id: 1,
            class: "A",
            kind: ObjectKind::Regular { lazy_prefix: "" },
            props: RefCell::new(vec![
                (key(b"x"), PropertySlot::Uninitialized(ZStr::from("int"))),
                (key(b"\0*\0p"), PropertySlot::Value(Value::Int(1))),
                (
                    key(b"\0A\0q"),
                    PropertySlot::Value(Value::Array(ZArray::from_values([Value::Int(1)]))),
                ),
                (key(b"o"), PropertySlot::Value(Value::Null)),
            ]),
        }))
    }

    #[test]
    fn class_with_visibility_and_uninitialized() {
        let mut ctx = Ctx::default();
        let a = class_a();
        let mut out = Vec::new();
        var_dump(&mut out, &a, &mut ctx).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "object(A)#1 (3) {\n  [\"x\"]=>\n  uninitialized(int)\n  [\"p\":protected]=>\n  int(1)\n  [\"q\":\"A\":private]=>\n  array(1) {\n    [0]=>\n    int(1)\n  }\n  [\"o\"]=>\n  NULL\n}\n"
        );
        assert_eq!(
            String::from_utf8(print_r(&a, &mut ctx).unwrap()).unwrap(),
            "A Object\n(\n    [p:protected] => 1\n    [q:A:private] => Array\n        (\n            [0] => 1\n        )\n\n    [o] => \n)\n"
        );
        assert_eq!(
            String::from_utf8(var_export(&a, &mut ctx).unwrap()).unwrap(),
            "\\A::__set_state(array(\n   'p' => 1,\n   'q' => \n  array (\n    0 => 1,\n  ),\n   'o' => NULL,\n))"
        );
    }

    #[test]
    fn std_class_enum_and_recursion() {
        let mut ctx = Ctx::default();
        let std = Obj::new(TestObj {
            id: 4,
            class: "stdClass",
            kind: ObjectKind::StdClass,
            props: RefCell::new(vec![
                (key(b"0"), PropertySlot::Value(Value::Int(1))),
                (
                    key(b"b"),
                    PropertySlot::Value(Value::Array(ZArray::from_values([Value::Int(1)]))),
                ),
            ]),
        });
        assert_eq!(
            String::from_utf8(var_export(&Value::Object(std.clone()), &mut ctx).unwrap()).unwrap(),
            "(object) array(\n   '0' => 1,\n   'b' => \n  array (\n    0 => 1,\n  ),\n)"
        );
        // $o->self = $o
        if let Some(o) = std.downcast_ref::<TestObj>() {
            o.props.borrow_mut().push((
                key(b"self"),
                PropertySlot::Value(Value::Object(std.clone())),
            ));
        }
        let mut out = Vec::new();
        var_dump(&mut out, &Value::Object(std.clone()), &mut ctx).unwrap();
        assert!(String::from_utf8_lossy(&out).ends_with("  [\"self\"]=>\n  *RECURSION*\n}\n"));
        let p = String::from_utf8(print_r(&Value::Object(std.clone()), &mut ctx).unwrap()).unwrap();
        assert!(p.contains("    [self] => stdClass Object\n *RECURSION*\n"));
        let e = Obj::new(TestObj {
            id: 9,
            class: "E",
            kind: ObjectKind::Enum {
                case_name: ZStr::from("X"),
                backing_type: None,
                has_debug_info: false,
            },
            props: RefCell::new(vec![]),
        });
        let mut out = Vec::new();
        var_dump(&mut out, &Value::Object(e.clone()), &mut ctx).unwrap();
        assert_eq!(out, b"enum(E::X)\n");
        let arr = Value::Array(ZArray::from_values([Value::Object(e)]));
        assert_eq!(
            String::from_utf8(var_export(&arr, &mut ctx).unwrap()).unwrap(),
            "array (\n  0 => \n  \\E::X,\n)"
        );
        // break the Rc cycle
        if let Some(o) = std.downcast_ref::<TestObj>() {
            o.props.borrow_mut().clear();
        }
    }
}
