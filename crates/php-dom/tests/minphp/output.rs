//! var_dump / print_r / string conversion.

use super::value::{Key, ObjKind, V};
use php_dom::dispatch;
use php_dom::DomValue;

pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NAN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "INF".into() } else { "-INF".into() };
    }
    if f == f.trunc() && f.abs() < 1e15 {
        if f == 0.0 && f.is_sign_negative() {
            return "-0".into();
        }
        return format!("{}", f as i64);
    }
    let s = format!("{f:?}");
    if let Some((m, e)) = s.split_once('e') {
        let m = if m.contains('.') {
            m.to_string()
        } else {
            format!("{m}.0")
        };
        let e: i32 = e.parse().unwrap_or(0);
        return format!("{m}E{}{}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    s
}

pub fn to_bytes(v: &V) -> Vec<u8> {
    match v {
        V::Null => Vec::new(),
        V::Bool(b) => {
            if *b {
                b"1".to_vec()
            } else {
                Vec::new()
            }
        }
        V::Int(i) => i.to_string().into_bytes(),
        V::Float(f) => dispatch::float_to_string(*f).into_bytes(),
        V::Str(s) => s.clone(),
        V::Arr(_) => b"Array".to_vec(),
        V::Obj(o) => o.class.borrow().clone().into_bytes(),
    }
}

fn indent(out: &mut Vec<u8>, n: usize) {
    for _ in 0..n {
        out.push(b' ');
    }
}

pub fn obj_props(o: &super::value::Obj) -> Vec<(Vec<u8>, V)> {
    let mut props = o.props.borrow().clone();
    if let Some(d) = o.dom() {
        for (name, v) in dispatch::debug_info(&d) {
            let vv = super::interp::from_dom_plain(v);
            if let Some(slot) = props.iter_mut().find(|(k, _)| k == name.as_bytes()) {
                slot.1 = vv;
            } else {
                props.push((name.as_bytes().to_vec(), vv));
            }
        }
    }
    if let ObjKind::Enum(_, case) = &*o.kind.borrow() {
        let _ = case;
    }
    props
}

pub fn var_dump(out: &mut Vec<u8>, v: &V, level: usize) {
    indent(out, level);
    match v {
        V::Null => out.extend_from_slice(b"NULL\n"),
        V::Bool(b) => out.extend_from_slice(format!("bool({b})\n").as_bytes()),
        V::Int(i) => out.extend_from_slice(format!("int({i})\n").as_bytes()),
        V::Float(f) => out.extend_from_slice(format!("float({})\n", float_repr(*f)).as_bytes()),
        V::Str(s) => {
            out.extend_from_slice(format!("string({}) \"", s.len()).as_bytes());
            out.extend_from_slice(s);
            out.extend_from_slice(b"\"\n");
        }
        V::Arr(a) => {
            out.extend_from_slice(format!("array({}) {{\n", a.len()).as_bytes());
            for (k, v) in &a.items {
                indent(out, level + 2);
                match k {
                    Key::Int(i) => out.extend_from_slice(format!("[{i}]=>\n").as_bytes()),
                    Key::Str(s) => {
                        out.extend_from_slice(b"[\"");
                        out.extend_from_slice(s);
                        out.extend_from_slice(b"\"]=>\n");
                    }
                }
                var_dump(out, v, level + 2);
            }
            indent(out, level);
            out.extend_from_slice(b"}\n");
        }
        V::Obj(o) => {
            if let ObjKind::Enum(c, case) = &*o.kind.borrow() {
                out.extend_from_slice(format!("enum({c}::{case})\n").as_bytes());
                return;
            }
            let props = obj_props(o);
            out.extend_from_slice(
                format!(
                    "object({})#{} ({}) {{\n",
                    o.class.borrow(),
                    o.id,
                    props.len()
                )
                .as_bytes(),
            );
            for (k, v) in &props {
                indent(out, level + 2);
                out.extend_from_slice(b"[\"");
                out.extend_from_slice(k);
                out.extend_from_slice(b"\"]=>\n");
                var_dump(out, v, level + 2);
            }
            indent(out, level);
            out.extend_from_slice(b"}\n");
        }
    }
}

pub fn print_r(out: &mut Vec<u8>, v: &V, level: usize) {
    match v {
        V::Arr(a) => {
            out.extend_from_slice(b"Array\n");
            indent(out, level);
            out.extend_from_slice(b"(\n");
            for (k, v) in &a.items {
                indent(out, level + 4);
                out.push(b'[');
                match k {
                    Key::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
                    Key::Str(s) => out.extend_from_slice(s),
                }
                out.extend_from_slice(b"] => ");
                print_r(out, v, level + 8);
                out.push(b'\n');
            }
            indent(out, level);
            out.extend_from_slice(b")\n");
        }
        V::Obj(o) => {
            if let ObjKind::Enum(c, case) = &*o.kind.borrow() {
                out.extend_from_slice(format!("{c} Enum\n").as_bytes());
                indent(out, level);
                out.extend_from_slice(b"(\n");
                indent(out, level + 4);
                out.extend_from_slice(format!("[name] => {case}\n").as_bytes());
                indent(out, level);
                out.extend_from_slice(b")\n");
                return;
            }
            out.extend_from_slice(format!("{} Object\n", o.class.borrow()).as_bytes());
            indent(out, level);
            out.extend_from_slice(b"(\n");
            for (k, v) in obj_props(o) {
                indent(out, level + 4);
                out.push(b'[');
                out.extend_from_slice(&k);
                out.extend_from_slice(b"] => ");
                print_r(out, &v, level + 8);
                out.push(b'\n');
            }
            indent(out, level);
            out.extend_from_slice(b")\n");
        }
        other => out.extend_from_slice(&to_bytes(other)),
    }
}

pub fn var_export(out: &mut Vec<u8>, v: &V, level: usize) {
    match v {
        V::Null => out.extend_from_slice(b"NULL"),
        V::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        V::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
        V::Float(f) => {
            let s = float_repr(*f);
            out.extend_from_slice(s.as_bytes());
            if !s.contains('.') && !s.contains('E') && !s.contains('N') && !s.contains('I') {
                out.extend_from_slice(b".0");
            }
        }
        V::Str(s) => {
            out.push(b'\'');
            for &c in s {
                if c == b'\'' || c == b'\\' {
                    out.push(b'\\');
                }
                out.push(c);
            }
            out.push(b'\'');
        }
        V::Arr(a) => {
            out.extend_from_slice(b"array (\n");
            for (k, v) in &a.items {
                indent(out, level + 2);
                match k {
                    Key::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
                    Key::Str(s) => {
                        out.push(b'\'');
                        out.extend_from_slice(s);
                        out.push(b'\'');
                    }
                }
                out.extend_from_slice(b" => ");
                if matches!(v, V::Arr(_) | V::Obj(_)) {
                    out.push(b'\n');
                    indent(out, level + 2);
                }
                var_export(out, v, level + 2);
                out.extend_from_slice(b",\n");
            }
            indent(out, level);
            out.push(b')');
        }
        V::Obj(o) => {
            if let ObjKind::Enum(c, case) = &*o.kind.borrow() {
                out.extend_from_slice(format!("\\{c}::{case}").as_bytes());
                return;
            }
            out.extend_from_slice(
                format!("\\{}::__set_state(array(\n", o.class.borrow()).as_bytes(),
            );
            for (k, v) in obj_props(o) {
                indent(out, level + 3);
                out.push(b'\'');
                out.extend_from_slice(&k);
                out.extend_from_slice(b"' => ");
                var_export(out, &v, level + 2);
                out.extend_from_slice(b",\n");
            }
            indent(out, level);
            out.extend_from_slice(b"))");
        }
    }
}

pub fn dom_value_type(v: &DomValue) -> String {
    v.type_name()
}
