//! Enums (`Zend/zend_enum.c`), internal object hooks and callable checks.

use crate::class::{Class, ClassConst};
use crate::object;
use crate::types::mask;
use crate::vm::{Vm, VmResult};
use php_runtime::{ArrayKey, Obj, Value, ZArray, ZStr};
use std::cell::RefCell;
use std::rc::Rc;

mod callable;
#[cfg(test)]
mod tests;

thread_local! {
    /// Backed enums whose case table is being built (guards against
    /// re-entrance from case value expressions).
    static BUILDING: RefCell<Vec<*const Class>> = const { RefCell::new(Vec::new()) };
}

impl Vm {
    /// The object for enum case `name`.
    pub(crate) fn enum_case(&mut self, cls: &Rc<Class>, name: &ZStr) -> VmResult<Value> {
        if let Some(v) = cls.enum_cases.borrow().get(name) {
            return Ok(v.clone());
        }
        let Some(c) = cls.find_const(name.as_bytes()).cloned() else {
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Undefined constant {}::{}",
                    cls.name,
                    name.to_string_lossy()
                ),
            ));
        };
        if !c.is_case {
            return Err(self.make_throw(
                "Error",
                &format!("{}::{} is not a case", cls.name, name.to_string_lossy()),
            ));
        }
        if cls.backing.is_some() {
            self.enum_ensure_backed_table(cls)?;
            if let Some(v) = cls.enum_cases.borrow().get(name) {
                return Ok(v.clone());
            }
        }
        let value = if cls.backing.is_some() {
            Some(self.enum_case_backing_value(cls, &c)?)
        } else {
            None
        };
        Ok(Self::enum_create_case(cls, &c, value))
    }

    /// Evaluates and type-checks the backing value of case `c`.
    fn enum_case_backing_value(&mut self, cls: &Rc<Class>, c: &ClassConst) -> VmResult<Value> {
        let v = self.eval_const_expr(&c.expr, Some(cls))?;
        let is_int = cls.backing == Some(mask::LONG);
        let ok = if is_int {
            matches!(v, Value::Int(_))
        } else {
            matches!(v, Value::Str(_))
        };
        if !ok {
            let ty = if is_int { "int" } else { "string" };
            return Err(self.make_throw(
                "TypeError",
                &format!(
                    "Enum case type {} does not match enum backing type {ty}",
                    crate::typecheck::given_type(&v)
                ),
            ));
        }
        Ok(v)
    }

    /// Builds the backed-enum table unless all cases already exist (or it is
    /// being built further up the stack).
    fn enum_ensure_backed_table(&mut self, cls: &Rc<Class>) -> VmResult<()> {
        let total = cls.consts.iter().filter(|c| c.is_case).count();
        let ptr = Rc::as_ptr(cls);
        if cls.enum_cases.borrow().len() >= total || BUILDING.with(|b| b.borrow().contains(&ptr)) {
            return Ok(());
        }
        BUILDING.with(|b| b.borrow_mut().push(ptr));
        let r = self.enum_build_backed_table(cls);
        BUILDING.with(|b| b.borrow_mut().retain(|p| !std::ptr::eq(*p, ptr)));
        r
    }

    /// `zend_enum_build_backed_enum_table`: evaluates every case value,
    /// rejecting mismatched types and duplicates, then creates the cases.
    fn enum_build_backed_table(&mut self, cls: &Rc<Class>) -> VmResult<()> {
        let cases: Vec<Rc<ClassConst>> = cls.consts.iter().filter(|c| c.is_case).cloned().collect();
        let mut values: Vec<Value> = Vec::with_capacity(cases.len());
        for c in &cases {
            let existing = cls.enum_cases.borrow().get(&c.name).cloned();
            let v = match existing {
                Some(Value::Object(o)) => Self::enum_case_value(&o).unwrap_or_default(),
                _ => self.enum_case_backing_value(cls, c)?,
            };
            if let Some(i) = values.iter().position(|x| backing_eq(x, &v)) {
                return Err(self.make_throw(
                    "Error",
                    &format!(
                        "Duplicate value in enum {} for cases {} and {}",
                        cls.name, cases[i].name, c.name
                    ),
                ));
            }
            values.push(v);
        }
        for (c, v) in cases.iter().zip(values) {
            if !cls.enum_cases.borrow().contains_key(&c.name) {
                Self::enum_create_case(cls, c, Some(v));
            }
        }
        Ok(())
    }

    /// Creates (and caches) the object of case `c`.
    fn enum_create_case(cls: &Rc<Class>, c: &ClassConst, value: Option<Value>) -> Value {
        let mut slots = vec![None; cls.props.len()];
        if let Some(pi) = cls.prop_map.get(&ZStr::from("name")) {
            slots[pi.slot as usize] = Some(Value::Str(c.name.clone()));
        }
        if let (Some(v), Some(pi)) = (value, cls.prop_map.get(&ZStr::from("value"))) {
            slots[pi.slot as usize] = Some(v);
        }
        let o = object::new_object(cls.clone(), slots);
        let d = object::data(&o);
        d.flags
            .set(d.flags.get() | object::oflags::ENUM_CASE | object::oflags::DESTRUCTED);
        let v = Value::Object(o);
        cls.enum_cases
            .borrow_mut()
            .insert(c.name.clone(), v.clone());
        *c.value.borrow_mut() = Some(v.clone());
        v
    }

    /// The backing value of an enum case object.
    pub(crate) fn enum_case_value(o: &Obj) -> Option<Value> {
        let cls = object::class_of(o);
        let pi = cls.prop_map.get(&ZStr::from("value"))?;
        object::data(o)
            .slots
            .borrow()
            .get(pi.slot as usize)
            .cloned()
            .flatten()
    }

    /// `zend_verify_enum`: link-time checks of an enum declaration
    /// (properties, forbidden magic methods, `Serializable`). Returns the
    /// compile error message.
    #[allow(clippy::unused_self)] // called as a link-time hook on the VM
    pub(crate) fn verify_enum(&self, cls: &Rc<Class>) -> Option<String> {
        let backed = cls.backing.is_some();
        let has_prop = cls
            .props
            .iter()
            .map(|p| &p.name)
            .chain(cls.static_props.iter().map(|p| &p.name))
            .any(|n| !(n.as_bytes() == b"name" || (backed && n.as_bytes() == b"value")));
        if has_prop {
            return Some(format!("Enum {} cannot include properties", cls.name));
        }
        let m = &cls.magic;
        let magic = [
            (m.construct.is_some(), "__construct"),
            (m.destruct.is_some(), "__destruct"),
            (m.clone.is_some(), "__clone"),
            (m.get.is_some(), "__get"),
            (m.set.is_some(), "__set"),
            (m.unset.is_some(), "__unset"),
            (m.isset.is_some(), "__isset"),
            (m.to_string.is_some(), "__toString"),
            (m.serialize.is_some(), "__serialize"),
            (m.unserialize.is_some(), "__unserialize"),
            (cls.methods.contains_key(&ZStr::from("__sleep")), "__sleep"),
            (
                cls.methods.contains_key(&ZStr::from("__wakeup")),
                "__wakeup",
            ),
            (
                cls.methods.contains_key(&ZStr::from("__set_state")),
                "__set_state",
            ),
        ];
        if let Some((_, name)) = magic.iter().find(|(has, _)| *has) {
            return Some(format!(
                "Enum {} cannot include magic method {name}",
                cls.name
            ));
        }
        if cls
            .interfaces
            .iter()
            .any(|i| i.lc_name.as_bytes() == b"serializable")
        {
            return Some(format!(
                "Enum {} cannot implement the Serializable interface",
                cls.name
            ));
        }
        None
    }

    /// The case name of an enum object.
    #[allow(clippy::unused_self)] // method-style API used across the VM
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
        let names: Vec<ZStr> = cls
            .consts
            .iter()
            .filter(|c| c.is_case)
            .map(|c| c.name.clone())
            .collect();
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
                Value::Str(s) => {
                    let parsed = php_runtime::numeric::is_numeric_string_ex(s.as_bytes(), false)
                        .map(|n| n.value);
                    match parsed {
                        Some(php_runtime::numeric::Number::Int(i)) => Value::Int(i),
                        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
                        Some(php_runtime::numeric::Number::Float(f))
                            if f.is_finite() && f >= i64::MIN as f64 && f < i64::MAX as f64 =>
                        {
                            if f.fract() != 0.0 {
                                self.deprecated(&format!(
                                    "Implicit conversion from float-string \"{}\" to int loses precision",
                                    s.to_string_lossy()
                                ))?;
                            }
                            Value::Int(f as i64)
                        }
                        _ => {
                            return Err(self.make_throw(
                                "TypeError",
                                &format!(
                                "{}::{}(): Argument #1 ($value) must be of type int, string given",
                                cls.name,
                                if try_ { "tryFrom" } else { "from" }
                            ),
                            ))
                        }
                    }
                }
                other => other,
            }
        } else {
            match v {
                Value::Str(_) => v,
                other => Value::Str(self.to_zstr(&other)?),
            }
        };
        self.enum_ensure_backed_table(cls)?;
        let names: Vec<ZStr> = cls
            .consts
            .iter()
            .filter(|c| c.is_case)
            .map(|c| c.name.clone())
            .collect();
        for n in names {
            let case = self.enum_case(cls, &n)?;
            if let Value::Object(o) = &case {
                if Self::enum_case_value(o).is_some_and(|cv| backing_eq(&cv, &v)) {
                    return Ok(case);
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
    #[allow(clippy::unnecessary_wraps)] // fallible hook signature used by the object handlers
    pub(crate) fn internal_object_array(&mut self, o: &Obj) -> VmResult<Option<ZArray>> {
        if self.closure_data(o).is_some() {
            return Ok(crate::closure::closure_debug_array(self, o));
        }
        Ok(None)
    }

    /// Whether an object has a custom property table.
    #[allow(clippy::unused_self)] // method-style API used by the object handlers
    pub(crate) fn has_internal_array(&self, o: &Obj) -> bool {
        object::has_internal::<crate::closure::ClosureData>(o)
    }

    /// Comparison of internal objects (`None`: default). Closures compare
    /// equal only when both are fake closures of the same function, scope,
    /// called scope and `$this` (`zend_closure_compare`).
    #[allow(clippy::unnecessary_wraps)] // fallible hook signature used by the object handlers
    pub(crate) fn internal_compare(&mut self, a: &Obj, b: &Obj) -> VmResult<Option<i32>> {
        let (Some(lhs), Some(rhs)) = (self.closure_data(a), self.closure_data(b)) else {
            return Ok(None);
        };
        let same_class = |l: Option<&Rc<Class>>, r: Option<&Rc<Class>>| match (l, r) {
            (Some(l), Some(r)) => Rc::ptr_eq(l, r),
            (None, None) => true,
            _ => false,
        };
        let same = lhs.fake
            && rhs.fake
            && match (&lhs.this, &rhs.this) {
                (Some(l), Some(r)) => Obj::ptr_eq(l, r),
                (None, None) => true,
                _ => false,
            }
            && same_class(lhs.called.as_ref(), rhs.called.as_ref())
            && same_class(lhs.scope.as_ref(), rhs.scope.as_ref())
            && matches!(
                (&lhs.func, &rhs.func),
                (crate::func::Func::User(_), crate::func::Func::User(_))
                    | (crate::func::Func::Builtin(_), crate::func::Func::Builtin(_))
            )
            && crate::closure::closure_callable_name(&lhs)
                == crate::closure::closure_callable_name(&rhs);
        Ok(Some(i32::from(!same)))
    }

    /// Copies internal state when cloning.
    #[allow(clippy::unnecessary_wraps)] // fallible hook signature used by the object handlers
    pub(crate) fn clone_internal(&mut self, old: &Obj, new: &Obj) -> VmResult<()> {
        if let Some(d) = self.closure_data(old) {
            object::set_internal(new, d);
        }
        Ok(())
    }

    /// `zend_is_callable` (deprecations suppressed, exceptions discarded).
    pub(crate) fn is_callable_value(&mut self, v: &Value) -> bool {
        matches!(self.zend_is_callable(v, true), Ok(Ok(_)))
    }

    /// The callable name (`zend_get_callable_name_ex`).
    pub(crate) fn callable_name(&mut self, v: &Value) -> String {
        match v {
            Value::Str(s) => s.to_string_lossy(),
            Value::Array(arr) => {
                let target = arr.get(&ArrayKey::Int(0)).map(Value::deref);
                let method = arr.get(&ArrayKey::Int(1)).map(Value::deref);
                match (arr.len(), target, method) {
                    (2, Some(Value::Object(o)), Some(Value::Str(m))) => {
                        format!("{}::{}", object::class_of(&o).name, m.to_string_lossy())
                    }
                    (2, Some(Value::Str(c)), Some(Value::Str(m))) => {
                        format!("{}::{}", c.to_string_lossy(), m.to_string_lossy())
                    }
                    _ => "Array".to_string(),
                }
            }
            Value::Object(o) => match object::internal::<crate::closure::ClosureData, _>(o, |d| {
                crate::closure::closure_callable_name(d)
            }) {
                Some(n) => n,
                None => format!("{}::__invoke", object::class_of(o).name),
            },
            Value::Reference(r) => {
                let inner = r.get();
                self.callable_name(&inner)
            }
            other => match other.to_zstr(self) {
                Ok(s) => s.to_string_lossy(),
                Err(_) => String::new(),
            },
        }
    }

    /// The reason a value is not callable (for "must be a valid callback").
    pub(crate) fn callable_error(&mut self, v: &Value) -> String {
        match self.zend_is_callable(v, true) {
            Ok(Err(e)) => e,
            _ => "no array or string given".to_string(),
        }
    }
}

/// Equality of two backing values (both `int` or both `string`).
fn backing_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x.as_bytes() == y.as_bytes(),
        _ => false,
    }
}
