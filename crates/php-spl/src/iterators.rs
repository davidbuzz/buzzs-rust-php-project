//! The iterator classes of `spl_iterators.c`.

/// The state of `IteratorIterator` and the classes built on it.
#[derive(Debug, Clone, Default)]
pub struct DualIt {}

impl DualIt {
    /// A new state for the internal class `base`.
    #[must_use]
    pub fn new(base: &str) -> Self {
        let _ = base;
        DualIt {}
    }
}

/// The state of `RecursiveIteratorIterator` / `RecursiveTreeIterator`.
#[derive(Debug, Clone, Default)]
pub struct RecursiveIt {}

impl RecursiveIt {
    /// A new state for the internal class `base`.
    #[must_use]
    pub fn new(base: &str) -> Self {
        let _ = base;
        RecursiveIt {}
    }
}

use crate::host::{ObjectIterator, SplHost};
use php_runtime::{Obj, RtResult, Value};

pub(crate) fn call(
    _host: &mut dyn SplHost,
    _class: &str,
    _method: &str,
    _this: Option<&Obj>,
    _args: &[Value],
) -> Option<RtResult<Value>> {
    None
}

/// The native iterator of `RecursiveIteratorIterator`.
#[derive(Debug)]
pub struct RecursiveItIter {
    obj: Obj,
}

impl RecursiveItIter {
    /// Creates it.
    #[must_use]
    pub fn new(obj: Obj) -> Self {
        RecursiveItIter { obj }
    }
}

impl ObjectIterator for RecursiveItIter {
    fn rewind(&mut self, host: &mut dyn SplHost) -> RtResult<()> {
        host.call_method(&self.obj, "rewind", &[]).map(|_| ())
    }
    fn valid(&mut self, host: &mut dyn SplHost) -> RtResult<bool> {
        let v = host.call_method(&self.obj, "valid", &[])?;
        v.to_bool(host)
    }
    fn current(&mut self, host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        host.call_method(&self.obj, "current", &[]).map(Some)
    }
    fn key(&mut self, host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        host.call_method(&self.obj, "key", &[]).map(Some)
    }
    fn next(&mut self, host: &mut dyn SplHost) -> RtResult<()> {
        host.call_method(&self.obj, "next", &[]).map(|_| ())
    }
}
