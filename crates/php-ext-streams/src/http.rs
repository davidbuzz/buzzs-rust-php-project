//! The VM side of the `http://` wrapper: the [`HttpEnv`] services
//! (transports, TLS for proxy tunnels, `$http_response_header`), the
//! context notification callback, `get_headers()` and
//! `http_get_last_response_headers()` / `http_clear_last_response_headers()`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use php_runtime::{ArrayKey, Value, ZArray, ZStr};
use php_streams::context::{self, ContextRef, Notification};
use php_streams::http::{self, HttpEnv};
use php_streams::stream::Stream;
use php_streams::value::Value as SValue;
use php_streams::wrapper::{REPORT_ERRORS, STREAM_USE_URL};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::vm::FrameKind;
use php_vm::Vm;

use crate::bridge::{self, bytes};
use crate::res::{self, path_arg};

/// `STREAM_CRYPTO_METHOD_SSLv23_CLIENT`.
const SSLV23_CLIENT: i64 = 57;
/// `STREAM_CLIENT_CONNECT`.
const CLIENT_CONNECT: i64 = 4;

thread_local! {
    /// `BG(last_http_headers)`.
    static LAST_HEADERS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

/// The headers as a PHP list.
fn header_list(headers: &[Vec<u8>]) -> Value {
    let mut a = ZArray::new();
    for h in headers {
        let _ = a.make_mut().push(bytes(h));
    }
    Value::Array(a)
}

/// `zend_set_local_var_str(name, value, force = 0)`: assigns in the
/// nearest PHP frame — always in a scope with a symbol table (main
/// script, include, eval), in a function only when it uses the variable.
fn set_local_var(vm: &mut Vm, name: &[u8], value: Value) {
    let Some(idx) = vm
        .frames
        .iter()
        .rposition(|f| !matches!(f.kind, FrameKind::Builtin | FrameKind::ConstExpr))
    else {
        return;
    };
    let is_function = vm.frames[idx].kind == FrameKind::Function;
    let Some(table) = vm.rebuild_symbol_table(idx) else {
        return;
    };
    let old = {
        let mut vars = table.borrow_mut();
        let slot = if is_function {
            vars.find(name)
        } else {
            Some(vars.find_or_add(name))
        };
        slot.and_then(|i| vars.slots.get_mut(i as usize))
            .and_then(|s| s.replace(value))
    };
    vm.sync_symbol_table_writes(&table);
    drop(old);
}

/// The [`HttpEnv`] backed by the running VM.
#[derive(Debug)]
struct VmEnv;

impl HttpEnv for VmEnv {
    fn connect(
        &self,
        transport: &[u8],
        timeout: Option<Duration>,
        context: Option<&ContextRef>,
    ) -> Result<Stream, Option<Vec<u8>>> {
        let address = String::from_utf8_lossy(transport).into_owned();
        let r = bridge::with_vm(|vm| {
            crate::net::xport_create(
                vm,
                &address,
                timeout,
                false,
                CLIENT_CONNECT,
                None,
                context.cloned(),
            )
        });
        match r {
            Some(Ok(Ok(s))) => Ok(s),
            Some(Ok(Err(e))) => Err(Some(e.message.into_bytes())),
            Some(Err(t)) => {
                bridge::set_pending(t);
                Err(None)
            }
            None => Err(None),
        }
    }

    fn enable_crypto(&self, stream: Stream, peer_name: Option<&[u8]>) -> Option<Stream> {
        let hook = crate::xport::crypto_hooks().enable?;
        let cell = stream.context.as_ref().and_then(res::ctx_cell_of_lib);
        if let (Some(c), Some(p)) = (&cell, peer_name) {
            crate::streams::set_option(c, b"ssl", b"peer_name", bytes(p));
        }
        let mut stream = stream;
        let r = bridge::with_vm(|vm| hook(vm, &mut stream, true, Some(SSLV23_CLIENT), None));
        if let (Some(c), Some(_)) = (&cell, peer_name) {
            crate::streams::unset_option(c, b"ssl", b"peer_name");
        }
        match r {
            Some(Ok((Value::Bool(true), replacement))) => Some(match replacement {
                Some(new) => {
                    // The TLS stream took over the descriptor.
                    stream.add_flags(php_streams::stream::flags::NO_CLOSE);
                    drop(stream);
                    new
                }
                None => stream,
            }),
            Some(Err(t)) => {
                bridge::set_pending(t);
                None
            }
            _ => None,
        }
    }

    fn invalid_uri_parser(&self) {
        bridge::with_vm(|vm| {
            let f = vm
                .frames
                .iter()
                .rev()
                .find_map(|f| f.builtin_name.as_ref().map(ZStr::to_string_lossy))
                .unwrap_or_default();
            let t = vm.throw(
                "ValueError",
                &format!(
                    "{f}(): Provided stream context has invalid value for the \"uri_parser_class\" option"
                ),
            );
            bridge::set_pending(t);
        });
    }

    fn exception_pending(&self) -> bool {
        bridge::has_pending()
    }

    fn publish_headers(&self, headers: &[Vec<u8>]) {
        let list = header_list(headers);
        LAST_HEADERS.with(|l| *l.borrow_mut() = Some(list.clone()));
        bridge::with_vm(|vm| set_local_var(vm, b"http_response_header", list));
    }
}

/// Runs the context's `notification` callback.
fn notify(ctx: &ContextRef, n: &Notification) {
    let Some(cell) = res::ctx_cell_of_lib(ctx) else {
        return;
    };
    let Some(callback) = cell.notifier.borrow().clone() else {
        return;
    };
    if bridge::has_pending() {
        return;
    }
    let to_long = |v: u64| i64::from_ne_bytes(v.to_ne_bytes());
    let args = vec![
        Value::Int(n.code),
        Value::Int(n.severity),
        n.message.as_ref().map_or(Value::Null, bytes),
        Value::Int(n.message_code),
        Value::Int(to_long(n.bytes_sofar)),
        Value::Int(to_long(n.bytes_max)),
    ];
    bridge::with_vm(|vm| {
        bridge::flush_diags(vm);
        if let Err(t) = vm.call_value(&callback, args) {
            bridge::set_pending(t);
        }
    });
}

/// Installs the wrapper services and the notifier hook.
pub fn install() {
    http::set_env(Rc::new(VmEnv));
    context::set_notify_hook(Some(Rc::new(notify)));
}

/// `get_headers(string $url, bool $associative = false, $context = null)`.
fn get_headers(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let url = path_arg(vm, a, 0)?;
    let assoc = a.bool(1);
    let ctx = res::context_arg(vm, a, 2, false)?;
    let opts = REPORT_ERRORS | STREAM_USE_URL | http::STREAM_ONLY_GET_HEADERS;
    let Some((mut stream, _)) = crate::files::open_stream(vm, a, &url, "r", opts, ctx.as_ref())?
    else {
        return Ok(Value::Bool(false));
    };
    let data = stream.wrapper_data.take();
    bridge::run(vm, || stream.close())?;
    let Some(SValue::Array(items)) = data else {
        return Ok(Value::Bool(false));
    };
    let mut out = ZArray::new();
    for (_, item) in items {
        let SValue::Str(line) = item else {
            continue;
        };
        let colon = if assoc {
            line.iter().position(|&c| c == b':')
        } else {
            None
        };
        let Some(colon) = colon else {
            let _ = out.make_mut().push(bytes(&line));
            continue;
        };
        let mut start = colon + 1;
        while line
            .get(start)
            .is_some_and(|c| c.is_ascii_whitespace() || *c == 0x0b)
        {
            start += 1;
        }
        let key = ArrayKey::from_zstr(&ZStr::from(&line[..colon]));
        let value = bytes(&line[start..]);
        let table = out.make_mut();
        match table.get_mut(&key) {
            None => {
                table.insert(key, value);
            }
            Some(prev) => {
                // Repeated headers become a list.
                if !matches!(prev, Value::Array(_)) {
                    let mut list = ZArray::new();
                    let _ = list.make_mut().push(prev.clone());
                    *prev = Value::Array(list);
                }
                if let Value::Array(list) = prev {
                    let _ = list.make_mut().push(value);
                }
            }
        }
    }
    Ok(Value::Array(out))
}

/// `http_get_last_response_headers(): ?array`.
fn http_get_last_response_headers(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(LAST_HEADERS
        .with(|l| l.borrow().clone())
        .unwrap_or(Value::Null))
}

/// `http_clear_last_response_headers(): void`.
fn http_clear_last_response_headers(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    LAST_HEADERS.with(|l| *l.borrow_mut() = None);
    Ok(Value::Null)
}

/// Request shutdown: forgets the last headers.
pub fn shutdown() {
    LAST_HEADERS.with(|l| *l.borrow_mut() = None);
}

/// Registers the functions.
pub fn register(m: &mut ModuleBuilder<'_>) {
    install();
    m.function_static(
        "get_headers(string $url, bool $associative = false, $context = null): array|false",
        get_headers,
    )
    .function_static(
        "http_get_last_response_headers(): ?array",
        http_get_last_response_headers,
    )
    .function_static(
        "http_clear_last_response_headers(): void",
        http_clear_last_response_headers,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_of_headers() {
        let v = header_list(&[b"HTTP/1.0 200 OK".to_vec(), b"A: b".to_vec()]);
        let Value::Array(a) = v else { panic!() };
        assert_eq!(a.len(), 2);
    }
}
