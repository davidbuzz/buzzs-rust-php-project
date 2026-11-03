//! `RuntimeContext` / `ArrayContext` / `SplHost` for the test interpreter.

use super::interp::*;
use php_runtime::serialize::SerializedObject;
use php_runtime::{
    ops, ArrayKey, CastTarget, ErrorLevel, HashTable, ObjectDump, ObjectKind, Obj, PropertyPurpose,
    PropertySlot, RtResult, RuntimeContext, RuntimeError, Value, ZArray, ZStr,
};
use php_spl::{ClassInfo, ObjectIterator, SplHost, SplObject};
use php_stdarr::ArrayContext;
use std::rc::Rc;

impl RuntimeContext for Interp {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        self.emit(level, message)
    }

    fn cast_object(&mut self, obj: &Obj, target: CastTarget) -> RtResult<Option<Value>> {
        match target {
            CastTarget::Bool => Ok(Some(Value::Bool(true))),
            CastTarget::String => {
                let cls = tobj(obj).class.clone();
                if let Some(m) = cls.find_method("__tostring") {
                    let v = self.call_method_h(Some(obj), &cls, &m, &[])?;
                    return match v {
                        Value::Str(s) => Ok(Some(Value::Str(s))),
                        _ => Err(RuntimeError::Error(format!(
                            "{}::__toString(): Return value must be of type string, {} returned",
                            cls.name,
                            v.value_name()
                        ))),
                    };
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn compare_objects(&mut self, op1: &Value, op2: &Value) -> RtResult<i32> {
        if let (Value::Object(a), Value::Object(b)) = (op1.deref(), op2.deref()) {
            if Obj::ptr_eq(&a, &b) {
                return Ok(0);
            }
            if let Some(r) = php_spl::handlers::compare(self, &a, &b)? {
                return Ok(r);
            }
            if !Rc::ptr_eq(&tobj(&a).class, &tobj(&b).class) {
                return Ok(1);
            }
            let pa = tobj(&a).props.borrow().clone();
            let pb = tobj(&b).props.borrow().clone();
            return ops::compare_tables(&pa, &pb, self);
        }
        ops::std_compare_objects(self, op1, op2, |_, _, _| Ok(1))
    }

    fn object_properties(&mut self, obj: &Obj, purpose: PropertyPurpose) -> RtResult<Option<ZArray>> {
        if let Some(t) = php_spl::handlers::properties_for(self, obj, purpose)? {
            return Ok(Some(t));
        }
        Ok(Some(tobj(obj).props.borrow().clone()))
    }

    fn object_dump(&mut self, obj: &Obj, purpose: PropertyPurpose) -> RtResult<ObjectDump> {
        let cls = tobj(obj).class.clone();
        let props = if purpose == PropertyPurpose::Debug {
            if let Some(m) = cls.find_method("__debuginfo") {
                match self.call_method_h(Some(obj), &cls, &m, &[])? {
                    Value::Array(a) => Some(a),
                    Value::Null => Some(ZArray::new()),
                    _ => {
                        return Err(RuntimeError::Fatal("__debuginfo() must return an array".into()));
                    }
                }
            } else if let Some(d) = php_spl::handlers::debug_info(self, obj)? {
                Some(d)
            } else {
                self.object_properties(obj, purpose)?
            }
        } else {
            self.object_properties(obj, purpose)?
        };
        let kind = if cls.name.eq_ignore_ascii_case("stdClass") {
            ObjectKind::StdClass
        } else {
            ObjectKind::Regular { lazy_prefix: "" }
        };
        Ok(ObjectDump {
            kind,
            properties: props.map(|t| t.iter().map(|(k, v)| (k.clone(), PropertySlot::Value(v.clone()))).collect()),
        })
    }

    fn serialize_object(&mut self, obj: &Obj) -> RtResult<SerializedObject> {
        let cls = tobj(obj).class.clone();
        if let Some(m) = cls.find_method("__serialize") {
            let v = self.call_method_h(Some(obj), &cls, &m, &[])?;
            let Value::Array(a) = v else {
                return Err(RuntimeError::TypeError(format!(
                    "{}::__serialize() must return an array",
                    cls.name
                )));
            };
            return Ok(SerializedObject::Object { class: obj.class_name(), props: a });
        }
        if cls.is_a("Serializable") {
            if let Some(m) = cls.find_method("serialize") {
                let v = self.call_method_h(Some(obj), &cls, &m, &[])?;
                return Ok(match v {
                    Value::Str(s) => SerializedObject::Custom { class: obj.class_name(), data: s.as_bytes().to_vec() },
                    _ => SerializedObject::Null,
                });
            }
        }
        if let Some(m) = cls.find_method("__sleep") {
            let v = self.call_method_h(Some(obj), &cls, &m, &[])?;
            let mut out = HashTable::new();
            if let Value::Array(names) = v {
                for n in names.values() {
                    let n = n.to_zstr(self)?.to_string_lossy();
                    let key = self.find_prop_key(obj, &n).unwrap_or_else(|| ZStr::from(n.as_str()));
                    let val = tobj(obj).props.borrow().get_str(key.as_bytes()).cloned().unwrap_or_default();
                    out.insert_str_raw(key, val);
                }
            }
            return Ok(SerializedObject::Object { class: obj.class_name(), props: ZArray::from_table(out) });
        }
        let props = tobj(obj).props.borrow().clone();
        Ok(SerializedObject::Object { class: obj.class_name(), props })
    }

    fn unserialize_object_create(&mut self, class: &ZStr) -> RtResult<Value> {
        let cls = match self.find_class(class.as_bytes()) {
            Some(c) => c,
            None => self.find_class(b"__PHP_Incomplete_Class").expect("incomplete class"),
        };
        let o = self.new_object_raw(&cls)?;
        Ok(Value::Object(o))
    }

    fn unserialize_object_finish(&mut self, obj: &Value, props: ZArray) -> RtResult<()> {
        let Value::Object(o) = obj else { return Ok(()) };
        let cls = tobj(o).class.clone();
        if let Some(m) = cls.find_method("__unserialize") {
            self.call_method_h(Some(o), &cls, &m, &[Value::Array(ZArray::from_table(props.to_symtable()))])?;
            return Ok(());
        }
        for (k, v) in props.iter() {
            let key = k.to_zstr();
            tobj(o).props.borrow_mut().make_mut().insert_str_raw(key, v.clone());
        }
        if let Some(m) = cls.find_method("__wakeup") {
            self.call_method_h(Some(o), &cls, &m, &[])?;
        }
        Ok(())
    }

    fn unserialize_custom(&mut self, class: &ZStr, data: &[u8]) -> RtResult<Value> {
        let cls = self.lookup_class(class.as_bytes())?;
        let o = self.new_object_raw(&cls)?;
        self.call_method(&o, "unserialize", &[Value::Str(ZStr::from(data))])?;
        Ok(Value::Object(o))
    }

    fn count_object(&mut self, obj: &Obj) -> RtResult<Option<i64>> {
        ArrayContext::count_object(self, obj)
    }

    fn property_read(&mut self, obj: &Obj, name: &ZStr) -> RtResult<Option<Value>> {
        let t = tobj(obj);
        Ok(t.props.borrow().get_str(name.as_bytes()).cloned())
    }

    fn property_write(&mut self, obj: &Obj, name: &ZStr, value: Value) -> RtResult<()> {
        let exists = tobj(obj).props.borrow().get_str(name.as_bytes()).is_some();
        if !exists && name.as_bytes().first() != Some(&0) {
            let cls = tobj(obj).class.clone();
            let n = name.to_string_lossy();
            let declared = cls.all_props().iter().any(|(k, i)| k.props[*i].name == n);
            if !declared && !cls.allow_dynamic && !cls.name.eq_ignore_ascii_case("stdClass") {
                self.emit(
                    ErrorLevel::Deprecated,
                    &format!("Creation of dynamic property {}::${n} is deprecated", cls.name),
                )?;
            }
        }
        tobj(obj).props.borrow_mut().make_mut().insert_str_raw(name.clone(), value);
        Ok(())
    }

    fn create_std_object(&mut self, props: ZArray) -> RtResult<Value> {
        let cls = self.find_class(b"stdClass").expect("stdClass");
        let o = self.new_object_raw(&cls)?;
        *tobj(&o).props.borrow_mut() = props;
        Ok(Value::Object(o))
    }
}

impl ArrayContext for Interp {
    fn count_object(&mut self, obj: &Obj) -> RtResult<Option<i64>> {
        if let Some(n) = php_spl::handlers::count_elements(self, obj)? {
            return Ok(Some(n));
        }
        if tobj(obj).class.is_a("Countable") {
            let v = self.call_method(obj, "count", &[])?;
            return v.to_long(self).map(Some);
        }
        Ok(None)
    }
}

impl SplHost for Interp {
    fn spl_state(&mut self, obj: &Obj) -> Option<Rc<SplObject>> {
        obj.downcast_ref::<TObj>().and_then(|t| t.spl.borrow().clone())
    }

    fn call_method(&mut self, obj: &Obj, name: &str, args: &[Value]) -> RtResult<Value> {
        Interp::call_method(self, obj, name, args)
    }

    fn has_user_method(&mut self, obj: &Obj, name: &str) -> bool {
        let cls = tobj(obj).class.clone();
        cls.find_method(&name.to_ascii_lowercase())
            .is_some_and(|m| matches!(m.kind, MKind::User(_) | MKind::Abstract))
    }

    fn has_method(&mut self, obj: &Obj, name: &str) -> bool {
        tobj(obj).class.find_method(&name.to_ascii_lowercase()).is_some()
    }

    fn instance_of(&mut self, obj: &Obj, class: &str) -> bool {
        tobj(obj).class.is_a(class)
    }

    fn create_object(&mut self, class: &ZStr) -> RtResult<Obj> {
        let cls = self.lookup_class(class.as_bytes())?;
        self.new_object_raw(&cls)
    }

    fn instantiate(&mut self, class: &ZStr, args: &[Value]) -> RtResult<Obj> {
        let cls = self.lookup_class(class.as_bytes())?;
        Interp::instantiate(self, &cls, args)
    }

    fn class_is_a(&mut self, class: &ZStr, parent: &str) -> Option<bool> {
        self.find_class(class.as_bytes()).map(|c| c.is_a(parent))
    }

    fn call_callable(&mut self, callable: &Value, args: &[Value]) -> RtResult<Value> {
        self.call_value(callable, args)
    }

    fn check_callable(&mut self, callable: &Value) -> Result<(), String> {
        self.is_callable_value(callable)
    }

    fn foreign_iterator(&mut self, obj: &Obj) -> Option<RtResult<Box<dyn ObjectIterator>>> {
        let t = tobj(obj);
        if t.class.name == "InternalIterator" {
            return Some(Ok(Box::new(InternalIt(obj.clone()))));
        }
        None
    }

    fn std_properties(&mut self, obj: &Obj) -> ZArray {
        tobj(obj).props.borrow().clone()
    }

    fn std_has_property(&mut self, obj: &Obj, name: &ZStr) -> bool {
        match self.find_prop_key(obj, &name.to_string_lossy()) {
            Some(k) => self.prop_visible(obj, &k),
            None => false,
        }
    }

    fn set_std_properties(&mut self, obj: &Obj, props: ZArray) -> RtResult<()> {
        *tobj(obj).props.borrow_mut() = props;
        Ok(())
    }

    fn class_info(&mut self, name: &ZStr, autoload: bool) -> Option<ClassInfo> {
        let cls = if autoload {
            self.lookup_class(name.as_bytes()).ok()?
        } else {
            self.find_class(name.as_bytes())?
        };
        Some(class_info(&cls))
    }

    fn object_class_info(&mut self, obj: &Obj) -> ClassInfo {
        class_info(&tobj(obj).class)
    }

    fn new_internal_iterator(&mut self, it: Box<dyn ObjectIterator>) -> RtResult<Value> {
        let cls = self.find_class(b"InternalIterator").expect("InternalIterator");
        let o = self.new_object_raw(&cls)?;
        *tobj(&o).extra.borrow_mut() = Extra::Iter(it);
        Ok(Value::Object(o))
    }

    fn autoload_include(&mut self, file: &ZStr) -> RtResult<bool> {
        let path = format!("{}/{}", self.dir, file.to_string_lossy());
        if std::path::Path::new(&path).exists() {
            let src = std::fs::read(&path).unwrap_or_default();
            self.run_include(&src)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn pcre(&mut self) -> &mut php_pcre::Pcre {
        &mut self.pcre
    }

    fn write_output(&mut self, bytes: &[u8]) -> RtResult<()> {
        self.echo(bytes);
        Ok(())
    }
}

fn class_info(cls: &Rc<Class>) -> ClassInfo {
    let mut parents = Vec::new();
    let mut p = cls.parent.clone();
    while let Some(k) = p {
        parents.push(ZStr::from(k.name.as_str()));
        p = k.parent.clone();
    }
    let mut interfaces: Vec<ZStr> = Vec::new();
    let mut c = Some(cls.clone());
    while let Some(k) = c {
        for i in &k.interfaces {
            if !interfaces.iter().any(|x| x.eq_ignore_ascii_case(i.as_bytes())) {
                interfaces.push(ZStr::from(i.as_str()));
            }
        }
        c = k.parent.clone();
    }
    ClassInfo {
        name: ZStr::from(cls.name.as_str()),
        parents,
        interfaces,
        traits: Vec::new(),
    }
}

/// Iterates an `InternalIterator` object through its stored iterator.
struct InternalIt(Obj);

impl InternalIt {
    fn with<R>(&self, host: &mut dyn SplHost, f: impl FnOnce(&mut dyn ObjectIterator, &mut dyn SplHost) -> RtResult<R>) -> RtResult<R> {
        let t = tobj(&self.0);
        let mut extra = std::mem::replace(&mut *t.extra.borrow_mut(), Extra::None);
        let r = match &mut extra {
            Extra::Iter(it) => f(it.as_mut(), host),
            _ => Err(RuntimeError::Error("bad iterator".into())),
        };
        *t.extra.borrow_mut() = extra;
        r
    }
}

impl ObjectIterator for InternalIt {
    fn rewind(&mut self, host: &mut dyn SplHost) -> RtResult<()> {
        self.with(host, |it, h| it.rewind(h))
    }
    fn valid(&mut self, host: &mut dyn SplHost) -> RtResult<bool> {
        self.with(host, |it, h| it.valid(h))
    }
    fn current(&mut self, host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        self.with(host, |it, h| it.current(h))
    }
    fn key(&mut self, host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        self.with(host, |it, h| it.key(h))
    }
    fn next(&mut self, host: &mut dyn SplHost) -> RtResult<()> {
        self.with(host, |it, h| it.next(h))
    }
}

pub fn internal_iter_call(it: &mut Interp, this: &Obj, m: &str) -> RtResult<Value> {
    let mut ii = InternalIt(this.clone());
    match m {
        "rewind" => ii.rewind(it).map(|()| Value::Null),
        "valid" => ii.valid(it).map(Value::Bool),
        "current" => ii.current(it).map(Option::unwrap_or_default),
        "key" => ii.key(it).map(Option::unwrap_or_default),
        _ => ii.next(it).map(|()| Value::Null),
    }
}

impl Interp {
    pub fn run_include(&mut self, src: &[u8]) -> RtResult<()> {
        let program = php_parser::parse(src, &php_parser::ParseOptions::default())
            .map_err(|e| RuntimeError::Fatal(e.message.clone()))?;
        let stmts = Rc::new(program.statements);
        let _ = self.exec_block(&stmts)?;
        for st in stmts.iter() {
            if let php_parser::ast::StmtKind::Class(c) = &st.kind {
                self.declare_class(c)?;
            }
        }
        Ok(())
    }
}

pub fn key_of(k: &ArrayKey) -> Value {
    k.to_value()
}
