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
use php_runtime::{Obj, RtResult, Value, ZStr};
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
        let has = self.column_prop_exists(obj, name) || self.has_property(obj, name, false).map_err(|t| self.to_rt(t))?;
        if !has {
            return Ok(None);
        }
        self.read_property(obj, name, crate::opcode::FetchMode::Is)
            .map(|v| Some(v.deref()))
            .map_err(|t| self.to_rt(t))
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
                pi.hooks.is_none() && object::data(obj).slots.borrow().get(pi.slot as usize).is_some_and(Option::is_some)
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
        self.ext.random.get_or_insert_with(php_random::RandomGlobals::new)
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

/// Registers the array functions: they are provided by the
/// `php-ext-stdarr` extension, so there is nothing to do here.
pub(crate) fn register(_vm: &mut Vm) {}
