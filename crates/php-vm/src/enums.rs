//! Enums (`Zend/zend_enum.c`), internal object hooks and callable checks.

use crate::class::Class;
use crate::object;
use crate::types::mask;
use crate::vm::{Vm, VmResult};
use php_runtime::{ArrayKey, Obj, Value, ZArray, ZStr};
use std::rc::Rc;

impl Vm {
    /// The object for enum case `name`.
    pub(crate) fn enum_case(&mut self, cls: &Rc<Class>, name: &ZStr) -> VmResult<Value> {
        if let Some(v) = cls.enum_cases.borrow().get(name) {
            return Ok(v.clone());
        }
        let Some(c) = cls.find_const(name.as_bytes()).cloned() else {
            return Err(self.make_throw(
                "Error",
                &format!("Undefined constant {}::{}", cls.name, name.to_string_lossy()),
            ));
        };
        if !c.is_case {
            return Err(self.make_throw(
                "Error",
                &format!("{}::{} is not a case", cls.name, name.to_string_lossy()),
            ));
        }
        let mut slots = vec![None; cls.props.len()];
        if let Some(pi) = cls.prop_map.get(&ZStr::from("name")) {
            slots[pi.slot as usize] = Some(Value::Str(name.clone()));
        }
        if cls.backing.is_some() {
            let v = self.eval_const_expr(&c.expr, Some(cls))?;
            let ok = match cls.backing {
                Some(m) if m == mask::LONG => matches!(v, Value::Int(_)),
                _ => matches!(v, Value::Str(_)),
            };
            if !ok {
                let ty = if cls.backing == Some(mask::LONG) { "int" } else { "string" };
                return Err(self.fatal(&format!(
                    "Enum case type {} does not match enum backing type {ty}",
                    crate::typecheck::given_type(&v)
                )));
            }
            if let Some(pi) = cls.prop_map.get(&ZStr::from("value")) {
                slots[pi.slot as usize] = Some(v);
            }
        }
        let o = object::new_object(cls.clone(), slots);
        let d = object::data(&o);
        d.flags.set(d.flags.get() | object::oflags::ENUM_CASE | object::oflags::DESTRUCTED);
        let v = Value::Object(o);
        cls.enum_cases.borrow_mut().insert(name.clone(), v.clone());
        *c.value.borrow_mut() = Some(v.clone());
        Ok(v)
    }

    /// The case name of an enum object.
    pub(crate) fn enum_case_name(&self, o: &Obj) -> Option<ZStr> {
        let cls = object::class_of(o);
        let pi = cls.prop_map.get(&ZStr::from("name"))?;
        match object::data(o).slots.borrow().get(pi.slot as usize) {
            Some(Some(Value::Str(s))) => Some(s.clone()),
            _ => None,
        }
    }

    /// `Enum::cases()`.
    pub(crate) fn enum_cases_array(&mut self, cls: &Rc<Class>) -> VmResult<Value> {
        let names: Vec<ZStr> = cls.consts.iter().filter(|c| c.is_case).map(|c| c.name.clone()).collect();
        let mut out = Vec::new();
        for n in names {
            out.push(self.enum_case(cls, &n)?);
        }
        Ok(Value::Array(ZArray::from_values(out)))
    }

    /// `Enum::from()` / `Enum::tryFrom()`.
    pub(crate) fn enum_from(&mut self, cls: &Rc<Class>, v: Value, try_: bool) -> VmResult<Value> {
        let is_int = cls.backing == Some(mask::LONG);
        let v = if is_int {
            match v {
                Value::Int(_) => v,
                Value::Str(s) => match php_runtime::numeric::is_numeric_string_ex(s.as_bytes(), false) {
                    Some(n) => match n.value {
                        php_runtime::numeric::Number::Int(i) => Value::Int(i),
                        php_runtime::numeric::Number::Float(_) => {
                            return Err(self.make_throw(
                                "TypeError",
                                &format!(
                                    "{}::{}(): Argument #1 ($value) must be of type int, string given",
                                    cls.name,
                                    if try_ { "tryFrom" } else { "from" }
                                ),
                            ))
                        }
                    },
                    None => {
                        return Err(self.make_throw(
                            "TypeError",
                            &format!(
                                "{}::{}(): Argument #1 ($value) must be of type int, string given",
                                cls.name,
                                if try_ { "tryFrom" } else { "from" }
                            ),
                        ))
                    }
                },
                other => other,
            }
        } else {
            match v {
                Value::Str(_) => v,
                other => Value::Str(self.to_zstr(&other)?),
            }
        };
        let names: Vec<ZStr> = cls.consts.iter().filter(|c| c.is_case).map(|c| c.name.clone()).collect();
        for n in names {
            let case = self.enum_case(cls, &n)?;
            if let Value::Object(o) = &case {
                let pi = cls.prop_map.get(&ZStr::from("value")).cloned();
                if let Some(pi) = pi {
                    let cv = object::data(o).slots.borrow()[pi.slot as usize].clone().unwrap_or_default();
                    if php_runtime::ops::is_identical(&cv, &v).unwrap_or(false) {
                        return Ok(case);
                    }
                }
            }
        }
        if try_ {
            return Ok(Value::Null);
        }
        let desc = match &v {
            Value::Int(i) => i.to_string(),
            Value::Str(s) => format!("\"{}\"", s.to_string_lossy()),
            other => format!("{other:?}"),
        };
        Err(self.make_throw(
            "ValueError",
            &format!("{desc} is not a valid backing value for enum {}", cls.name),
        ))
    }

    /// Custom property table of internal objects (closures).
    pub(crate) fn internal_object_array(&mut self, o: &Obj) -> VmResult<Option<ZArray>> {
        if self.closure_data(o).is_some() {
            return Ok(crate::closure::closure_debug_array(self, o));
        }
        Ok(None)
    }

    /// Whether an object has a custom property table.
    pub(crate) fn has_internal_array(&self, o: &Obj) -> bool {
        self.closure_data(o).is_some()
    }

    /// Comparison of internal objects (`None`: default).
    pub(crate) fn internal_compare(&mut self, a: &Obj, b: &Obj) -> VmResult<Option<i32>> {
        if let (Some(x), Some(y)) = (self.closure_data(a), self.closure_data(b)) {
            let same = crate::func::Func::ptr_eq(&x.func, &y.func)
                && match (&x.this, &y.this) {
                    (Some(p), Some(q)) => Obj::ptr_eq(p, q),
                    (None, None) => true,
                    _ => false,
                };
            return Ok(Some(i32::from(!same)));
        }
        Ok(None)
    }

    /// Copies internal state when cloning.
    pub(crate) fn clone_internal(&mut self, old: &Obj, new: &Obj) -> VmResult<()> {
        if let Some(d) = self.closure_data(old) {
            object::set_internal(new, d);
        }
        Ok(())
    }

    /// `zend_is_callable`.
    pub(crate) fn is_callable_value(&mut self, v: &Value) -> bool {
        match v {
            Value::Str(s) => {
                if let Some(p) = crate::call::find_double_colon(s.as_bytes()) {
                    let cn = ZStr::from(&s.as_bytes()[..p]);
                    let m = &s.as_bytes()[p + 2..];
                    return match self.lookup_class(&cn) {
                        Ok(Some(c)) => c.find_method(m).is_some_and(|mm| {
                            let scope = self.scope();
                            self.can_access_method(mm, scope.as_ref()) && mm.is_static()
                        }) || c.magic.call_static.is_some(),
                        _ => false,
                    };
                }
                let n = s.as_bytes().strip_prefix(b"\\").unwrap_or(s.as_bytes());
                self.functions.contains_key(n.to_ascii_lowercase().as_slice())
            }
            Value::Array(a) => {
                if a.len() != 2 {
                    return false;
                }
                let (Some(t), Some(m)) = (a.get(&ArrayKey::Int(0)), a.get(&ArrayKey::Int(1))) else {
                    return false;
                };
                let Value::Str(m) = m.deref() else { return false };
                let cls = match t.deref() {
                    Value::Object(o) => object::class_of(&o).clone(),
                    Value::Str(cn) => match self.lookup_class(&cn) {
                        Ok(Some(c)) => c,
                        _ => return false,
                    },
                    _ => return false,
                };
                let mname = match crate::call::find_double_colon(m.as_bytes()) {
                    Some(p) => &m.as_bytes()[p + 2..],
                    None => m.as_bytes(),
                };
                if let Some(mm) = cls.find_method(mname).cloned() {
                    let scope = self.scope();
                    if self.can_access_method(&mm, scope.as_ref()) {
                        return true;
                    }
                }
                let is_obj = matches!(t.deref(), Value::Object(_));
                (is_obj && cls.magic.call.is_some()) || cls.magic.call_static.is_some()
            }
            Value::Object(o) => self.closure_data(o).is_some() || object::class_of(o).magic.invoke.is_some(),
            Value::Reference(r) => {
                let inner = r.get();
                self.is_callable_value(&inner)
            }
            _ => false,
        }
    }

    /// The callable name (`is_callable($v, false, $name)`).
    pub(crate) fn callable_name(&mut self, v: &Value) -> String {
        match v {
            Value::Str(s) => s.to_string_lossy(),
            Value::Array(a) => {
                let t = a.get(&ArrayKey::Int(0)).map(Value::deref);
                let m = a.get(&ArrayKey::Int(1)).map(Value::deref);
                match (t, m) {
                    (Some(Value::Object(o)), Some(Value::Str(m))) => {
                        format!("{}::{}", object::class_of(&o).name, m.to_string_lossy())
                    }
                    (Some(Value::Str(c)), Some(Value::Str(m))) => {
                        format!("{}::{}", c.to_string_lossy(), m.to_string_lossy())
                    }
                    _ => "Array".to_string(),
                }
            }
            Value::Object(o) => {
                if self.closure_data(o).is_some() {
                    "Closure::__invoke".to_string()
                } else {
                    format!("{}::__invoke", object::class_of(o).name)
                }
            }
            other => match other.to_zstr(self) {
                Ok(s) => s.to_string_lossy(),
                Err(_) => String::new(),
            },
        }
    }

    /// The reason a value is not callable (for "must be a valid callback").
    pub(crate) fn callable_error(&mut self, v: &Value) -> String {
        match v {
            Value::Str(s) => {
                if let Some(p) = crate::call::find_double_colon(s.as_bytes()) {
                    let cn = String::from_utf8_lossy(&s.as_bytes()[..p]).into_owned();
                    let m = String::from_utf8_lossy(&s.as_bytes()[p + 2..]).into_owned();
                    return match self.lookup_class(&ZStr::from(cn.as_str())) {
                        Ok(Some(c)) => format!("class {} does not have a method \"{m}\"", c.name),
                        _ => format!("class \"{cn}\" not found"),
                    };
                }
                format!("function \"{}\" not found or invalid function name", s.to_string_lossy())
            }
            Value::Array(a) => {
                if a.len() != 2 {
                    return "array callback must have exactly two members".to_string();
                }
                let t = a.get(&ArrayKey::Int(0)).map(Value::deref);
                let m = a.get(&ArrayKey::Int(1)).map(Value::deref);
                match (t, m) {
                    (Some(Value::Object(o)), Some(Value::Str(m))) => {
                        let c = object::class_of(&o).clone();
                        match c.find_method(m.as_bytes()) {
                            Some(mm) => format!(
                                "cannot access {} method {}::{}()",
                                if mm.visibility() == crate::func::flags::PRIVATE { "private" } else { "protected" },
                                c.name,
                                mm.name
                            ),
                            None => format!("class {} does not have a method \"{}\"", c.name, m.to_string_lossy()),
                        }
                    }
                    (Some(Value::Str(cn)), Some(Value::Str(m))) => match self.lookup_class(&cn) {
                        Ok(Some(c)) => match c.find_method(m.as_bytes()) {
                            Some(mm) if !mm.is_static() => format!(
                                "non-static method {}::{}() cannot be called statically",
                                c.name, mm.name
                            ),
                            Some(mm) => format!(
                                "cannot access {} method {}::{}()",
                                if mm.visibility() == crate::func::flags::PRIVATE { "private" } else { "protected" },
                                c.name,
                                mm.name
                            ),
                            None => format!("class {} does not have a method \"{}\"", c.name, m.to_string_lossy()),
                        },
                        _ => format!("class \"{}\" not found", cn.to_string_lossy()),
                    },
                    _ => "first array member is not a valid class name or object".to_string(),
                }
            }
            Value::Object(_) => "no array or string given".to_string(),
            _ => "no array or string given".to_string(),
        }
    }
}
