// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Native methods of the SOAP classes and the global functions.

// Every handler has the `NativeFn` signature, fallible or not.
#![allow(clippy::unnecessary_wraps)]

use std::rc::Rc;

use php_runtime::{Obj, Value, ZArray, ZStr};
use php_soap::objects::Actor;
use php_vm::ext::{Args, VmResult};
use php_vm::object::class_of;
use php_vm::vm::FrameKind;
use php_vm::Vm;

use crate::host::VmHost;

/// Runs a library call with the VM as host and converts its result.
fn with_host<T>(
    vm: &mut Vm,
    f: impl FnOnce(&mut VmHost<'_>) -> php_runtime::RtResult<T>,
) -> VmResult<T> {
    let mut h = VmHost::new(vm);
    let r = f(&mut h);
    if let Some(msg) = crate::host::take_oom() {
        return Err(h.vm.fatal(&msg));
    }
    h.vm_result(r)
}

/// An optional string argument (`None` when missing or null).
fn opt_bytes(a: &Args, n: usize) -> Option<Vec<u8>> {
    match a.opt(n)? {
        Value::Str(s) => Some(s.as_bytes().to_vec()),
        _ => None,
    }
}

// ---- object state --------------------------------------------------------

/// `create_object` of `SoapClient`, `SoapServer` and `Soap\Sdl` (and their
/// subclasses): attaches the extension state.
pub(crate) fn create_state(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    let mut cur = Some(class_of(o).clone());
    while let Some(c) = cur {
        if let Some(s) = php_soap::state::create_state(&c.name.to_string_lossy()) {
            php_vm::object::set_internal::<Rc<php_soap::state::SoapObject>>(o, s);
            break;
        }
        cur = c.parent.as_ref().map(Rc::clone);
    }
    Ok(())
}

/// `Soap\Url::__construct` (`get_constructor` throws).
pub(crate) fn url_construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw("Error", "Cannot directly construct Soap\\Url"))
}

/// `Soap\Sdl::__construct` (`get_constructor` throws).
pub(crate) fn sdl_construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw("Error", "Cannot directly construct Soap\\Sdl"))
}

// ---- functions -----------------------------------------------------------

/// `use_soap_error_handler(bool $enable = true): bool`.
pub(crate) fn use_soap_error_handler(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(php_soap::objects::use_soap_error_handler(
        a.bool(0),
    )))
}

/// `is_soap_fault(mixed $object): bool`.
pub(crate) fn is_soap_fault(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(match a.at(0) {
        Value::Object(o) => class_of(&o).instance_of_name(b"soapfault"),
        _ => false,
    }))
}

// ---- value classes -------------------------------------------------------

/// `SoapParam::__construct(mixed $data, string $name)`.
pub(crate) fn param_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let data = a.at(0);
    let name = a.str(1);
    with_host(vm, |h| {
        php_soap::objects::soap_param_construct(h, &this, &data, name.as_bytes())
    })?;
    Ok(Value::Null)
}

/// `SoapHeader::__construct(...)`.
pub(crate) fn header_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let ns = a.str(0);
    let name = a.str(1);
    let data = a.get(2);
    let must = a.bool(3);
    let actor = match a.opt(4) {
        Some(Value::Str(s)) => Some(Actor::Uri(s.as_bytes().to_vec())),
        Some(Value::Int(i)) => Some(Actor::Code(i)),
        _ => None,
    };
    with_host(vm, |h| {
        php_soap::objects::soap_header_construct(
            h,
            &this,
            ns.as_bytes(),
            name.as_bytes(),
            data.as_ref(),
            must,
            actor,
        )
    })?;
    Ok(Value::Null)
}

/// `SoapFault::__construct(...)`.
pub(crate) fn fault_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let code = a.at(0);
    let string = a.str(1);
    let actor = opt_bytes(a, 2);
    let details = a.opt(3);
    let name = opt_bytes(a, 4);
    let header_fault = a.opt(5);
    with_host(vm, |h| {
        php_soap::fault::construct(
            h,
            &this,
            &code,
            string.as_bytes(),
            actor.as_deref(),
            details.as_ref(),
            name.as_deref(),
            header_fault.as_ref(),
        )
    })?;
    Ok(Value::Null)
}

/// `SoapFault::__toString(): string`.
pub(crate) fn fault_to_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let s = with_host(vm, |h| php_soap::fault::to_string(h, &this))?;
    Ok(Value::from(s.as_slice()))
}

/// `SoapVar::__construct(...)`.
pub(crate) fn var_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let data = a.at(0);
    let encoding = match a.opt(1) {
        Some(Value::Int(i)) => Some(i),
        _ => None,
    };
    let opt_str = |n: usize| match a.opt(n) {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    };
    let (tn, tns, nn, nns) = (opt_str(2), opt_str(3), opt_str(4), opt_str(5));
    with_host(vm, |h| {
        php_soap::objects::soap_var_construct(
            h,
            &this,
            &data,
            encoding,
            tn.as_ref(),
            tns.as_ref(),
            nn.as_ref(),
            nns.as_ref(),
        )
    })?;
    Ok(Value::Null)
}

// ---- SoapServer ----------------------------------------------------------

/// `SoapServer::__construct(?string $wsdl, array $options = [])`.
pub(crate) fn server_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let wsdl = opt_bytes(a, 0);
    let options = a.get(1).map(|_| a.array(1));
    with_host(vm, |h| {
        php_soap::server::construct(h, &this, wsdl.as_deref(), options.as_ref())
    })?;
    Ok(Value::Null)
}

/// `SoapServer::fault(...)`.
pub(crate) fn server_fault(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let code = a.str(0);
    let string = a.str(1);
    let actor = opt_bytes(a, 2);
    let details = a.get(3);
    let name = opt_bytes(a, 4);
    let lang = opt_bytes(a, 5).unwrap_or_default();
    with_host(vm, |h| {
        php_soap::server::fault(
            h,
            &this,
            code.as_bytes(),
            string.as_bytes(),
            actor.as_deref(),
            details.as_ref(),
            name.as_deref(),
            &lang,
        )
    })?;
    Ok(Value::Null)
}

/// `SoapServer::addSoapHeader(SoapHeader $header): void`.
pub(crate) fn server_add_soap_header(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let Value::Object(header) = a.at(0) else {
        return Ok(Value::Null);
    };
    with_host(vm, |h| php_soap::server::add_soap_header(h, &this, header))?;
    Ok(Value::Null)
}

/// `SoapServer::setPersistence(int $mode): void`.
pub(crate) fn server_set_persistence(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let mode = a.int(0);
    with_host(vm, |h| php_soap::server::set_persistence(h, &this, mode))?;
    Ok(Value::Null)
}

/// `SoapServer::setClass(string $class, mixed ...$args): void`.
pub(crate) fn server_set_class(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let name = a.str(0);
    let cls = vm.lookup_class(&name)?;
    let Some(cls) = cls else {
        return Err(a.type_error(
            vm,
            0,
            &format!(
                "must be a valid class name, {} given",
                name.to_string_lossy()
            ),
        ));
    };
    let args: Vec<Value> = a.values.iter().skip(1).map(Value::deref).collect();
    let class = cls.name.clone();
    with_host(vm, |h| php_soap::server::set_class(h, &this, class, args))?;
    Ok(Value::Null)
}

/// `SoapServer::setObject(object $object): void`.
pub(crate) fn server_set_object(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let Value::Object(o) = a.at(0) else {
        return Ok(Value::Null);
    };
    with_host(vm, |h| php_soap::server::set_object(h, &this, o))?;
    Ok(Value::Null)
}

/// `SoapServer::getFunctions(): array`.
pub(crate) fn server_get_functions(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    with_host(vm, |h| php_soap::server::get_functions(h, &this))
}

/// `SoapServer::addFunction($functions): void`.
pub(crate) fn server_add_function(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let f = a.at(0);
    with_host(vm, |h| php_soap::server::add_function(h, &this, &f))?;
    Ok(Value::Null)
}

/// `SoapServer::handle(?string $request = null): void`.
pub(crate) fn server_handle(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let request = opt_bytes(a, 0);
    with_host(vm, |h| {
        php_soap::server::handle(h, &this, request.as_deref())
    })?;
    Ok(Value::Null)
}

/// `SoapServer::__getLastResponse(): ?string`.
pub(crate) fn server_get_last_response(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    with_host(vm, |h| php_soap::server::get_last_response(h, &this))
}

// ---- SoapClient ----------------------------------------------------------

/// `SoapClient::__construct(?string $wsdl, array $options = [])`.
pub(crate) fn client_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let wsdl = opt_bytes(a, 0);
    let options = a.get(1).map(|_| a.array(1));
    with_host(vm, |h| {
        php_soap::client::construct(h, &this, wsdl.as_deref(), options.as_ref())
    })?;
    Ok(Value::Null)
}

/// `SoapClient::__call(string $name, array $args)`, also reached for every
/// undefined method (`$client->someOperation(...)`): the VM then passes
/// the call's own arguments and the name through
/// [`Vm::magic_call_name`].
pub(crate) fn client_call(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let (name, args) = if let Some(n) = vm.magic_call_name() {
        let args = ZArray::from_values(a.values.iter().map(Value::deref));
        // Backtraces show the trampoline as `__call('name', Array)`.
        if let Some(f) = vm.frames.last_mut() {
            if f.kind == FrameKind::Builtin {
                f.args = vec![
                    Some(Value::Str(n.clone())),
                    Some(Value::Array(args.clone())),
                ];
            }
        }
        (n, args)
    } else {
        direct_call_args(vm, a)?
    };
    with_host(vm, |h| {
        php_soap::client::call(h, &this, name.as_bytes(), &args)
    })
}

/// ZPP of a direct `$client->__call($name, $args)` (the method is declared
/// variadic so that magic dispatch reaches it with any arity).
fn direct_call_args(vm: &mut Vm, a: &Args) -> VmResult<(ZStr, ZArray)> {
    if a.values.len() != 2 {
        let msg = format!(
            "SoapClient::__call() expects exactly 2 arguments, {} given",
            a.values.len()
        );
        return Err(vm.throw("ArgumentCountError", &msg));
    }
    let name = match a.values[0].deref() {
        Value::Str(s) => s,
        v @ (Value::Int(_) | Value::Float(_) | Value::Bool(_)) => vm.value_to_string(&v)?,
        other => {
            let msg = format!(
                "SoapClient::__call(): Argument #1 ($name) must be of type string, {} given",
                php_soap::client_type_name(&other)
            );
            return Err(vm.throw("TypeError", &msg));
        }
    };
    let Value::Array(args) = a.values[1].deref() else {
        let msg = format!(
            "SoapClient::__call(): Argument #2 ($args) must be of type array, {} given",
            php_soap::client_type_name(&a.values[1])
        );
        return Err(vm.throw("TypeError", &msg));
    };
    Ok((name, args))
}

/// `SoapClient::__soapCall(...)`.
pub(crate) fn client_soap_call(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let name = a.str(0);
    let args = a.array(1);
    let options = match a.opt(2) {
        Some(Value::Array(o)) => Some(o),
        _ => None,
    };
    let input_headers = a.get(3);
    let want_out = a.has(4);
    let mut out = Value::Null;
    let r = with_host(vm, |h| {
        php_soap::client::soap_call(
            h,
            &this,
            name.as_bytes(),
            &args,
            options.as_ref(),
            input_headers.as_ref(),
            want_out.then_some(&mut out),
        )
    });
    if want_out {
        a.set_ref(4, out);
    }
    r
}

/// `SoapClient::__getFunctions(): ?array`.
pub(crate) fn client_get_functions(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let mut h = VmHost::new(vm);
    Ok(php_soap::client::get_functions(&mut h, &this))
}

/// `SoapClient::__getTypes(): ?array`.
pub(crate) fn client_get_types(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let mut h = VmHost::new(vm);
    Ok(php_soap::client::get_types(&mut h, &this))
}

fn get_last(vm: &mut Vm, a: &Args, which: &str) -> Value {
    let this = a.this_obj();
    let mut h = VmHost::new(vm);
    php_soap::client::get_last(&mut h, &this, which)
}

/// `SoapClient::__getLastRequest(): ?string`.
pub(crate) fn client_get_last_request(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(get_last(vm, a, "__last_request"))
}

/// `SoapClient::__getLastResponse(): ?string`.
pub(crate) fn client_get_last_response(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(get_last(vm, a, "__last_response"))
}

/// `SoapClient::__getLastRequestHeaders(): ?string`.
pub(crate) fn client_get_last_request_headers(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(get_last(vm, a, "__last_request_headers"))
}

/// `SoapClient::__getLastResponseHeaders(): ?string`.
pub(crate) fn client_get_last_response_headers(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(get_last(vm, a, "__last_response_headers"))
}

/// `SoapClient::__doRequest(...)`: the HTTP transport.
pub(crate) fn client_do_request(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let request = a.str(0);
    let location = a.str(1);
    let action = a.str(2);
    let version = a.int(3);
    let one_way = a.bool(4);
    let parser = opt_bytes(a, 5);
    with_host(vm, |h| {
        php_soap::client::do_request_method(
            h,
            &this,
            request.as_bytes(),
            location.as_bytes(),
            action.as_bytes(),
            version,
            one_way,
            parser.as_deref(),
        )
    })
}

/// `SoapClient::__setCookie(string $name, ?string $value = null): void`.
pub(crate) fn client_set_cookie(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let name = a.str(0);
    let value = opt_bytes(a, 1);
    let mut h = VmHost::new(vm);
    php_soap::client::set_cookie(&mut h, &this, name.as_bytes(), value.as_deref());
    Ok(Value::Null)
}

/// `SoapClient::__getCookies(): array`.
pub(crate) fn client_get_cookies(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let mut h = VmHost::new(vm);
    Ok(php_soap::client::get_cookies(&mut h, &this))
}

/// `SoapClient::__setSoapHeaders($headers = null): bool`.
pub(crate) fn client_set_soap_headers(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let headers = a.get(0);
    let r = with_host(vm, |h| {
        php_soap::client::set_soap_headers(h, &this, headers.as_ref())
    })?;
    Ok(Value::Bool(r))
}

/// `SoapClient::__setLocation(?string $location = null): ?string`.
pub(crate) fn client_set_location(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let location = opt_bytes(a, 0);
    let mut h = VmHost::new(vm);
    Ok(php_soap::client::set_location(
        &mut h,
        &this,
        location.as_deref(),
    ))
}
