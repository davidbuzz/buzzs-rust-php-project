// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `stream_wrapper_register()` & co.: user-space wrapper classes, bound to
//! the library's calling protocol through [`UserWrapperClass`] and
//! [`UserStreamObject`].

use std::cell::RefCell;
use std::os::fd::RawFd;
use std::rc::Rc;

use php_runtime::{Obj, PhpRef, Resource, Value, ZArray};
use php_streams::context::ContextRef;
use php_streams::stream::CastAs;
use php_streams::user::{self, RegisterResult, UserStreamObject, UserWrapperClass};
use php_streams::value::{UserCall, Value as SValue};
use php_vm::class::Class;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::bridge;
use crate::res;

thread_local! {
    /// The most recently instantiated wrapper object (becomes the stream's
    /// `wrapper_data` when an open succeeds).
    static LAST_OBJECT: RefCell<Option<Obj>> = const { RefCell::new(None) };
    /// `le_protocols` resources allocated by `stream_wrapper_register()`.
    /// (protocol, resource), released by `stream_wrapper_unregister()`
    /// (GH-8548).
    static PROTOCOL_RESOURCES: RefCell<Vec<(Vec<u8>, Resource)>> = const { RefCell::new(Vec::new()) };
}

/// `zend_close_rsrc_list()` for the `stream factory` resources the
/// request's `stream_wrapper_register()` calls allocated (the request's
/// wrapper registrations themselves go with `php_shutdown_stream_hashes()`):
/// closed in reverse order of creation, as the regular list is. Kept, each
/// request on a long-running worker, or under `php --repeat`, left one more
/// behind.
pub fn shutdown() {
    let held = PROTOCOL_RESOURCES.with(|p| std::mem::take(&mut *p.borrow_mut()));
    for (_, r) in held.into_iter().rev() {
        r.close();
    }
}

/// The `stream factory` resources still open, in order of creation.
#[must_use]
pub fn live_factories() -> Vec<Resource> {
    PROTOCOL_RESOURCES.with(|p| p.borrow().iter().map(|(_, r)| r.clone()).collect())
}

/// Forgets the last instantiated object.
pub fn clear_last_object() {
    LAST_OBJECT.with(|l| *l.borrow_mut() = None);
}

/// Takes the last instantiated object as a value.
pub fn take_last_object() -> Option<Value> {
    LAST_OBJECT
        .with(|l| l.borrow_mut().take())
        .map(Value::Object)
}

/// A class registered as a wrapper.
#[derive(Debug)]
struct WrapperClass {
    class: Rc<Class>,
}

/// An instance of a wrapper class.
#[derive(Debug)]
pub struct WrapperObject {
    obj: Obj,
}

impl WrapperObject {
    /// The PHP object.
    pub fn object(&self) -> &Obj {
        &self.obj
    }
}

impl UserWrapperClass for WrapperClass {
    fn class_name(&self) -> String {
        php_vm::object::display_name(&self.class.name).to_string_lossy()
    }

    fn instantiate(&self, context: Option<&ContextRef>) -> Option<Box<dyn UserStreamObject>> {
        bridge::with_vm(|vm| {
            if bridge::has_pending() {
                return None;
            }
            let obj = match vm.new_object(&self.class) {
                Ok(o) => o,
                Err(t) => {
                    bridge::set_pending(t);
                    return None;
                }
            };
            let ctx = context.map_or(Value::Null, res::context_value);
            if let Err(t) = vm.set_property(&obj, "context", ctx) {
                bridge::set_pending(t);
                return None;
            }
            if self.class.magic.construct.is_some() {
                if let Err(t) = vm.call_method_by_name(&obj, "__construct", Vec::new()) {
                    bridge::set_pending(t);
                    return None;
                }
            }
            LAST_OBJECT.with(|l| *l.borrow_mut() = Some(obj.clone()));
            Some(Box::new(WrapperObject { obj }) as Box<dyn UserStreamObject>)
        })
        .flatten()
    }
}

/// Calls `method` on `obj` with library arguments, converting by-ref
/// arguments (`by_ref[i]`) back. Shared with user filters.
pub fn call_method(obj: &Obj, method: &str, args: &mut [SValue], by_ref: &[usize]) -> UserCall {
    bridge::with_vm(|vm| {
        if bridge::has_pending() {
            return UserCall::Threw;
        }
        if !method_exists(vm, obj, method) {
            return UserCall::NotImplemented;
        }
        let mut refs: Vec<(usize, PhpRef)> = Vec::new();
        let vals: Vec<Value> = args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let v = bridge::from_lib(a);
                if by_ref.contains(&i) {
                    let r = PhpRef::new(v);
                    refs.push((i, r.clone()));
                    Value::Reference(r)
                } else {
                    v
                }
            })
            .collect();
        match vm.call_method_by_name(obj, method, vals) {
            Ok(Value::Object(o)) if method == user::methods::READ => {
                // convert_to_string() of the returned value.
                match vm.value_to_string(&Value::Object(o)) {
                    Ok(s) => UserCall::Returned(SValue::Str(s.as_bytes().to_vec())),
                    Err(t) => {
                        bridge::set_pending(t);
                        UserCall::Threw
                    }
                }
            }
            Ok(v) => {
                for (i, r) in refs {
                    args[i] = bridge::to_lib(&r.get());
                }
                UserCall::Returned(bridge::to_lib(&v))
            }
            Err(t) => {
                bridge::set_pending(t);
                UserCall::Threw
            }
        }
    })
    .unwrap_or(UserCall::Threw)
}

/// `zend_is_callable_ex()` for a method name on an object.
pub fn method_exists(vm: &mut Vm, obj: &Obj, method: &str) -> bool {
    let mut cb = ZArray::new();
    let _ = cb.make_mut().push(Value::Object(obj.clone()));
    let _ = cb.make_mut().push(Value::Str(method.into()));
    vm.is_callable(&Value::Array(cb))
}

impl UserStreamObject for WrapperObject {
    fn call(&mut self, method: &str, args: &mut [SValue]) -> UserCall {
        let by_ref: &[usize] = if method == user::methods::OPEN {
            &[3]
        } else {
            &[]
        };
        call_method(&self.obj, method, args, by_ref)
    }

    fn is_callable(&self, method: &str) -> bool {
        bridge::with_vm(|vm| method_exists(vm, &self.obj, method)).unwrap_or(false)
    }

    fn cast_stream(&mut self, stream_id: u64, kind: CastAs) -> Option<RawFd> {
        let Value::Resource(r) = res::stream_value_by_id(stream_id)? else {
            return None;
        };
        let cell = res::cell_of(&r)?;
        let mut s = cell.get()?;
        s.cast(kind, true)
    }
}

fn stream_wrapper_register(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let protocol = res::str_arg(a, 0);
    let name = a.str(1);
    let Some(class) = vm.lookup_class(&name)? else {
        return Err(a.type_error(
            vm,
            2,
            &format!(
                "must be a valid class name, \"{}\" given",
                name.to_string_lossy()
            ),
        ));
    };
    if class.is_interface() || class.is_trait() || class.is_abstract() || class.is_enum() {
        return Err(a.value_error(vm, 2, "must be a concrete class"));
    }
    let flags = a.int(2);
    let r = Resource::new("stream factory", None);
    let wc: Rc<dyn UserWrapperClass> = Rc::new(WrapperClass { class });
    let result = bridge::run(vm, || user::register(&protocol, wc, flags))?;
    if result == RegisterResult::Ok {
        PROTOCOL_RESOURCES.with(|p| p.borrow_mut().push((protocol.clone(), r)));
        Ok(Value::Bool(true))
    } else {
        r.close();
        Ok(Value::Bool(false))
    }
}

fn stream_wrapper_unregister(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let protocol = res::str_arg(a, 0);
    let r = bridge::run(vm, || user::unregister(&protocol))?;
    if r {
        let released = PROTOCOL_RESOURCES.with(|p| {
            let mut p = p.borrow_mut();
            p.iter()
                .rposition(|(name, _)| *name == protocol)
                .map(|i| p.remove(i))
        });
        if let Some((_, res)) = released {
            res.close();
        }
    }
    Ok(Value::Bool(r))
}

fn stream_wrapper_restore(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let protocol = res::str_arg(a, 0);
    let r = bridge::run(vm, || user::restore(&protocol))?;
    Ok(Value::Bool(r))
}

/// Registers the functions of this module.
pub fn register(m: &mut ModuleBuilder<'_>) {
    m.function_static(
        "stream_wrapper_register(string $protocol, string $class, int $flags = 0): bool",
        stream_wrapper_register,
    )
    .function_static(
        "stream_register_wrapper(string $protocol, string $class, int $flags = 0): bool",
        stream_wrapper_register,
    )
    .function_static(
        "stream_wrapper_unregister(string $protocol): bool",
        stream_wrapper_unregister,
    )
    .function_static(
        "stream_wrapper_restore(string $protocol): bool",
        stream_wrapper_restore,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The request's `stream factory` resources are closed at its end, and
    /// the next request holds none of them.
    #[test]
    fn shutdown_closes_the_requests_factories() {
        let a = Resource::new("stream factory", None);
        let b = Resource::new("stream factory", None);
        PROTOCOL_RESOURCES.with(|p| {
            p.borrow_mut().push((b"a".to_vec(), a.clone()));
            p.borrow_mut().push((b"b".to_vec(), b.clone()));
        });
        assert_eq!(live_factories().len(), 2);
        shutdown();
        assert!(a.is_closed() && b.is_closed());
        assert!(live_factories().is_empty());
    }
}
