//! Access to the `ssl` stream context options (`GET_VER_OPT*` in
//! `xp_ssl.c`) and writing captured values back into the context.

use php_ext_streams::res::ctx_cell_of_lib;
use php_runtime::{Value, ZArray, ZStr};
use php_streams::context::ContextRef;
use php_streams::stream::Stream;
use php_vm::ext::VmResult;
use php_vm::Vm;

/// The `ssl` options of a stream's context.
#[derive(Debug, Clone, Default)]
pub(crate) struct SslOpts {
    /// Snapshot of `$context['ssl']`.
    ssl: ZArray,
    /// The context, for `php_stream_context_set_option()`.
    ctx: Option<ContextRef>,
}

impl SslOpts {
    /// The options of `ctx` (re-read: options may change between calls).
    pub(crate) fn from_context(ctx: Option<&ContextRef>) -> Self {
        let ssl = ctx
            .and_then(ctx_cell_of_lib)
            .and_then(|cell| {
                cell.options
                    .borrow()
                    .get_str(b"ssl")
                    .map(Value::deref)
                    .and_then(|v| v.as_array().cloned())
            })
            .unwrap_or_default();
        SslOpts {
            ssl,
            ctx: ctx.cloned(),
        }
    }

    /// The options of a stream's context.
    pub(crate) fn of_stream(s: &Stream) -> Self {
        Self::from_context(s.context.as_ref())
    }

    /// Whether the stream has a context (`PHP_STREAM_CONTEXT(stream)`).
    pub(crate) fn has_context(&self) -> bool {
        self.ctx.is_some()
    }

    /// `GET_VER_OPT(name)`.
    pub(crate) fn get(&self, name: &str) -> Option<Value> {
        self.ssl.get_str(name.as_bytes()).map(Value::deref)
    }

    /// `GET_VER_OPT(name) && zend_is_true(val)` / `!zend_is_true`.
    pub(crate) fn bool(&self, vm: &mut Vm, name: &str) -> VmResult<Option<bool>> {
        match self.get(name) {
            Some(v) => Ok(Some(vm.value_to_bool(&v)?)),
            None => Ok(None),
        }
    }

    /// `GET_VER_OPT_LONG`.
    pub(crate) fn long(&self, vm: &mut Vm, name: &str) -> VmResult<Option<i64>> {
        match self.get(name) {
            Some(v) => Ok(Some(vm.value_to_int(&v)?)),
            None => Ok(None),
        }
    }

    /// `GET_VER_OPT_STRING` (C strings stop at the first NUL).
    pub(crate) fn string(&self, vm: &mut Vm, name: &str) -> VmResult<Option<String>> {
        match self.get(name) {
            Some(v) => {
                let s = vm.value_to_string(&v)?;
                let b = s.as_bytes();
                let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
                Ok(Some(String::from_utf8_lossy(&b[..end]).into_owned()))
            }
            None => Ok(None),
        }
    }

    /// `GET_VER_OPT_STRINGL`: the whole string, NUL bytes included.
    pub(crate) fn string_raw(&self, vm: &mut Vm, name: &str) -> VmResult<Option<String>> {
        match self.get(name) {
            Some(v) => Ok(Some(vm.value_to_string(&v)?.to_string_lossy())),
            None => Ok(None),
        }
    }

    /// `php_stream_context_set_option(ctx, "ssl", name, value)`.
    pub(crate) fn set(&self, name: &str, value: Value) {
        let Some(cell) = self.ctx.as_ref().and_then(ctx_cell_of_lib) else {
            return;
        };
        let mut opts = cell.options.borrow_mut();
        let t = opts.make_mut();
        let mut ssl = t
            .get_str(b"ssl")
            .map(Value::deref)
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        ssl.make_mut().insert(ZStr::from(name), value);
        t.insert(ZStr::from("ssl"), Value::Array(ssl));
    }
}
