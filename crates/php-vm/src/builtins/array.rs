//! The VM side of the array functions.
//!
//! The functions of `ext/standard/array.c` are implemented by `php-stdarr`
//! and bound by the `php-ext-stdarr` extension crate. This module only
//! provides what that binding needs from VM internals: the
//! [`php_stdarr::ArrayContext`] object hooks, the request RNG accessor
//! shared with `mt_rand()`, the callable diagnostics used for
//! "must be a valid callback, ..." errors, and immediate destructor runs.

use crate::object;
use crate::props::PropLookup;
use crate::vm::Vm;
use php_runtime::{ArrayKey, Obj, PhpRef, RtResult, Value, ZArray, ZStr};
use php_stdarr as sa;

impl sa::ArrayContext for Vm {
    fn is_enum(&self, obj: &Obj) -> bool {
        object::class_of(obj).is_enum()
    }

    fn count_object(&mut self, obj: &Obj) -> RtResult<Option<i64>> {
        php_runtime::RuntimeContext::count_object(self, obj)
    }

    fn column_property(&mut self, obj: &Obj, name: &ZStr) -> RtResult<Option<Value>> {
        // `has_property(ZEND_PROPERTY_EXISTS) || has_property(ZEND_PROPERTY_ISSET)`.
        let has = self.column_prop_exists(obj, name)
            || self
                .has_property(obj, name, false)
                .map_err(|t| self.to_rt(t))?;
        if !has {
            return Ok(None);
        }
        self.read_property(obj, name, crate::opcode::FetchMode::Is)
            .map(|v| Some(v.deref()))
            .map_err(|t| self.to_rt(t))
    }

    fn prepare_object_walk(&mut self, obj: &Obj) -> RtResult<()> {
        self.lazy_object_for(obj, None)
            .map(|_| ())
            .map_err(|t| self.to_rt(t))
    }

    fn property_reference(
        &mut self,
        obj: &Obj,
        key: &ArrayKey,
    ) -> RtResult<Option<sa::PropertyRef>> {
        let target = self.lazy_object_for(obj, None).map_err(|t| self.to_rt(t))?;
        let obj = &target;
        let cls = object::class_of(obj).clone();
        if cls
            .hooks
            .as_ref()
            .is_some_and(|h| h.properties_for.is_some())
        {
            // Native property tables: a detached reference, written back.
            let table = php_runtime::RuntimeContext::object_properties(
                self,
                obj,
                php_runtime::PropertyPurpose::ArrayCast,
            )?;
            return Ok(table.and_then(|t| {
                t.get(key).map(|v| match v {
                    Value::Reference(r) => sa::PropertyRef::Bound(r.clone()),
                    other => sa::PropertyRef::Detached(PhpRef::new(other.clone())),
                })
            }));
        }
        let d = object::data(obj);
        // A declared slot (`ZVAL_MAKE_REF` on the property slot);
        // uninitialized typed properties are skipped.
        if let Some(pi) = cls
            .props
            .iter()
            .find(|p| !p.is_virtual() && crate::props::mangle(p) == *key)
        {
            if pi.is_readonly() {
                return Err(self.make_throw(
                    "Error",
                    &format!(
                        "Cannot acquire reference to readonly property {}::${}",
                        pi.class, pi.name
                    ),
                ))
                .map_err(|t| self.to_rt(t));
            }
            let r = {
                let mut slots = d.slots.borrow_mut();
                match slots.get_mut(pi.slot as usize) {
                    Some(Some(v)) => v.make_ref(),
                    _ => return Ok(None),
                }
            };
            self.add_slot_type_source(obj, pi.slot as usize, &r);
            return Ok(Some(sa::PropertyRef::Bound(r)));
        }
        // A dynamic property (numeric names are integer keys in the
        // `(array)` table but string keys in the object).
        let mut dynamic = d.dynamic.borrow_mut();
        let Some(t) = dynamic.as_mut() else {
            return Ok(None);
        };
        let lookup = match key {
            ArrayKey::Int(i) if t.get(key).is_none() => ArrayKey::Str(ZStr::from(i.to_string())),
            k => k.clone(),
        };
        Ok(t.get_mut(&lookup)
            .map(|v| sa::PropertyRef::Bound(v.make_ref())))
    }

    fn this_object(&mut self) -> Option<Value> {
        let n = self.frames.len();
        if n < 2 {
            return None;
        }
        self.frames[n - 2].this.clone().map(Value::Object)
    }
}

impl Vm {
    /// `has_property(ZEND_PROPERTY_EXISTS)` without magic: an accessible,
    /// initialized declared property or a dynamic property, even if `null`.
    fn column_prop_exists(&mut self, obj: &Obj, name: &ZStr) -> bool {
        let cls = object::class_of(obj).clone();
        let scope = self.scope();
        match self.lookup_prop(&cls, name, scope.as_ref()) {
            PropLookup::Found(pi) => {
                pi.hooks.is_none()
                    && object::data(obj)
                        .slots
                        .borrow()
                        .get(pi.slot as usize)
                        .is_some_and(Option::is_some)
            }
            PropLookup::Inaccessible(_) => false,
            PropLookup::NotFound => object::data(obj)
                .dynamic
                .borrow()
                .as_ref()
                .is_some_and(|t| t.get(&php_runtime::ArrayKey::Str(name.clone())).is_some()),
        }
    }

    pub(crate) fn random_globals(&mut self) -> &mut php_random::RandomGlobals {
        self.ext
            .random
            .get_or_insert_with(php_random::RandomGlobals::new)
    }

    /// Runs the destructors of objects released so far, now, instead of
    /// after the current opcode (`zval_ptr_dtor` destroying objects inside
    /// a native function, e.g. `array_splice()` dropping removed elements).
    pub fn run_released_destructors(&mut self) -> crate::vm::VmResult<()> {
        if object::has_pending_destructors() {
            self.run_pending_destructors()?;
        }
        Ok(())
    }

    /// Why `v` is not callable, as printed after "must be a valid
    /// callback, " (`zend_is_callable_ex` error text), e.g.
    /// `function "foo" not found or invalid function name`.
    pub fn callable_error_message(&mut self, v: &Value) -> String {
        self.callable_error(v)
    }
}

/// Registers the array functions for the bare VM.
pub(crate) fn register(vm: &mut Vm) {
    super::reg(
        vm,
        "in_array(mixed $needle, array $haystack, bool $strict = false): bool",
        in_array,
    );
    super::reg(
        vm,
        "array_slice(array $array, int $offset, ?int $length = null, bool $preserve_keys = false): array",
        array_slice,
    );
}

fn count(vm: &mut Vm, a: &mut crate::func::Args) -> crate::vm::VmResult<Value> {
    let v = a.at(0);
    let mode = a.int(1);
    if mode == sa::COUNT_NORMAL {
        if let Value::Array(arr) = &v {
            if arr.is_packed() {
                return Ok(Value::Int(i64::try_from(arr.len()).unwrap_or(0)));
            }
        }
    }
    let f = a.func_name();
    sa::basic::count(vm, &f, &v, mode)
        .map(Value::Int)
        .map_err(|e| vm.rt_throw(e))
}

fn in_array(vm: &mut Vm, a: &mut crate::func::Args) -> crate::vm::VmResult<Value> {
    let n = a.at(0);
    let h = a.array(1);
    let strict = a.bool(2);
    if h.is_packed() {
        if strict {
            match &n {
                Value::Int(target) => {
                    for v in h.values() {
                        if let Value::Int(x) = v.deref() {
                            if x == *target {
                                return Ok(Value::Bool(true));
                            }
                        }
                    }
                    return Ok(Value::Bool(false));
                }
                Value::Str(target) => {
                    for v in h.values() {
                        if let Value::Str(x) = v.deref() {
                            if x.as_bytes() == target.as_bytes() {
                                return Ok(Value::Bool(true));
                            }
                        }
                    }
                    return Ok(Value::Bool(false));
                }
                Value::Bool(target) => {
                    for v in h.values() {
                        if let Value::Bool(x) = v.deref() {
                            if x == *target {
                                return Ok(Value::Bool(true));
                            }
                        }
                    }
                    return Ok(Value::Bool(false));
                }
                _ => {
                    for v in h.values() {
                        if php_stdarr::ops::is_identical(&n, &v.deref()).map_err(|e| vm.rt_throw(e))? {
                            return Ok(Value::Bool(true));
                        }
                    }
                    return Ok(Value::Bool(false));
                }
            }
        } else {
            for v in h.values() {
                if php_stdarr::ops::is_equal(&n, v, vm).map_err(|e| vm.rt_throw(e))? {
                    return Ok(Value::Bool(true));
                }
            }
            return Ok(Value::Bool(false));
        }
    }
    sa::basic::in_array(vm, &n, &h, strict)
        .map(Value::Bool)
        .map_err(|e| vm.rt_throw(e))
}

fn array_slice(_vm: &mut Vm, a: &mut crate::func::Args) -> crate::vm::VmResult<Value> {
    let arr = a.array(0);
    let offset = a.int(1);
    let length = a.opt(2).map(|_| a.int(2));
    let preserve_keys = a.bool(3);

    if arr.is_packed() {
        let num_in = i64::try_from(arr.len()).unwrap_or(i64::MAX);
        let mut len = length.unwrap_or(num_in);
        let mut off = offset;
        if off > num_in {
            return Ok(Value::Array(ZArray::new()));
        } else if off < 0 {
            off += num_in;
            if off < 0 {
                off = 0;
            }
        }
        if len < 0 {
            len += num_in - off;
        } else if off.unsigned_abs().saturating_add(len.unsigned_abs()) > num_in.unsigned_abs() {
            len = num_in - off;
        }
        if len <= 0 {
            return Ok(Value::Array(ZArray::new()));
        }
        let skip = usize::try_from(off).unwrap_or(0);
        let take = usize::try_from(len).unwrap_or(usize::MAX);

        if !preserve_keys || (off == 0 && arr.is_without_holes()) {
            let vals = arr
                .values()
                .skip(skip)
                .take(take)
                .map(php_stdarr::util::copy_elem);
            return Ok(Value::Array(ZArray::from_values(vals)));
        }
    }

    Ok(Value::Array(sa::basic::array_slice(
        &arr,
        offset,
        length,
        preserve_keys,
    )))
}

/// Registers `count()`, `sizeof()` and the `COUNT_*` constants for the
/// bare [`Vm::new`], attributed to `standard` as `php-ext-stdarr`
/// attributes them.
pub(crate) fn register_bare_count(vm: &mut Vm) {
    crate::reflection::record_module(vm, "standard", true);
    let mut m = crate::ext::ModuleBuilder::new(vm, "standard");
    m.function(
        "count(Countable|array $value, int $mode = COUNT_NORMAL): int",
        count,
    );
    m.function(
        "sizeof(Countable|array $value, int $mode = COUNT_NORMAL): int",
        count,
    );
    m.function(
        "in_array(mixed $needle, array $haystack, bool $strict = false): bool",
        in_array,
    );
    m.function(
        "array_slice(array $array, int $offset, ?int $length = null, bool $preserve_keys = false): array",
        array_slice,
    );
    m.constant("COUNT_NORMAL", Value::Int(sa::COUNT_NORMAL));
    m.constant("COUNT_RECURSIVE", Value::Int(sa::COUNT_RECURSIVE));
    crate::reflection::record_module(vm, "standard", false);
}

#[cfg(test)]
mod tests {
    use crate::{Vm, VmOptions};
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Sink(Rc<RefCell<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn bare_vm_has_count_and_with_extensions_starts_without_it() {
        let sink = Sink::default();
        let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
        let status = vm.run_script(
            br#"<?php
class C implements Countable { function count(): int { return 7; } }
echo count([1, 2, 3]), " ", sizeof([1, [2, 3]], COUNT_RECURSIVE), " ",
    count(new C), " ", COUNT_NORMAL, " ", COUNT_RECURSIVE, " ",
    (new ReflectionFunction('count'))->getExtensionName(), " ",
    (new ReflectionConstant('COUNT_NORMAL'))->getExtensionName(), "\n";
try { count(42); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
"#,
            "t.php",
        );
        assert_eq!(status, 0);
        let out = String::from_utf8(sink.0.borrow().clone()).expect("utf8");
        assert_eq!(
            out,
            "3 4 7 0 1 standard standard\ncount(): Argument #1 ($value) must be of type Countable|array, int given\n"
        );

        let sink2 = Sink::default();
        let mut ext_vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink2.clone()), &[]);
        let status2 = ext_vm.run_script(
            br"<?php
var_dump(function_exists('count'), function_exists('sizeof'), defined('COUNT_NORMAL'), defined('COUNT_RECURSIVE'));
",
            "t2.php",
        );
        assert_eq!(status2, 0);
        let out2 = String::from_utf8(sink2.0.borrow().clone()).expect("utf8");
        assert_eq!(out2, "bool(false)\nbool(false)\nbool(false)\nbool(false)\n");
    }
}
