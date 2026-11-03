//! [`BridgeHost`]: the [`XslHost`] a transformation runs PHP callbacks
//! through.
//!
//! Transformations run inside `php_ext_xml::libxml::run`, which lends the
//! VM to the stream layer (libxslt's `document()`, `xsl:include` and
//! `transformToUri()` I/O goes through PHP streams) and collects the
//! first exception raised by any callback (`EG(exception)`); the host
//! reaches the VM the same way, through `php_ext_streams::bridge`.

use php_ext_streams::bridge;
use php_ext_xml::libxml;
use php_runtime::{Value, ZArray, ZStr};
use php_vm::{Throw, Vm, VmResult};
use php_xsl::{CallbackArg, CallbackContext, CallbackReturn, NodeArg, XslError, XslHost};

use crate::access::{DomAccess, XmlSource};

/// Converts a library exception into a VM throw.
pub(crate) fn throw_xsl(vm: &mut Vm, e: &XslError) -> Throw {
    vm.throw(e.class().name(), &e.message())
}

/// Runs `f` with the lent VM; a throw becomes the pending exception.
fn with_vm<R>(f: impl FnOnce(&mut Vm) -> VmResult<R>) -> Option<R> {
    match bridge::with_vm(f) {
        Some(Ok(r)) => Some(r),
        Some(Err(t)) => {
            bridge::set_pending(t);
            None
        }
        None => None,
    }
}

/// Sets the pending exception to a new exception of `class`.
fn throw(class: &str, message: &str) {
    with_vm(|vm| -> VmResult<()> { Err(vm.throw(class, message)) });
}

/// The engine services of one transformation.
pub(crate) struct BridgeHost<'a> {
    input: &'a XmlSource,
    /// The DOM provider (node-set arguments and node return values).
    dom: Option<&'static dyn DomAccess>,
}

impl std::fmt::Debug for BridgeHost<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeHost")
            .field("input", self.input)
            .finish_non_exhaustive()
    }
}

impl<'a> BridgeHost<'a> {
    pub(crate) fn new(input: &'a XmlSource, dom: Option<&'static dyn DomAccess>) -> Self {
        Self { input, dom }
    }

    /// Converts the callback arguments; `None` when a node-set cannot be
    /// represented (an exception is then pending).
    fn convert_args(
        &self,
        vm: &mut Vm,
        args: Vec<CallbackArg<'_>>,
        ctx: &CallbackContext<'_>,
    ) -> VmResult<Vec<Value>> {
        let mut out = Vec::with_capacity(args.len());
        for arg in args {
            out.push(match arg {
                CallbackArg::Str(s) => Value::Str(ZStr::from(s)),
                CallbackArg::Bool(b) => Value::Bool(b),
                CallbackArg::Float(f) => Value::Float(f),
                CallbackArg::Nodes(nodes) => Value::Array(self.node_array(vm, nodes, ctx)?),
            });
        }
        Ok(out)
    }

    fn node_array(
        &self,
        vm: &mut Vm,
        nodes: Vec<NodeArg<'_>>,
        ctx: &CallbackContext<'_>,
    ) -> VmResult<ZArray> {
        let mut arr = ZArray::new();
        for node in nodes {
            let value = match self.dom {
                Some(dom) => dom.node_value(vm, self.input, node, ctx)?,
                None => None,
            };
            let Some(value) = value else {
                return Err(vm.throw(
                    "Error",
                    "XPath node-sets cannot be passed to PHP callbacks: the dom extension is not available",
                ));
            };
            let _ = arr.make_mut().push(value);
        }
        Ok(arr)
    }

    /// `php_dom_xpath_callback_dispatch`'s handling of the return value.
    fn convert_return(&self, vm: &mut Vm, value: &Value, modern: bool) -> VmResult<CallbackReturn> {
        Ok(match value.deref() {
            Value::Object(o) => match self.dom.and_then(|d| d.returned_node(vm, &o, modern)) {
                Some(node) => CallbackReturn::Node(node),
                None => CallbackReturn::NonNodeObject,
            },
            Value::Bool(b) => CallbackReturn::Bool(b),
            other => CallbackReturn::Str(vm.value_to_string(&other)?.as_bytes().to_vec()),
        })
    }

    /// Converts the arguments, reports the libxslt messages queued so far
    /// (so that they appear in order with the callback's own output),
    /// calls `callable` and converts its result.
    fn invoke(
        &self,
        callable: &Value,
        ctx: &CallbackContext<'_>,
        args: Vec<CallbackArg<'_>>,
    ) -> CallbackReturn {
        if bridge::has_pending() {
            return CallbackReturn::Undef;
        }
        with_vm(|vm| {
            let args = self.convert_args(vm, args, ctx)?;
            libxml::flush_diagnostics(vm)?;
            let ret = vm.call_value(callable, args)?;
            self.convert_return(vm, &ret, ctx.modern)
        })
        .unwrap_or(CallbackReturn::Undef)
    }
}

impl XslHost<Value> for BridgeHost<'_> {
    fn call_by_name(
        &mut self,
        ctx: &CallbackContext<'_>,
        args: Vec<CallbackArg<'_>>,
    ) -> CallbackReturn {
        let name = Value::Str(ZStr::from(ctx.function));
        // zend_call_function() with an uncallable string: "Invalid callback
        // f, function "f" not found or invalid function name". Static method
        // names are left to the call itself, which autoloads the class first
        // (and propagates an exception thrown by the autoloader).
        if !ctx.function.windows(2).any(|w| w == b"::") {
            let invalid = with_vm(|vm| -> VmResult<Option<String>> {
                Ok((!vm.is_callable(&name)).then(|| vm.callable_error_message(&name)))
            });
            if let Some(Some(reason)) = invalid {
                let msg = format!(
                    "Invalid callback {}, {reason}",
                    String::from_utf8_lossy(ctx.function)
                );
                throw("Error", &msg);
                return CallbackReturn::Undef;
            }
        }
        self.invoke(&name, ctx, args)
    }

    fn call(
        &mut self,
        callable: &Value,
        ctx: &CallbackContext<'_>,
        args: Vec<CallbackArg<'_>>,
    ) -> CallbackReturn {
        self.invoke(callable, ctx, args)
    }

    fn throw(&mut self, error: XslError) {
        throw(error.class().name(), &error.message());
    }

    fn exception_pending(&self) -> bool {
        bridge::has_pending()
    }

    fn warning(&mut self, message: &str) {
        if bridge::has_pending() {
            return;
        }
        with_vm(|vm| {
            libxml::flush_diagnostics(vm)?;
            vm.php_warning(message)
        });
    }

    fn open_basedir_allows(&mut self, path: &[u8]) -> bool {
        // Queues the restriction warning, emitted when the call returns.
        php_streams::paths::check_open_basedir(path)
    }

    fn transformation_finished(&mut self) {
        if let Some(dom) = self.dom {
            bridge::with_vm(|vm| dom.transformation_finished(vm));
        }
    }
}
