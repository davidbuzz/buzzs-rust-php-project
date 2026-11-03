//! Randomised operation sequences over SimpleXML objects: no panics, no
//! memory errors (run under valgrind for the latter), consistent results.

mod common;

use common::{s, T};
use php_runtime::Value;
use php_simplexml::{CheckKind, FetchMode, Sxe};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % (n as u64)).unwrap()
    }
}

const DOCS: &[&str] = &[
    "<r a=\"1\" b=\"2\"><item>a</item><item>b</item><x:item xmlns:x=\"urn:x\" x:k=\"v\">c</x:item><!--c--><?pi d?></r>",
    "<?xml version=\"1.0\"?>\n<!DOCTYPE r [<!ENTITY e \"ent\">]>\n<r>&e;<a><b><c/></b></a>text<a/></r>",
    "<people><person name=\"Joe\"><child/></person><person/></people>",
    "<r xmlns=\"urn:d\" xmlns:p=\"urn:p\"><p:a p:x=\"1\">t</p:a><b/></r>",
];

const NAMES: &[&str] = &["item", "a", "b", "c", "person", "child", "x", "k", "name", "", "0", "e"];

fn rand_value(r: &mut Rng) -> Value {
    match r.below(6) {
        0 => Value::Int(i64::try_from(r.below(5)).unwrap() - 1),
        1 => s(NAMES[r.below(NAMES.len())]),
        2 => Value::Null,
        3 => Value::Bool(r.below(2) == 0),
        4 => Value::Float(1.5),
        _ => s("v&<>\"'"),
    }
}

fn step(t: &mut T, r: &mut Rng, pool: &mut Vec<Value>) {
    let v = pool[r.below(pool.len())].clone();
    if !matches!(v, Value::Object(_)) {
        return;
    }
    let sx = t.sxe(&v);
    let name = NAMES[r.below(NAMES.len())];
    let mode = [FetchMode::Read, FetchMode::Write, FetchMode::IsSet][r.below(3)];
    let res: Option<Value> = match r.below(26) {
        0 => Sxe::read_property(&sx, &mut t.h, name.as_bytes(), mode).ok(),
        1 => {
            let k = rand_value(r);
            Sxe::read_dimension(&sx, &mut t.h, Some(&k), mode).ok()
        }
        2 => {
            let val = rand_value(r);
            let _ = Sxe::write_property(&sx, &mut t.h, name.as_bytes(), &val);
            None
        }
        3 => {
            let k = rand_value(r);
            let val = rand_value(r);
            let key = if r.below(4) == 0 { None } else { Some(&k) };
            let _ = Sxe::write_dimension(&sx, &mut t.h, key, &val);
            None
        }
        4 => {
            let _ = Sxe::unset_property(&sx, &mut t.h, name.as_bytes());
            None
        }
        5 => {
            let k = rand_value(r);
            let _ = Sxe::unset_dimension(&sx, &mut t.h, &k);
            None
        }
        6 => {
            let _ = Sxe::has_property(&sx, &mut t.h, name.as_bytes(), CheckKind::NotEmpty);
            let k = rand_value(r);
            let _ = Sxe::has_dimension(&sx, &mut t.h, &k, CheckKind::Isset);
            None
        }
        7 => Sxe::children(&sx, &mut t.h, None, false).ok(),
        8 => Sxe::attributes(&sx, &mut t.h, None, false).ok(),
        9 => Sxe::xpath(&sx, &mut t.h, ["//*", "//@*", "//text()", "..", "count(//*)", "//comment()"][r.below(6)].as_bytes()).ok(),
        10 => {
            let _ = Sxe::properties(&sx, &mut t.h, r.below(2) == 0);
            None
        }
        11 => {
            let mut b = Vec::new();
            let _ = php_runtime::dump::var_dump(&mut b, &v, &mut t.h);
            None
        }
        12 => Sxe::clone_object(&sx).ok().map(|c| common::new_object("SimpleXMLElement", Some(c))),
        13 => {
            let _ = Sxe::rewind(&sx, &mut t.h);
            Sxe::iter_current(&sx)
        }
        14 => {
            let _ = Sxe::next(&sx, &mut t.h);
            let _ = Sxe::has_children(&sx, &mut t.h);
            Some(Sxe::get_children(&sx))
        }
        15 => Sxe::add_child(&sx, &mut t.h, name.as_bytes(), Some(b"v"), [None, Some(&b"urn:x"[..]), Some(&b""[..])][r.below(3)]).ok(),
        16 => {
            let _ = Sxe::add_attribute(&sx, &mut t.h, ["q", "p:q", "a"][r.below(3)].as_bytes(), b"1", [None, Some(&b"urn:p"[..])][r.below(2)]);
            None
        }
        17 => {
            let _ = Sxe::as_xml(&sx, None);
            let _ = Sxe::get_name(&sx);
            None
        }
        18 => {
            let _ = Sxe::cast(&sx, &mut t.h, php_runtime::CastTarget::String);
            let _ = Sxe::cast(&sx, &mut t.h, php_runtime::CastTarget::Bool);
            let _ = Sxe::count(&sx);
            None
        }
        19 => {
            let _ = Sxe::get_namespaces(&sx, true);
            let _ = Sxe::get_doc_namespaces(&sx, true, r.below(2) == 0);
            None
        }
        20 => match Sxe::property_ptr(&sx, &mut t.h, name.as_bytes()) {
            Ok(php_simplexml::PropertyPtr::Value(x)) => Some(x),
            _ => None,
        },
        21 => {
            let other = pool[r.below(pool.len())].clone();
            if let Value::Object(o) = &other {
                if let Some(os) = common::Host::unwrap(&mut t.h, o) {
                    let _ = Sxe::compare(&sx, &os);
                }
            }
            None
        }
        22 => {
            let _ = Sxe::construct(&sx, DOCS[r.below(DOCS.len())].as_bytes(), 0, false, b"", false);
            None
        }
        23 => {
            let _ = Sxe::register_xpath_namespace(&sx, b"x", b"urn:x");
            Sxe::xpath(&sx, &mut t.h, b"//x:item").ok()
        }
        24 => Sxe::export_node(&sx).ok().flatten().and_then(|n| {
            Sxe::import_dom(&mut t.h, Some(n), None).ok()
        }),
        _ => {
            pool.swap_remove(r.below(pool.len()));
            None
        }
    };
    if let Some(Value::Object(_)) = &res {
        pool.push(res.unwrap_or(Value::Null));
    } else if let Some(Value::Array(a)) = &res {
        pool.extend(a.values().filter(|x| matches!(x, Value::Object(_))).cloned());
    }
    if pool.len() > 64 {
        pool.swap_remove(r.below(pool.len()));
    }
}

#[test]
fn random_operation_sequences() {
    let iterations: usize = std::env::var("SXE_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
    for seed in 1..=40u64 {
        let mut t = T::new("fuzz");
        php_libxml::error::use_internal_errors(Some(true));
        let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut pool = Vec::new();
        for d in DOCS {
            pool.push(t.load_string(d));
        }
        for _ in 0..iterations {
            if pool.is_empty() {
                pool.push(t.load_string(DOCS[r.below(DOCS.len())]));
            }
            step(&mut t, &mut r, &mut pool);
        }
    }
}
