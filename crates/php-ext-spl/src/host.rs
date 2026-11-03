//! [`VmHost`]: the [`SplHost`] implementation over the VM, plus the
//! conversions between VM throws and runtime errors.

use php_runtime::serialize::SerializedObject;
use php_runtime::{
    ArrayKey, BinaryOp, CastTarget, ErrorLevel, HashTable, Obj, ObjectDump, PropertyPurpose,
    RtResult, RuntimeContext, RuntimeError, Value, ZArray, ZStr,
};
use php_spl::{ClassInfo, ObjectIterator, SplHost, SplObject};
use php_stdarr::ArrayContext;
use php_vm::class::Class;
use php_vm::func::{flags, Func};
use php_vm::object::{class_of, data};
use php_vm::{Throw, Vm};
use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    /// The `RegexIterator` PCRE state (pattern cache, last error). The
    /// `php-ext-pcre` state is private to that crate, so SPL keeps its own.
    static PCRE: RefCell<Option<php_pcre::Pcre>> = const { RefCell::new(None) };
}

/// Converts a VM throw into a runtime error. Bailouts (`exit`, fatal
/// errors) are parked in [`Vm::pending_throw`] and come back out of
/// [`rt_throw`].
pub fn to_rt(vm: &mut Vm, t: Throw) -> RuntimeError {
    match t {
        Throw::Exception(o) => RuntimeError::Thrown(Value::Object(o)),
        other => {
            if vm.pending_throw.is_none() {
                vm.pending_throw = Some(other);
            }
            RuntimeError::Fatal(String::new())
        }
    }
}

/// Converts a runtime error into a VM throw, creating the exception object
/// for errors raised by `php-spl` (`LogicException`, `TypeError`, ...).
pub fn rt_throw(vm: &mut Vm, e: RuntimeError) -> Throw {
    if let Some(t) = vm.pending_throw.take() {
        return t;
    }
    let class = e.class_name().unwrap_or("Error");
    match e {
        RuntimeError::Thrown(Value::Object(o)) => Throw::Exception(o),
        RuntimeError::Thrown(_) => Throw::Fatal,
        RuntimeError::Fatal(m) => vm.fatal(&m),
        RuntimeError::Custom { class, message } => vm.throw(&class.to_string_lossy(), &message),
        RuntimeError::Error(m)
        | RuntimeError::TypeError(m)
        | RuntimeError::ValueError(m)
        | RuntimeError::ArithmeticError(m)
        | RuntimeError::DivisionByZeroError(m)
        | RuntimeError::ArgumentCountError(m) => vm.throw(class, &m),
    }
}

/// The SPL state attached to an object by the `create` hook.
#[must_use]
pub fn spl_state_of(obj: &Obj) -> Option<Rc<SplObject>> {
    php_vm::object::internal::<Rc<SplObject>, _>(obj, |s| s.clone())
}

/// Whether the method `lc` of `cls` is implemented in user code.
fn is_user_method(cls: &Rc<Class>, name: &str) -> bool {
    let lc = name.to_ascii_lowercase();
    cls.find_method(lc.as_bytes())
        .is_some_and(|m| matches!(m.func, Func::User(_)))
}

/// `"\0Class\0name"` / `"\0*\0name"` / `"name"`.
fn mangle(flags_: u32, class: &ZStr, name: &ZStr) -> ZStr {
    let mut v = Vec::new();
    if flags_ & flags::PRIVATE != 0 {
        v.push(0);
        v.extend_from_slice(class.as_bytes());
        v.push(0);
    } else if flags_ & flags::PROTECTED != 0 {
        v.extend_from_slice(b"\0*\0");
    } else {
        return name.clone();
    }
    v.extend_from_slice(name.as_bytes());
    ZStr::from(v)
}

/// Splits a mangled property name into (class or `*`, name).
fn demangle(key: &[u8]) -> (Option<&[u8]>, &[u8]) {
    if key.first() == Some(&0) {
        if let Some(end) = key[1..].iter().position(|&b| b == 0) {
            return (Some(&key[1..=end]), &key[end + 2..]);
        }
    }
    (None, key)
}

/// `zend_std_get_properties`: declared (initialized) properties in slot
/// order with mangled names, then dynamic ones.
#[must_use]
pub fn std_properties(obj: &Obj) -> ZArray {
    let cls = class_of(obj).clone();
    let d = data(obj);
    let mut t = HashTable::new();
    {
        let slots = d.slots.borrow();
        for pi in &cls.props {
            if pi.flags & flags::STATIC != 0 || pi.is_virtual() {
                continue;
            }
            if let Some(Some(v)) = slots.get(pi.slot as usize) {
                t.insert(
                    ArrayKey::Str(mangle(pi.flags, &pi.class, &pi.name)),
                    v.clone(),
                );
            }
        }
    }
    if let Some(dynp) = d.dynamic.borrow().as_ref() {
        for (k, v) in dynp.iter() {
            t.insert(k.clone(), v.clone());
        }
    }
    ZArray::from_table(t)
}

/// Replaces an object's property table (inverse of [`std_properties`]).
pub fn set_std_properties(obj: &Obj, props: &ZArray) {
    let cls = class_of(obj).clone();
    let d = data(obj);
    let mut slots = d.slots.borrow_mut();
    let mut seen = vec![false; slots.len()];
    let mut dynamic = HashTable::new();
    for (k, v) in props.iter() {
        let declared = match k {
            ArrayKey::Str(s) => {
                let (scope, name) = demangle(s.as_bytes());
                cls.props.iter().find(|pi| {
                    pi.flags & flags::STATIC == 0
                        && pi.name.as_bytes() == name
                        && match scope {
                            None => pi.flags & (flags::PRIVATE | flags::PROTECTED) == 0,
                            Some(b"*") => pi.flags & flags::PROTECTED != 0,
                            Some(c) => {
                                pi.flags & flags::PRIVATE != 0
                                    && pi.class.as_bytes().eq_ignore_ascii_case(c)
                            }
                        }
                })
            }
            ArrayKey::Int(_) => None,
        };
        match declared {
            Some(pi) => {
                let i = pi.slot as usize;
                if i < slots.len() {
                    slots[i] = Some(v.clone());
                    seen[i] = true;
                }
            }
            None => {
                dynamic.insert(k.clone(), v.clone());
            }
        }
    }
    for (i, s) in seen.iter().enumerate() {
        if !s {
            slots[i] = None;
        }
    }
    drop(slots);
    let had_dynamic = d.dynamic.borrow().is_some();
    if had_dynamic || !dynamic.is_empty() {
        *d.dynamic.borrow_mut() = Some(dynamic);
    }
}

fn class_info_of(cls: &Rc<Class>) -> ClassInfo {
    let mut parents = Vec::new();
    let mut p = cls.parent.clone();
    while let Some(x) = p {
        parents.push(x.name.clone());
        p = x.parent.clone();
    }
    ClassInfo {
        name: cls.name.clone(),
        parents,
        interfaces: cls.interfaces.iter().map(|i| i.name.clone()).collect(),
        traits: cls.traits.iter().map(|t| t.name.clone()).collect(),
    }
}

/// The VM seen through [`SplHost`].
pub struct VmHost<'a> {
    /// The VM.
    pub vm: &'a mut Vm,
    pcre: Option<php_pcre::Pcre>,
}

impl std::fmt::Debug for VmHost<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VmHost")
    }
}

impl<'a> VmHost<'a> {
    /// Wraps the VM.
    pub fn new(vm: &'a mut Vm) -> Self {
        VmHost { vm, pcre: None }
    }

    fn vm_rt<T>(&mut self, r: Result<T, Throw>) -> RtResult<T> {
        r.map_err(|t| to_rt(self.vm, t))
    }
}

impl Drop for VmHost<'_> {
    fn drop(&mut self) {
        if let Some(p) = self.pcre.take() {
            PCRE.with(|s| {
                let mut s = s.borrow_mut();
                if s.is_none() {
                    *s = Some(p);
                }
            });
        }
    }
}

impl RuntimeContext for VmHost<'_> {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        self.vm.diagnostic(level, message)
    }
    fn precision(&self) -> i64 {
        self.vm.precision()
    }
    fn serialize_precision(&self) -> i64 {
        self.vm.serialize_precision()
    }
    fn cast_object(&mut self, obj: &Obj, target: CastTarget) -> RtResult<Option<Value>> {
        self.vm.cast_object(obj, target)
    }
    fn do_operation(&mut self, op: BinaryOp, op1: &Value, op2: &Value) -> RtResult<Option<Value>> {
        self.vm.do_operation(op, op1, op2)
    }
    fn compare_objects(&mut self, op1: &Value, op2: &Value) -> RtResult<i32> {
        self.vm.compare_objects(op1, op2)
    }
    fn object_properties(
        &mut self,
        obj: &Obj,
        purpose: PropertyPurpose,
    ) -> RtResult<Option<ZArray>> {
        self.vm.object_properties(obj, purpose)
    }
    fn object_dump(&mut self, obj: &Obj, purpose: PropertyPurpose) -> RtResult<ObjectDump> {
        self.vm.object_dump(obj, purpose)
    }
    fn serialize_object(&mut self, obj: &Obj) -> RtResult<SerializedObject> {
        self.vm.serialize_object(obj)
    }
    fn unserialize_object_create(&mut self, class: &ZStr) -> RtResult<Value> {
        self.vm.unserialize_object_create(class)
    }
    fn unserialize_object_finish(&mut self, obj: &Value, props: ZArray) -> RtResult<()> {
        self.vm.unserialize_object_finish(obj, props)
    }
    fn unserialize_enum(&mut self, class: &ZStr, case: &ZStr) -> RtResult<Value> {
        self.vm.unserialize_enum(class, case)
    }
    fn unserialize_custom(&mut self, class: &ZStr, data: &[u8]) -> RtResult<Value> {
        self.vm.unserialize_custom(class, data)
    }
    fn is_enum(&mut self, obj: &Obj) -> bool {
        class_of(obj).is_enum()
    }
    fn count_object(&mut self, obj: &Obj) -> RtResult<Option<i64>> {
        RuntimeContext::count_object(self.vm, obj)
    }
    fn property_read(&mut self, obj: &Obj, name: &ZStr) -> RtResult<Option<Value>> {
        self.vm.property_read(obj, name)
    }
    fn property_write(&mut self, obj: &Obj, name: &ZStr, value: Value) -> RtResult<()> {
        self.vm.property_write(obj, name, value)
    }
    fn create_std_object(&mut self, props: ZArray) -> RtResult<Value> {
        self.vm.create_std_object(props)
    }
}

impl ArrayContext for VmHost<'_> {
    fn is_enum(&self, obj: &Obj) -> bool {
        class_of(obj).is_enum()
    }
    fn count_object(&mut self, obj: &Obj) -> RtResult<Option<i64>> {
        ArrayContext::count_object(self.vm, obj)
    }
    fn column_property(&mut self, obj: &Obj, name: &ZStr) -> RtResult<Option<Value>> {
        self.vm.column_property(obj, name)
    }
    fn this_object(&mut self) -> Option<Value> {
        self.vm.this_object()
    }
}

impl SplHost for VmHost<'_> {
    fn spl_state(&mut self, obj: &Obj) -> Option<Rc<SplObject>> {
        spl_state_of(obj)
    }

    fn call_method(&mut self, obj: &Obj, name: &str, args: &[Value]) -> RtResult<Value> {
        let r = self.vm.call_method_by_name(obj, name, args.to_vec());
        self.vm_rt(r)
    }

    fn has_user_method(&mut self, obj: &Obj, name: &str) -> bool {
        is_user_method(class_of(obj), name)
    }

    fn has_method(&mut self, obj: &Obj, name: &str) -> bool {
        let lc = name.to_ascii_lowercase();
        class_of(obj).find_method(lc.as_bytes()).is_some()
    }

    fn instance_of(&mut self, obj: &Obj, class: &str) -> bool {
        let lc = class.trim_start_matches('\\').to_ascii_lowercase();
        class_of(obj).instance_of_name(lc.as_bytes())
    }

    fn create_object(&mut self, class: &ZStr) -> RtResult<Obj> {
        let Some(cls) = self.vm.find_class(class.as_bytes()) else {
            return Err(RuntimeError::Error(format!(
                "Class \"{}\" not found",
                class.to_string_lossy()
            )));
        };
        let r = self.vm.new_object(&cls);
        self.vm_rt(r)
    }

    fn instantiate(&mut self, class: &ZStr, args: &[Value]) -> RtResult<Obj> {
        let r = self.vm.lookup_class(class);
        let Some(cls) = self.vm_rt(r)? else {
            return Err(RuntimeError::Error(format!(
                "Class \"{}\" not found",
                class.to_string_lossy()
            )));
        };
        let r = self.vm.new_object_with_args(&cls, args.to_vec());
        self.vm_rt(r)
    }

    fn class_is_a(&mut self, class: &ZStr, parent: &str) -> Option<bool> {
        let cls = self.vm.find_class(class.as_bytes())?;
        let lc = parent.trim_start_matches('\\').to_ascii_lowercase();
        Some(cls.instance_of_name(lc.as_bytes()))
    }

    fn call_callable(&mut self, callable: &Value, args: &[Value]) -> RtResult<Value> {
        let r = self.vm.call_value(callable, args.to_vec());
        self.vm_rt(r)
    }

    fn check_callable(&mut self, callable: &Value) -> Result<(), String> {
        if self.vm.is_callable(callable) {
            Ok(())
        } else {
            Err(self.vm.callable_error_message(callable))
        }
    }

    fn is_overloaded_object(&mut self, obj: &Obj) -> bool {
        spl_state_of(obj).is_none()
            && class_of(obj)
                .hooks
                .as_ref()
                .is_some_and(|h| h.properties_for.is_some())
    }

    fn std_properties(&mut self, obj: &Obj) -> ZArray {
        std_properties(obj)
    }

    fn set_std_properties(&mut self, obj: &Obj, props: ZArray) -> RtResult<()> {
        set_std_properties(obj, &props);
        Ok(())
    }

    fn class_info(&mut self, name: &ZStr, autoload: bool) -> Option<ClassInfo> {
        let cls = if autoload {
            match self.vm.lookup_class(name) {
                Ok(c) => c,
                Err(t) => {
                    if self.vm.pending_throw.is_none() {
                        self.vm.pending_throw = Some(t);
                    }
                    None
                }
            }
        } else {
            self.vm.find_class(name.as_bytes())
        }?;
        Some(class_info_of(&cls))
    }

    fn object_class_info(&mut self, obj: &Obj) -> ClassInfo {
        class_info_of(class_of(obj))
    }

    fn new_internal_iterator(&mut self, it: Box<dyn ObjectIterator>) -> RtResult<Value> {
        let r = crate::iter::new_internal_iterator(self.vm, it);
        self.vm_rt(r)
    }

    fn autoload_include(&mut self, file: &ZStr) -> RtResult<bool> {
        let path = file.to_string_lossy();
        let resolved = crate::funcs::resolve_include(self.vm, &path);
        let Some(full) = resolved else {
            return Ok(false);
        };
        if self.vm.included_set.contains(&ZStr::from(full.as_str())) {
            return Ok(true);
        }
        let r = self.vm.include_file(&full);
        self.vm_rt(r)?;
        Ok(true)
    }

    fn pcre(&mut self) -> &mut php_pcre::Pcre {
        if self.pcre.is_none() {
            let p = PCRE.with(|s| s.borrow_mut().take());
            self.pcre = Some(p.unwrap_or_else(php_pcre::Pcre::new));
        }
        self.pcre.get_or_insert_with(php_pcre::Pcre::new)
    }

    fn write_output(&mut self, bytes: &[u8]) -> RtResult<()> {
        self.vm.output(bytes);
        Ok(())
    }
}
