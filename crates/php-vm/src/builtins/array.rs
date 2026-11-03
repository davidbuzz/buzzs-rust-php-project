//! The VM side of the array functions.
//!
//! The functions of `ext/standard/array.c` are implemented by `php-stdarr`
//! and bound by the `php-ext-stdarr` extension crate. This module only
//! provides what that binding needs from VM internals: the
//! [`php_stdarr::ArrayContext`] object hooks, the request RNG accessor
//! shared with `mt_rand()`, and the callable diagnostics used for
//! "must be a valid callback, ..." errors.

use crate::object;
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
        let has = self.has_property(obj, name, false).map_err(|t| self.to_rt(t))?;
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
    pub(crate) fn random_globals(&mut self) -> &mut php_random::RandomGlobals {
        self.ext.random.get_or_insert_with(php_random::RandomGlobals::new)
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
