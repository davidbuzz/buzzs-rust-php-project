// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! In-place edits of registered functions and classes, for the parts of
//! the stubs that `ModuleBuilder::function` / `ClassBuilder` cannot
//! express (parameter and property attributes, `#[\NoDiscard]`,
//! deprecated aliases, traits of internal classes, attribute classes,
//! typed class constants, doc comments).

use php_runtime::ZStr;
use php_vm::class::{Class, Magic, NameMap};
use php_vm::func::{flags, Builtin, Func, Method};
use php_vm::Vm;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// Replaces the native function `name` with an edited copy.
pub(crate) fn edit_function(vm: &mut Vm, name: &str, edit: impl FnOnce(&mut Builtin)) {
    let lc = ZStr::from(name.to_ascii_lowercase());
    if let Some(Func::Builtin(b)) = vm.functions.get(&lc).cloned() {
        let mut nb = (*b).clone();
        edit(&mut nb);
        vm.functions.insert(lc, Func::Builtin(Rc::new(nb)));
    }
}

/// Marks a native function deprecated (`@deprecated` in a stub, or
/// `#[\Deprecated(since: ..., message: ...)]`).
pub(crate) fn deprecate_function(vm: &mut Vm, name: &str, since: &str, message: &str) {
    edit_function(vm, name, |b| b.flags |= flags::DEPRECATED);
    let mut suffix = Vec::new();
    if !since.is_empty() {
        suffix.extend_from_slice(b" since ");
        suffix.extend_from_slice(since.as_bytes());
    }
    if !message.is_empty() {
        suffix.extend_from_slice(b", ");
        suffix.extend_from_slice(message.as_bytes());
    }
    vm.builtin_deprecations
        .insert(ZStr::from(name.to_ascii_lowercase()), suffix);
}

/// `build_magic` over a method table.
// `ZStr` caches its hash in a cell; keys are never mutated (the VM's own
// tables use the same key type).
#[allow(clippy::mutable_key_type)]
pub(crate) fn magic_of(methods: &NameMap<Rc<Method>>) -> Magic {
    let g = |n: &str| methods.get(n.as_bytes()).cloned();
    Magic {
        construct: g("__construct"),
        destruct: g("__destruct"),
        get: g("__get"),
        set: g("__set"),
        isset: g("__isset"),
        unset: g("__unset"),
        call: g("__call"),
        call_static: g("__callstatic"),
        to_string: g("__tostring"),
        invoke: g("__invoke"),
        clone: g("__clone"),
        debug_info: g("__debuginfo"),
        serialize: g("__serialize"),
        unserialize: g("__unserialize"),
        sleep: g("__sleep"),
        wakeup: g("__wakeup"),
    }
}

/// A method of `old` re-pointed at the class being built (`weak`).
fn remap_method(m: &Rc<Method>, old: &Weak<Class>, weak: &Weak<Class>) -> Rc<Method> {
    let own = Weak::ptr_eq(&m.scope, old);
    let proto_own = Weak::ptr_eq(&m.proto_scope, old);
    if !own && !proto_own {
        return m.clone();
    }
    Rc::new(Method {
        name: m.name.clone(),
        func: m.func.clone(),
        flags: m.flags,
        scope: if own { weak.clone() } else { m.scope.clone() },
        proto_scope: if proto_own {
            weak.clone()
        } else {
            m.proto_scope.clone()
        },
    })
}

/// Re-creates the registered class `cls` with `edit` applied, and
/// replaces it in the class table. Methods declared by the class are
/// re-pointed at the new class; the magic method cache is rebuilt after
/// the edit. Register subclasses only after patching their parent.
#[allow(clippy::mutable_key_type)]
pub(crate) fn rebuild_class(
    vm: &mut Vm,
    cls: &Rc<Class>,
    edit: impl FnOnce(&mut Class, &Weak<Class>),
) -> Rc<Class> {
    let old = Rc::downgrade(cls);
    let new = Rc::new_cyclic(|weak: &Weak<Class>| {
        let methods: NameMap<Rc<Method>> = cls
            .methods
            .iter()
            .map(|(k, m)| (k.clone(), remap_method(m, &old, weak)))
            .collect();
        let mut c = Class {
            name: cls.name.clone(),
            lc_name: cls.lc_name.clone(),
            flags: cls.flags,
            parent: cls.parent.clone(),
            interfaces: cls.interfaces.clone(),
            traits: cls.traits.clone(),
            consts: cls.consts.clone(),
            const_map: cls.const_map.clone(),
            props: cls.props.clone(),
            prop_map: cls.prop_map.clone(),
            static_props: cls.static_props.clone(),
            static_map: cls.static_map.clone(),
            magic: Magic::default(),
            methods,
            method_order: cls.method_order.clone(),
            defaults: RefCell::new(cls.defaults.borrow().clone()),
            backing: cls.backing,
            enum_cases: RefCell::new(cls.enum_cases.borrow().clone()),
            decl: cls.decl.clone(),
            internal: cls.internal,
            attributes: cls.attributes.clone(),
            filename: cls.filename.clone(),
            line_start: cls.line_start,
            line_end: cls.line_end,
            doc_comment: cls.doc_comment.clone(),
            linked: Cell::new(cls.linked.get()),
            hooks: cls.hooks.clone(),
        };
        edit(&mut c, weak);
        c.magic = magic_of(&c.methods);
        c
    });
    vm.classes.insert(new.lc_name.clone(), new.clone());
    new
}

/// A copy of `m` declared by the class `weak` (for trait methods copied
/// into an internal class).
pub(crate) fn method_in(m: &Rc<Method>, weak: &Weak<Class>) -> Rc<Method> {
    Rc::new(Method {
        name: m.name.clone(),
        func: m.func.clone(),
        flags: m.flags,
        scope: weak.clone(),
        proto_scope: weak.clone(),
    })
}
