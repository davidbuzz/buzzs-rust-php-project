//! `CurlMultiHandle` and `curl_multi_*` (multi.c).

use crate::dispatch::{verify_handlers, Dispatch};
use crate::easy::{cast_hook, cleanup_handle, ctor_fn, handle_arg, not_comparable, NO_DYNAMIC_PROPERTIES};
use crate::state::{curl_of, multi_of, CurlMulti, Method};
use php_curl::consts::{
    CURLMOPT_CHUNK_LENGTH_PENALTY_SIZE, CURLMOPT_CONTENT_LENGTH_PENALTY_SIZE, CURLMOPT_MAXCONNECTS,
    CURLMOPT_MAX_CONCURRENT_STREAMS, CURLMOPT_MAX_HOST_CONNECTIONS, CURLMOPT_MAX_PIPELINE_LENGTH,
    CURLMOPT_MAX_TOTAL_CONNECTIONS, CURLMOPT_PIPELINING, CURLMOPT_PUSHFUNCTION,
};
use php_curl::Multi;
use php_runtime::{Obj, Value, ZArray};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::{object, Vm};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// `CURLM_UNKNOWN_OPTION`.
const CURLM_UNKNOWN_OPTION: i32 = 6;

fn multi_arg(vm: &mut Vm, a: &Args, n: usize) -> VmResult<(Obj, Rc<CurlMulti>)> {
    if let Value::Object(o) = a.at(n) {
        if let Some(m) = multi_of(&o) {
            return Ok((o, m));
        }
    }
    Err(vm.throw("Error", "Invalid cURL multi handle"))
}

/// Registers `CurlMultiHandle`.
pub(crate) fn register_class(m: &mut ModuleBuilder<'_>) {
    ClassBuilder::new("CurlMultiHandle")
        .flags(class_flags::FINAL | class_flags::NOT_SERIALIZABLE | class_flags::NO_CLONE | NO_DYNAMIC_PROPERTIES)
        .method("__construct()", acc::PUBLIC, ctor_fn())
        .hooks(ClassHooks {
            cast: Some(cast_hook),
            compare: Some(not_comparable),
            ..Default::default()
        })
        .register(m.vm());
}

fn curl_multi_init(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let Some(multi) = Multi::new() else {
        return Err(vm.throw("Error", "curl_multi_init(): Could not initialize a new cURL multi handle"));
    };
    let Some(cls) = vm.find_class(b"CurlMultiHandle") else {
        return Err(vm.throw("Error", "Class \"CurlMultiHandle\" not found"));
    };
    let o = vm.new_object(&cls)?;
    object::set_internal(
        &o,
        Rc::new(CurlMulti {
            multi,
            easyh: RefCell::new(Vec::new()),
            push: RefCell::new(None),
            err_no: Cell::new(0),
        }),
    );
    Ok(Value::Object(o))
}

fn curl_multi_add_handle(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, m) = multi_arg(vm, a, 0)?;
    let (o, c) = handle_arg(vm, a, 1)?;
    verify_handlers(vm, &c, true)?;
    cleanup_handle(&c);
    let rc = m.multi.add(&c.easy);
    m.err_no.set(rc);
    if rc == 0 {
        m.easyh.borrow_mut().push(o);
    }
    Ok(Value::Int(i64::from(rc)))
}

fn curl_multi_remove_handle(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, m) = multi_arg(vm, a, 0)?;
    let (o, c) = handle_arg(vm, a, 1)?;
    let rc = m.multi.remove(&c.easy);
    m.err_no.set(rc);
    if rc == 0 {
        let mut l = m.easyh.borrow_mut();
        if let Some(p) = l.iter().position(|x| Obj::ptr_eq(x, &o)) {
            l.remove(p);
        }
    }
    Ok(Value::Int(i64::from(rc)))
}

fn curl_multi_get_handles(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, m) = multi_arg(vm, a, 0)?;
    let mut arr = ZArray::new();
    for o in m.easyh.borrow().iter() {
        let _ = arr.make_mut().push(Value::Object(o.clone()));
    }
    Ok(Value::Array(arr))
}

fn curl_multi_select(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, m) = multi_arg(vm, a, 0)?;
    let timeout = if a.has(1) { a.float(1) } else { 1.0 };
    let max = f64::from(i32::MAX) / 1000.0;
    if !(timeout >= 0.0 && timeout <= max) {
        return Err(a.value_error(vm, 2, &format!("must be between 0 and {max:.6}")));
    }
    #[allow(clippy::cast_possible_truncation)]
    let ms = (timeout * 1000.0) as i32;
    match m.multi.wait(ms) {
        Ok(n) => Ok(Value::Int(i64::from(n))),
        Err(rc) => {
            m.err_no.set(rc);
            Ok(Value::Int(-1))
        }
    }
}

fn curl_multi_exec(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (mo, m) = multi_arg(vm, a, 0)?;
    let handles: Vec<Obj> = m.easyh.borrow().clone();
    for h in &handles {
        if let Some(c) = curl_of(h) {
            verify_handlers(vm, &c, true)?;
        }
    }
    let cur = a.at(1);
    // `zval_try_get_long()`.
    let running = match &cur {
        Value::Int(i) => Some(*i),
        Value::Null => Some(0),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Float(_) | Value::Str(_) => vm.parse_long_weak(&cur)?,
        _ => None,
    };
    let Some(running) = running else {
        return Err(a.type_error(vm, 2, &format!("must be of type int, {} given", cur.value_name())));
    };
    let (rc, still, pending) = {
        let mut d = Dispatch::new(vm, &handles).with_multi(mo, Rc::clone(&m));
        #[allow(clippy::cast_possible_truncation)]
        let (rc, still) = m.multi.perform(&mut d, running as i32);
        (rc, still, d.pending.take())
    };
    a.set_ref(1, Value::Int(i64::from(still)));
    m.err_no.set(rc);
    if let Some(t) = pending {
        return Err(t);
    }
    Ok(Value::Int(i64::from(rc)))
}

fn curl_multi_getcontent(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c) = handle_arg(vm, a, 0)?;
    let st = c.st.borrow();
    if st.write.method == Method::Return {
        return Ok(Value::from(st.write.buf.as_deref().unwrap_or_default()));
    }
    Ok(Value::Null)
}

fn curl_multi_info_read(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, m) = multi_arg(vm, a, 0)?;
    let Some(msg) = m.multi.info_read() else {
        return Ok(Value::Bool(false));
    };
    if a.has(1) {
        a.set_ref(1, Value::Int(i64::from(msg.queued)));
    }
    let mut r = ZArray::new();
    r.make_mut().insert("msg", Value::Int(i64::from(msg.msg)));
    r.make_mut().insert("result", Value::Int(i64::from(msg.result)));
    if let Some(key) = msg.easy {
        let found = m
            .easyh
            .borrow()
            .iter()
            .find(|o| curl_of(o).is_some_and(|c| c.easy.key() == key))
            .cloned();
        if let Some(o) = found {
            if let Some(c) = curl_of(&o) {
                c.err_no.set(msg.result);
            }
            r.make_mut().insert("handle", Value::Object(o));
        }
    }
    Ok(Value::Array(r))
}

fn curl_multi_close(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, m) = multi_arg(vm, a, 0)?;
    let handles: Vec<Obj> = m.easyh.borrow().clone();
    for h in &handles {
        if let Some(c) = curl_of(h) {
            verify_handlers(vm, &c, true)?;
            m.multi.remove(&c.easy);
        }
    }
    m.easyh.borrow_mut().clear();
    Ok(Value::Null)
}

fn curl_multi_errno(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, m) = multi_arg(vm, a, 0)?;
    Ok(Value::Int(i64::from(m.err_no.get())))
}

fn curl_multi_strerror(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::from(php_curl::multi::strerror(a.int(0))))
}

fn curl_multi_setopt(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, m) = multi_arg(vm, a, 0)?;
    let opt = a.int(1);
    let value = a.at(2);
    let rc = match opt {
        CURLMOPT_PIPELINING
        | CURLMOPT_MAXCONNECTS
        | CURLMOPT_CHUNK_LENGTH_PENALTY_SIZE
        | CURLMOPT_CONTENT_LENGTH_PENALTY_SIZE
        | CURLMOPT_MAX_HOST_CONNECTIONS
        | CURLMOPT_MAX_PIPELINE_LENGTH
        | CURLMOPT_MAX_TOTAL_CONNECTIONS
        | CURLMOPT_MAX_CONCURRENT_STREAMS => {
            let l = vm.value_to_int(&value)?;
            if opt == CURLMOPT_PIPELINING && (l & 1) != 0 {
                vm.php_warning("CURLPIPE_HTTP1 is no longer supported")?;
            }
            m.multi.set_long(opt, l)
        }
        CURLMOPT_PUSHFUNCTION => {
            *m.push.borrow_mut() = None;
            if !vm.is_callable(&value) {
                let why = vm.callable_error_message(&value);
                return Err(a.type_error(
                    vm,
                    2,
                    &format!("must be a valid callback for option CURLMOPT_PUSHFUNCTION, {why}"),
                ));
            }
            *m.push.borrow_mut() = Some(value);
            let rc = m.multi.enable_push();
            if rc != 0 {
                return Ok(Value::Bool(false));
            }
            rc
        }
        _ => {
            m.err_no.set(CURLM_UNKNOWN_OPTION);
            return Err(a.value_error(vm, 2, "is not a valid cURL multi option"));
        }
    };
    m.err_no.set(rc);
    Ok(Value::Bool(rc == 0))
}

/// Registers the multi functions.
pub(crate) fn register_functions(m: &mut ModuleBuilder<'_>) {
    m.function(
        "curl_multi_setopt(CurlMultiHandle $multi_handle, int $option, mixed $value): bool",
        curl_multi_setopt,
    )
    .function(
        "curl_multi_add_handle(CurlMultiHandle $multi_handle, CurlHandle $handle): int",
        curl_multi_add_handle,
    )
    .function("curl_multi_get_handles(CurlMultiHandle $multi_handle): array", curl_multi_get_handles)
    .function("curl_multi_close(CurlMultiHandle $multi_handle): void", curl_multi_close)
    .function("curl_multi_errno(CurlMultiHandle $multi_handle): int", curl_multi_errno)
    .function("curl_multi_exec(CurlMultiHandle $multi_handle, &$still_running): int", curl_multi_exec)
    .function("curl_multi_getcontent(CurlHandle $handle): ?string", curl_multi_getcontent)
    .function(
        "curl_multi_info_read(CurlMultiHandle $multi_handle, &$queued_messages = null): array|false",
        curl_multi_info_read,
    )
    .function("curl_multi_init(): CurlMultiHandle", curl_multi_init)
    .function(
        "curl_multi_remove_handle(CurlMultiHandle $multi_handle, CurlHandle $handle): int",
        curl_multi_remove_handle,
    )
    .function("curl_multi_select(CurlMultiHandle $multi_handle, float $timeout = 1.0): int", curl_multi_select)
    .function("curl_multi_strerror(int $error_code): ?string", curl_multi_strerror);
}
