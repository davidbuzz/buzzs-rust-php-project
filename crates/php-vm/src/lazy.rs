//! Lazy objects (`Zend/zend_lazy_objects.c`): ghosts and proxies created by
//! `ReflectionClass::newLazyGhost()` / `newLazyProxy()`.
//!
//! The property handlers in `props.rs` call [`Vm::lazy_object_for`] before
//! every property operation; it initializes an uninitialized lazy object
//! (or returns the real instance of an initialized proxy).
//!
//! Status: hook points only — the initialization logic and the Reflection
//! methods (`newLazyGhost`, `newLazyProxy`, `resetAsLazy*`,
//! `initializeLazyObject`, `markLazyObjectAsInitialized`,
//! `isUninitializedLazyObject`, `getLazyInitializer`,
//! `ReflectionProperty::skipLazyInitialization`,
//! `setRawValueWithoutLazyInitialization`, `isLazy`) are still to be
//! written.

use crate::object;
use crate::vm::{Vm, VmResult};
use php_runtime::{Obj, Value, ZStr};

/// Lazy-object state stored in `ObjectData::lazy`.
#[derive(Debug, Clone)]
pub struct LazyInfo {
    /// Proxy (true) or ghost (false).
    pub proxy: bool,
    /// The initializer (ghost) or factory (proxy) callable.
    pub initializer: Value,
    /// `ReflectionClass::SKIP_*` options.
    pub options: i64,
    /// Whether initialization has completed.
    pub initialized: bool,
    /// The real instance of an initialized proxy.
    pub instance: Option<Obj>,
    /// Re-entrancy guard.
    pub initializing: bool,
}

impl Vm {
    /// The object a property operation on `o` must act on, initializing
    /// `o` first if it is an uninitialized lazy object. `name` is the
    /// property (or `None` for whole-object operations such as
    /// `get_object_vars`).
    pub(crate) fn lazy_object_for(&mut self, o: &Obj, name: Option<&ZStr>) -> VmResult<Obj> {
        let _ = name;
        let d = object::data(o);
        let info = d.lazy.borrow().clone();
        match info {
            None => Ok(o.clone()),
            Some(info) => {
                if info.initialized {
                    if let Some(inst) = info.instance {
                        return Ok(inst);
                    }
                }
                Ok(o.clone())
            }
        }
    }

    /// Whether `o` is an uninitialized lazy object (`var_dump` prefix).
    pub(crate) fn lazy_prefix(&self, o: &Obj) -> &'static str {
        match object::data(o).lazy.borrow().as_ref() {
            Some(i) if !i.initialized => {
                if i.proxy {
                    "lazy proxy "
                } else {
                    "lazy ghost "
                }
            }
            Some(i) if i.proxy => "lazy proxy ",
            _ => "",
        }
    }
}
