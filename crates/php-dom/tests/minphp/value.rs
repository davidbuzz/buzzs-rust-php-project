//! Values of the mini PHP interpreter used to run `.phpt` tests.

use std::cell::RefCell;
use std::rc::Rc;

use php_dom::dispatch::{DomObject, IterHandle};

#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Int(i64),
    Str(Vec<u8>),
}

#[derive(Clone, Debug, Default)]
pub struct Arr {
    pub items: Vec<(Key, V)>,
    pub next: i64,
}

impl Arr {
    pub fn get(&self, k: &Key) -> Option<&V> {
        self.items.iter().find(|(kk, _)| kk == k).map(|(_, v)| v)
    }
    pub fn get_mut(&mut self, k: &Key) -> Option<&mut V> {
        self.items
            .iter_mut()
            .find(|(kk, _)| kk == k)
            .map(|(_, v)| v)
    }
    pub fn set(&mut self, k: Key, v: V) {
        if let Key::Int(i) = k {
            if i >= self.next {
                self.next = i + 1;
            }
        }
        if let Some(slot) = self.get_mut(&k) {
            *slot = v;
        } else {
            self.items.push((k, v));
        }
    }
    pub fn push(&mut self, v: V) {
        let k = Key::Int(self.next);
        self.next += 1;
        self.items.push((k, v));
    }
    pub fn remove(&mut self, k: &Key) {
        self.items.retain(|(kk, _)| kk != k);
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
}

pub type ArrRef = Rc<Arr>;

#[derive(Clone, Debug)]
pub enum V {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Vec<u8>),
    Arr(ArrRef),
    Obj(Rc<Obj>),
}

impl V {
    pub fn str(s: &str) -> V {
        V::Str(s.as_bytes().to_vec())
    }
    pub fn arr(a: Arr) -> V {
        V::Arr(Rc::new(a))
    }
    pub fn type_name(&self) -> String {
        match self {
            V::Null => "null".into(),
            V::Bool(_) => "bool".into(),
            V::Int(_) => "int".into(),
            V::Float(_) => "float".into(),
            V::Str(_) => "string".into(),
            V::Arr(_) => "array".into(),
            V::Obj(o) => o.class.borrow().clone(),
        }
    }
    pub fn truthy(&self) -> bool {
        match self {
            V::Null => false,
            V::Bool(b) => *b,
            V::Int(i) => *i != 0,
            V::Float(f) => *f != 0.0,
            V::Str(s) => !(s.is_empty() || s == b"0"),
            V::Arr(a) => a.len() > 0,
            V::Obj(_) => true,
        }
    }
}

pub struct UserFunc {
    pub name: Vec<u8>,
    pub params: Vec<php_parser::ast::Param>,
    pub body: Vec<php_parser::ast::Stmt>,
    pub is_static: bool,
}

pub enum Callable {
    Func(Rc<UserFunc>),
    Closure {
        func: Rc<UserFunc>,
        bound: Vec<(Vec<u8>, V)>,
        this: Option<V>,
        scope: Option<String>,
        arrow: Option<Rc<php_parser::ast::Expr>>,
    },
    Builtin(String),
    Method(V, Vec<u8>),
    StaticMethod(String, Vec<u8>),
}

pub enum ObjKind {
    Plain,
    Dom(DomObject),
    Closure(Rc<Callable>),
    Iter(IterHandle),
    ArrayIter { arr: ArrRef, pos: usize },
    Enum(&'static str, &'static str),
}

pub struct Obj {
    pub id: usize,
    pub gen: usize,
    pub class: RefCell<String>,
    pub kind: RefCell<ObjKind>,
    pub props: RefCell<Vec<(Vec<u8>, V)>>,
    /// Objects kept alive by this one (e.g. XPath result nodes).
    pub keep: RefCell<Vec<V>>,
}

impl std::fmt::Debug for Obj {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Obj#{}({})", self.id, self.class.borrow())
    }
}

impl Obj {
    pub fn prop(&self, name: &[u8]) -> Option<V> {
        self.props
            .borrow()
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }
    pub fn set_prop(&self, name: &[u8], v: V) {
        let mut p = self.props.borrow_mut();
        if let Some(slot) = p.iter_mut().find(|(k, _)| k == name) {
            slot.1 = v;
        } else {
            p.push((name.to_vec(), v));
        }
    }
    pub fn dom(&self) -> Option<DomObject> {
        match &*self.kind.borrow() {
            ObjKind::Dom(d) => Some(d.clone()),
            _ => None,
        }
    }
}

thread_local! {
    pub static NEXT_ID: std::cell::Cell<usize> = const { std::cell::Cell::new(1) };
    pub static FREE_IDS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    pub static GEN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Starts a fresh object-handle space (one per test script).
pub fn reset_ids() {
    GEN.with(|g| g.set(g.get() + 1));
    NEXT_ID.with(|n| n.set(1));
    FREE_IDS.with(|f| f.borrow_mut().clear());
}

pub fn new_obj(class: &str, kind: ObjKind) -> Rc<Obj> {
    let id = FREE_IDS.with(|f| f.borrow_mut().pop()).unwrap_or_else(|| {
        NEXT_ID.with(|n| {
            let v = n.get();
            n.set(v + 1);
            v
        })
    });
    Rc::new(Obj {
        id,
        gen: GEN.with(std::cell::Cell::get),
        class: RefCell::new(class.to_string()),
        kind: RefCell::new(kind),
        props: RefCell::new(Vec::new()),
        keep: RefCell::new(Vec::new()),
    })
}

impl Drop for Obj {
    fn drop(&mut self) {
        // Owned objects and properties are released before the handle
        // itself goes back to the free list (zend_objects_store_del order).
        drop(std::mem::take(&mut *self.keep.borrow_mut()));
        drop(std::mem::take(&mut *self.props.borrow_mut()));
        let id = self.id;
        if id != 0 && self.gen == GEN.with(std::cell::Cell::get) {
            FREE_IDS.with(|f| f.borrow_mut().push(id));
        }
    }
}

/// An object that does not consume an object handle (engine-internal).
pub fn new_internal(class: &str, kind: ObjKind) -> Rc<Obj> {
    Rc::new(Obj {
        id: 0,
        gen: 0,
        class: RefCell::new(class.to_string()),
        kind: RefCell::new(kind),
        props: RefCell::new(Vec::new()),
        keep: RefCell::new(Vec::new()),
    })
}
