// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `CurlShareHandle`, `CurlSharePersistentHandle` and `curl_share_*`
//! (share.c).

use crate::easy::{ctor_fn, not_comparable, NO_DYNAMIC_PROPERTIES};
use crate::state::{share_of, CurlShare};
use php_curl::consts::{
    CURLSHOPT_SHARE, CURLSHOPT_UNSHARE, CURL_LOCK_DATA_CONNECT, CURL_LOCK_DATA_COOKIE,
    CURL_LOCK_DATA_DNS, CURL_LOCK_DATA_PSL, CURL_LOCK_DATA_SSL_SESSION,
};
use php_curl::Share;
use php_runtime::{Obj, Value, ZArray};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::{object, Vm};
use std::cell::Cell;
use std::rc::Rc;

/// `CURLSHE_BAD_OPTION`.
const CURLSHE_BAD_OPTION: i32 = 1;

fn share_arg(vm: &mut Vm, a: &Args, n: usize) -> VmResult<(Obj, Rc<CurlShare>)> {
    if let Value::Object(o) = a.at(n) {
        if let Some(s) = share_of(&o) {
            return Ok((o, s));
        }
    }
    Err(vm.throw("Error", "Invalid cURL share handle"))
}

/// Registers both share classes.
pub(crate) fn register_classes(m: &mut ModuleBuilder<'_>) {
    let hooks = ClassHooks {
        compare: Some(not_comparable),
        ..Default::default()
    };
    ClassBuilder::new("CurlShareHandle")
        .flags(
            class_flags::FINAL
                | class_flags::NOT_SERIALIZABLE
                | class_flags::NO_CLONE
                | NO_DYNAMIC_PROPERTIES,
        )
        .method("__construct()", acc::PUBLIC, ctor_fn())
        .hooks(hooks.clone())
        .register(m.vm());
    ClassBuilder::new("CurlSharePersistentHandle")
        .flags(
            class_flags::FINAL
                | class_flags::NOT_SERIALIZABLE
                | class_flags::NO_CLONE
                | NO_DYNAMIC_PROPERTIES,
        )
        .prop("options", acc::PUBLIC | acc::READONLY, Some("array"), None)
        .method("__construct()", acc::PUBLIC, ctor_fn())
        .hooks(hooks)
        .register(m.vm());
}

fn new_share_obj(vm: &mut Vm, class: &str, share: Share) -> VmResult<Obj> {
    let Some(cls) = vm.find_class(class.as_bytes()) else {
        return Err(vm.throw("Error", &format!("Class \"{class}\" not found")));
    };
    let o = vm.new_object(&cls)?;
    object::set_internal(
        &o,
        Rc::new(CurlShare {
            share,
            err_no: Cell::new(0),
        }),
    );
    Ok(o)
}

fn curl_share_init(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let Some(share) = Share::new() else {
        return Err(vm.throw(
            "Error",
            "curl_share_init(): Could not initialize a new cURL share handle",
        ));
    };
    Ok(Value::Object(new_share_obj(vm, "CurlShareHandle", share)?))
}

fn curl_share_close(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    share_arg(vm, a, 0)?;
    Ok(Value::Null)
}

fn curl_share_setopt(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, s) = share_arg(vm, a, 0)?;
    let opt = a.int(1);
    let rc = match opt {
        CURLSHOPT_SHARE | CURLSHOPT_UNSHARE => {
            let v = vm.value_to_int(&a.at(2))?;
            s.share.setopt(opt, v)
        }
        _ => {
            s.err_no.set(CURLSHE_BAD_OPTION);
            return Err(a.value_error(vm, 2, "is not a valid cURL share option"));
        }
    };
    s.err_no.set(rc);
    Ok(Value::Bool(rc == 0))
}

fn curl_share_errno(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, s) = share_arg(vm, a, 0)?;
    Ok(Value::Int(i64::from(s.err_no.get())))
}

fn curl_share_strerror(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::from(php_curl::share::strerror(a.int(0))))
}

fn curl_share_init_persistent(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let opts = a.array(0);
    if opts.is_empty() {
        return Err(a.value_error(vm, 1, "must not be empty"));
    }
    let mut id: u64 = 0;
    for v in opts.values() {
        let v = v.deref();
        let n = match &v {
            Value::Int(i) => Some(*i),
            Value::Null => Some(0),
            Value::Bool(b) => Some(i64::from(*b)),
            Value::Float(_) | Value::Str(_) => vm.parse_long_weak(&v)?,
            _ => None,
        };
        let Some(n) = n else {
            return Err(a.type_error(
                vm,
                1,
                &format!("must contain only int values, {} given", v.value_name()),
            ));
        };
        id |= match n {
            CURL_LOCK_DATA_COOKIE => {
                return Err(a.value_error(
                    vm,
                    1,
                    "must not contain CURL_LOCK_DATA_COOKIE because sharing cookies across PHP requests is unsafe",
                ));
            }
            CURL_LOCK_DATA_DNS => 1,
            CURL_LOCK_DATA_SSL_SESSION => 2,
            CURL_LOCK_DATA_CONNECT => 4,
            CURL_LOCK_DATA_PSL => 8,
            _ => return Err(a.value_error(vm, 1, "must contain only CURL_LOCK_DATA_* constants")),
        };
    }
    let mut list = Vec::new();
    for (bit, data) in [
        (1, CURL_LOCK_DATA_DNS),
        (2, CURL_LOCK_DATA_SSL_SESSION),
        (4, CURL_LOCK_DATA_CONNECT),
        (8, CURL_LOCK_DATA_PSL),
    ] {
        if id & bit != 0 {
            list.push(data);
        }
    }
    let share = match Share::persistent(id, &list) {
        Ok(s) => s,
        Err(rc) => {
            return Err(vm.throw(
                "Exception",
                &format!(
                    "Could not construct persistent cURL share handle: {}",
                    php_curl::share::strerror(i64::from(rc))
                ),
            ));
        }
    };
    let o = new_share_obj(vm, "CurlSharePersistentHandle", share)?;
    let mut arr = ZArray::new();
    for d in list {
        let _ = arr.make_mut().push(Value::Int(d));
    }
    vm.set_property_raw(&o, "options", Value::Array(arr));
    Ok(Value::Object(o))
}

/// Registers the share functions.
pub(crate) fn register_functions(m: &mut ModuleBuilder<'_>) {
    m.function_static(
        "curl_share_close(CurlShareHandle $share_handle): void",
        curl_share_close,
    )
    .deprecated("8.5", "as it has no effect since PHP 8.0")
    .function_static(
        "curl_share_errno(CurlShareHandle $share_handle): int",
        curl_share_errno,
    )
    .function_static("curl_share_init(): CurlShareHandle", curl_share_init)
    .function_static(
        "curl_share_setopt(CurlShareHandle $share_handle, int $option, mixed $value): bool",
        curl_share_setopt,
    )
    .function_static(
        "curl_share_strerror(int $error_code): ?string",
        curl_share_strerror,
    )
    .function_static(
        "curl_share_init_persistent(array $share_options): CurlSharePersistentHandle",
        curl_share_init_persistent,
    );
}
