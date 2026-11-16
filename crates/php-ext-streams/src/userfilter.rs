// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/standard/user_filters.c`: `php_user_filter`, `StreamBucket`, the
//! bucket functions and `stream_filter_*`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::{Rc, Weak};

use php_runtime::{Obj, PhpRef, Resource, Value, ZArray};
use php_streams::filter::user::{UserFilterClass, UserFilterFactory, UserFilterObject};
use php_streams::filter::{Brigade, Bucket, ChainKind, FilterId};
use php_streams::value::{UserCall, Value as SValue};
use php_streams::{file as lib, wrapper};
use php_vm::class::Class;
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ModuleBuilder, VmResult};
use php_vm::{Throw, Vm};

use crate::bridge::{self, bytes};
use crate::res::{self, fname, str_arg, stream_arg, StreamCell};
use crate::userwrap;

/// Resource type of brigades.
const BRIGADE: &str = "userfilter.bucket brigade";
/// Resource type of buckets.
const BUCKET: &str = "userfilter.bucket";
/// Resource type of attached filters.
const FILTER: &str = "stream filter";

type SharedBrigade = Rc<RefCell<Brigade>>;
type SharedBucket = Rc<RefCell<Bucket>>;

thread_local! {
    static BUCKET_CLASS: RefCell<Option<Rc<Class>>> = const { RefCell::new(None) };
    /// The VM value of the `$params` argument of the filter being created.
    static PENDING_PARAMS: RefCell<Option<Value>> = const { RefCell::new(None) };
    /// The last `Bucket::tag` handed out.
    static BUCKET_TAG: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// The brigade each tagged bucket was last attached to
    /// (`bucket->brigade`).
    static BUCKET_HOME: RefCell<std::collections::HashMap<u64, Weak<RefCell<Brigade>>>> =
        RefCell::new(std::collections::HashMap::new());
    /// Generation of `BG(user_filter_map)`: bumped when the map is
    /// destroyed at request shutdown (`PHP_RSHUTDOWN(user_filters)`), so
    /// classes registered before that can no longer be instantiated while
    /// the resource list is closed.
    static MAP_EPOCH: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// `PHP_RSHUTDOWN_FUNCTION(user_filters)`: destroys `BG(user_filter_map)`.
/// The filter factories stay registered (`FG(stream_filters)` lives until
/// the end of `php_request_shutdown()`), so a user filter appended while
/// the remaining streams are closed fails with "Unable to create or locate
/// filter" (GH-22449, GH-22818).
pub fn shutdown() {
    MAP_EPOCH.with(|e| e.set(e.get().wrapping_add(1)));
}

/// Whether a class registered in map generation `epoch` is still in
/// `BG(user_filter_map)`.
fn in_user_filter_map(epoch: u64) -> bool {
    MAP_EPOCH.with(std::cell::Cell::get) == epoch
}

/// A filter attached with `stream_filter_append()`/`prepend()`.
#[derive(Debug)]
struct FilterRes {
    stream: Weak<StreamCell>,
    id: FilterId,
}

#[derive(Debug)]
struct FilterClass {
    class: Rc<Class>,
    /// The `BG(user_filter_map)` generation the class was registered in.
    epoch: u64,
}

#[derive(Debug)]
struct FilterObject {
    obj: Obj,
}

impl UserFilterClass for FilterClass {
    fn instantiate(
        &self,
        filtername: &str,
        params: Option<&SValue>,
    ) -> Option<(Box<dyn UserFilterObject>, UserCall)> {
        let vm_params = PENDING_PARAMS.with(|p| p.borrow_mut().take());
        // `BG(user_filter_map) == NULL`: destroyed at request shutdown.
        if !in_user_filter_map(self.epoch) {
            return None;
        }
        bridge::with_vm(|vm| {
            bridge::flush_diags(vm);
            if bridge::has_pending() {
                return None;
            }
            let obj = match vm.new_object(&self.class) {
                Ok(o) => o,
                Err(t) => {
                    bridge::set_pending(t);
                    return None;
                }
            };
            let p = vm_params.unwrap_or_else(|| params.map_or(Value::Null, bridge::from_lib));
            // add_property_string()/add_property_zval(): a failure leaves
            // the exception pending but the filter is still created.
            if let Err(t) = vm.set_property(&obj, "filtername", bytes(filtername)) {
                bridge::set_pending(t);
            }
            if let Err(t) = vm.set_property(&obj, "params", p) {
                set_pending_chained(vm, t);
            }
            Some(obj)
        })
        .flatten()
        .map(|obj| {
            // zend_call_method_if_exists() does nothing with an exception
            // pending.
            let created = if bridge::has_pending() {
                UserCall::Returned(SValue::Null)
            } else {
                userwrap::call_method(&obj, "onCreate", &mut [], &[])
            };
            let created = match created {
                UserCall::NotImplemented => UserCall::Returned(SValue::Null),
                c => c,
            };
            (
                Box::new(FilterObject { obj }) as Box<dyn UserFilterObject>,
                created,
            )
        })
    }
}

/// Records `t` like `zend_throw_exception_internal()`: an exception thrown
/// while another one is pending gets it as its `previous`.
fn set_pending_chained(vm: &mut Vm, t: Throw) {
    match (bridge::take_pending(), t) {
        (Some(Throw::Exception(old)), Throw::Exception(new)) => {
            vm.chain_exception(&new, &old);
            bridge::set_pending(Throw::Exception(new));
        }
        (Some(old), _) => bridge::set_pending(old),
        (None, t) => bridge::set_pending(t),
    }
}

/// `zend_update_property_ex()` with `EG(fake_scope)` = the filter class.
/// Without a PHP frame to carry the scope (request shutdown) the slot is
/// written directly.
fn set_stream_property(vm: &mut Vm, obj: &Obj, v: Value, scope: &Rc<Class>) -> VmResult<()> {
    if vm
        .frames
        .iter()
        .all(|f| f.kind == php_vm::vm::FrameKind::Builtin)
    {
        vm.set_property_raw(obj, "stream", v);
        return Ok(());
    }
    vm.set_property_scoped(obj, "stream", v, scope)
}

fn new_brigade(b: Brigade) -> (Resource, SharedBrigade) {
    let shared = Rc::new(RefCell::new(b));
    let r = Resource::new(BRIGADE, Some(Box::new(Rc::clone(&shared))));
    (r, shared)
}

impl UserFilterObject for FilterObject {
    fn filter(
        &mut self,
        stream_id: u64,
        input: &mut Brigade,
        output: &mut Brigade,
        consumed: &mut Option<i64>,
        closing: bool,
    ) -> UserCall {
        let obj = self.obj.clone();
        let (in_res, in_b) = new_brigade(std::mem::take(input));
        let (out_res, out_b) = new_brigade(std::mem::take(output));
        let r = bridge::with_vm(|vm| {
            bridge::flush_diags(vm);
            if bridge::has_pending() {
                return UserCall::Threw;
            }
            // userfilter_assign_stream(): only an existing `stream`
            // property is assigned, from the class scope, honouring its
            // type.
            let scope = php_vm::object::class_of(&obj).clone();
            let has_stream = vm
                .call_value(
                    &bytes("property_exists"),
                    vec![Value::Object(obj.clone()), bytes("stream")],
                )
                .is_ok_and(|v| matches!(v, Value::Bool(true)));
            if has_stream {
                let stream = res::filter_stream_value(stream_id);
                if let Err(t) = set_stream_property(vm, &obj, stream, &scope) {
                    bridge::set_pending(t);
                    return UserCall::Threw;
                }
            }
            let class_name = php_vm::object::display_name(&scope.name).to_string_lossy();
            if !userwrap::method_exists(vm, &obj, "filter") {
                // call_user_function() on a missing or inaccessible method.
                let why = match scope.methods.get(&php_runtime::ZStr::from("filter")) {
                    Some(m) => {
                        let vis = if m.flags & acc::PRIVATE != 0 {
                            "private"
                        } else {
                            "protected"
                        };
                        format!(
                            "cannot access {vis} method {class_name}::{}()",
                            m.name.to_string_lossy()
                        )
                    }
                    None => format!("class {class_name} does not have a method \"filter\""),
                };
                let msg = format!("Invalid callback {class_name}::filter, {why}");
                if vm
                    .frames
                    .iter()
                    .all(|f| f.kind == php_vm::vm::FrameKind::Builtin)
                {
                    // At shutdown (no stack frame) zend_call_function()
                    // reports an E_ERROR instead of throwing.
                    let _ = vm.raise(1, &msg);
                } else {
                    let t = vm.throw("Error", &msg);
                    bridge::set_pending(t);
                }
                return UserCall::Threw;
            }
            let c = PhpRef::new(consumed.map_or(Value::Null, Value::Int));
            let args = vec![
                Value::Resource(in_res.clone()),
                Value::Resource(out_res.clone()),
                Value::Reference(c.clone()),
                Value::Bool(closing),
            ];
            let r = match vm.call_method_by_name(&obj, "filter", args) {
                // convert_to_long(&retval) (warns for objects).
                Ok(v) => match vm.value_to_int(&v) {
                    Ok(i) => UserCall::Returned(SValue::Int(i)),
                    Err(t) => {
                        bridge::set_pending(t);
                        UserCall::Threw
                    }
                },
                Err(t) => {
                    bridge::set_pending(t);
                    UserCall::Threw
                }
            };
            if consumed.is_some() {
                *consumed = Some(match c.get() {
                    Value::Int(i) => i,
                    v => vm.value_to_int(&v).unwrap_or(0),
                });
            }
            if has_stream && !bridge::has_pending() {
                let _ = set_stream_property(vm, &obj, Value::Null, &scope);
            }
            r
        })
        .unwrap_or(UserCall::Threw);
        *input = std::mem::take(&mut *in_b.borrow_mut());
        *output = std::mem::take(&mut *out_b.borrow_mut());
        in_res.close();
        out_res.close();
        r
    }

    fn has_seek(&self) -> bool {
        bridge::with_vm(|vm| userwrap::method_exists(vm, &self.obj, "seek")).unwrap_or(false)
    }

    fn seek(&mut self, _stream_id: u64, offset: i64, whence: i32, chain: ChainKind) -> UserCall {
        let mut args = [
            SValue::Int(offset),
            SValue::Int(i64::from(whence)),
            SValue::Int(chain.as_long()),
        ];
        userwrap::call_method(&self.obj, "seek", &mut args, &[])
    }

    fn on_close(&mut self) {
        let _ = userwrap::call_method(&self.obj, "onClose", &mut [], &[]);
    }
}

// ---------------------------------------------------------------------------
// Functions
// ---------------------------------------------------------------------------

fn fetch_brigade(vm: &mut Vm, a: &Args, n: usize) -> VmResult<SharedBrigade> {
    let r = res::resource_arg(vm, a, n)?;
    let b = if r.is_closed() {
        None
    } else {
        r.data()
            .as_ref()
            .and_then(|d| d.downcast_ref::<SharedBrigade>())
            .cloned()
    };
    b.ok_or_else(|| {
        vm.throw(
            "TypeError",
            &format!(
                "{}(): supplied resource is not a valid {BRIGADE} resource",
                fname(a)
            ),
        )
    })
}

/// A fresh bucket identity (`Bucket::tag`).
fn next_tag() -> u64 {
    BUCKET_TAG.with(|t| {
        let v = t.get() + 1;
        t.set(v);
        v
    })
}

fn bucket_object(vm: &mut Vm, mut bucket: Bucket) -> VmResult<Value> {
    let Some(cls) = BUCKET_CLASS.with(|c| c.borrow().clone()) else {
        return Ok(Value::Null);
    };
    bucket.tag = next_tag();
    let len = i64::try_from(bucket.data.len()).unwrap_or(i64::MAX);
    let data = bytes(&bucket.data);
    let shared: SharedBucket = Rc::new(RefCell::new(bucket));
    let r = Resource::new(BUCKET, Some(Box::new(shared)));
    let o = vm.new_object(&cls)?;
    vm.set_property_raw(&o, "bucket", Value::Resource(r));
    vm.set_property_raw(&o, "data", data);
    vm.set_property_raw(&o, "datalen", Value::Int(len));
    vm.set_property_raw(&o, "dataLength", Value::Int(len));
    Ok(Value::Object(o))
}

fn stream_bucket_make_writeable(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let b = fetch_brigade(vm, a, 0)?;
    let bucket = b.borrow_mut().pop_front();
    match bucket {
        Some(bk) => bucket_object(vm, bk),
        None => Ok(Value::Null),
    }
}

fn attach(vm: &mut Vm, a: &Args, append: bool) -> VmResult<Value> {
    let b = fetch_brigade(vm, a, 0)?;
    let Value::Object(o) = a.at(1) else {
        return Ok(Value::Null);
    };
    let bucket_v = vm.get_property(&o, "bucket")?.deref();
    let shared = match &bucket_v {
        Value::Resource(r) if !r.is_closed() => r
            .data()
            .as_ref()
            .and_then(|d| d.downcast_ref::<SharedBucket>())
            .cloned(),
        _ => None,
    };
    let Some(shared) = shared else {
        if !matches!(bucket_v, Value::Resource(_)) {
            return Err(a.type_error(
                vm,
                2,
                &format!(
                    "must be of type resource, {} given",
                    bridge::given_type(&bucket_v)
                ),
            ));
        }
        return Err(vm.throw(
            "TypeError",
            &format!(
                "{}(): supplied resource is not a valid {BUCKET} resource",
                fname(a)
            ),
        ));
    };
    if let Value::Str(s) = vm.get_property(&o, "data")?.deref() {
        shared.borrow_mut().data = s.as_bytes().to_vec();
    }
    let bucket = shared.borrow().clone();
    // A bucket already on a brigade is unlinked first (the same
    // php_stream_bucket may be attached through clones of its object).
    let previous = BUCKET_HOME.with(|h| h.borrow().get(&bucket.tag).and_then(Weak::upgrade));
    if let Some(prev) = previous {
        if let Ok(mut prev) = prev.try_borrow_mut() {
            if let Some(i) = prev.iter().position(|x| x.tag == bucket.tag) {
                prev.remove(i);
            }
        }
    }
    let tag = bucket.tag;
    {
        let mut brigade = b.borrow_mut();
        if let Some(i) = brigade.iter().position(|x| x.tag == tag) {
            brigade.remove(i);
        }
        if append {
            brigade.push_back(bucket);
        } else {
            brigade.push_front(bucket);
        }
    }
    BUCKET_HOME.with(|h| {
        let mut h = h.borrow_mut();
        h.retain(|_, w| w.strong_count() > 0);
        h.insert(tag, Rc::downgrade(&b));
    });
    Ok(Value::Null)
}

fn stream_bucket_append(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    attach(vm, a, true)
}

fn stream_bucket_prepend(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    attach(vm, a, false)
}

fn stream_bucket_new(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let _ = stream_arg(vm, a, 0)?;
    let data = str_arg(a, 1);
    bucket_object(vm, Bucket::new(data))
}

fn stream_get_filters(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let mut arr = ZArray::new();
    for n in wrapper::filter_names() {
        let _ = arr.make_mut().push(bytes(n));
    }
    Ok(Value::Array(arr))
}

fn stream_filter_register(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0).to_string_lossy();
    let cname = a.str(1);
    let Some(class) = vm.lookup_class(&cname)? else {
        return Err(a.type_error(
            vm,
            2,
            &format!(
                "must be a valid class name, \"{}\" given",
                cname.to_string_lossy()
            ),
        ));
    };
    if name.is_empty() {
        return Err(a.value_error(vm, 1, "must be a non-empty string"));
    }
    if class.is_interface() || class.is_trait() || class.is_abstract() || class.is_enum() {
        return Err(a.value_error(vm, 2, "must be a concrete class"));
    }
    let epoch = MAP_EPOCH.with(std::cell::Cell::get);
    let factory = Rc::new(UserFilterFactory::new(Rc::new(FilterClass {
        class,
        epoch,
    })));
    Ok(Value::Bool(wrapper::register_filter_volatile(
        &name, factory,
    )))
}

fn apply(vm: &mut Vm, a: &Args, append: bool) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let name = a.str(1).to_string_lossy();
    let rw = a.int(2);
    let params = a.get(3);
    let lib_params = params.as_ref().map(bridge::to_lib);
    PENDING_PARAMS.with(|p| p.borrow_mut().clone_from(&params));
    let r = bridge::run(vm, || {
        let mut s = cell.get()?;
        let read_write = if rw & lib::STREAM_FILTER_ALL == 0 {
            let mut x = 0;
            if s.mode.contains(['r', '+']) {
                x |= lib::STREAM_FILTER_READ;
            }
            if s.mode.contains(['w', '+', 'a']) {
                x |= lib::STREAM_FILTER_WRITE;
            }
            x
        } else {
            rw
        };
        let mut last = None;
        for (bit, kind) in [
            (lib::STREAM_FILTER_READ, ChainKind::Read),
            (lib::STREAM_FILTER_WRITE, ChainKind::Write),
        ] {
            if read_write & bit == 0 {
                continue;
            }
            if params.is_some() {
                PENDING_PARAMS.with(|p| p.borrow_mut().clone_from(&params));
            }
            let f = wrapper::create_filter(&name, lib_params.as_ref(), cell.persistent)?;
            last = Some(if append {
                s.append_filter(kind, f)?
            } else {
                s.prepend_filter(kind, f)
            });
        }
        last
    })?;
    PENDING_PARAMS.with(|p| *p.borrow_mut() = None);
    Ok(match r {
        Some(id) => Value::Resource(Resource::new(
            FILTER,
            Some(Box::new(FilterRes {
                stream: Rc::downgrade(&cell),
                id,
            })),
        )),
        None => Value::Bool(false),
    })
}

fn stream_filter_append(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    apply(vm, a, true)
}

fn stream_filter_prepend(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    apply(vm, a, false)
}

fn stream_filter_remove(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = res::resource_arg(vm, a, 0)?;
    let found = if r.is_closed() {
        None
    } else {
        r.data()
            .as_ref()
            .and_then(|d| d.downcast_ref::<FilterRes>())
            .map(|f| (f.stream.upgrade(), f.id))
    };
    let invalid = |vm: &mut Vm| {
        vm.throw(
            "TypeError",
            &format!(
                "{}(): supplied resource is not a valid {FILTER} resource",
                fname(a)
            ),
        )
    };
    let Some((Some(cell), id)) = found else {
        return Err(invalid(vm));
    };
    if cell.is_closed() {
        return Err(invalid(vm));
    }
    let ok = bridge::run(vm, || {
        cell.get()
            .is_some_and(|mut s| lib::stream_filter_remove(&mut s, id))
    })?;
    if ok {
        r.close();
    }
    Ok(Value::Bool(ok))
}

fn filter_default(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(0))
}

fn on_create_default(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(true))
}

fn seek_default(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(true))
}

fn on_close_default(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

/// Registers the classes, functions and `PSFS_*` constants.
pub fn register(m: &mut ModuleBuilder<'_>) {
    for (n, v) in [
        ("PSFS_PASS_ON", 2),
        ("PSFS_FEED_ME", 1),
        ("PSFS_ERR_FATAL", 0),
        ("PSFS_FLAG_NORMAL", 0),
        ("PSFS_FLAG_FLUSH_INC", 1),
        ("PSFS_FLAG_FLUSH_CLOSE", 2),
    ] {
        m.constant(n, Value::Int(v));
    }
    ClassBuilder::new("php_user_filter")
        .prop("filtername", acc::PUBLIC, Some("string"), Some(bytes("")))
        .prop("params", acc::PUBLIC, Some("mixed"), Some(bytes("")))
        .prop("stream", acc::PUBLIC, None, Some(Value::Null))
        .method(
            "filter($in, $out, &$consumed, bool $closing): int",
            acc::PUBLIC,
            filter_default,
        )
        .method(
            "seek(int $offset, int $whence, int $chain): bool",
            acc::PUBLIC,
            seek_default,
        )
        .method("onCreate(): bool", acc::PUBLIC, on_create_default)
        .method("onClose(): void", acc::PUBLIC, on_close_default)
        .register(m.vm());
    let bucket = ClassBuilder::new("StreamBucket")
        .flags(class_flags::FINAL)
        // `@readonly` in the stub is documentation only: user filters
        // assign `$bucket->data` (user_filters_arginfo.h: ZEND_ACC_PUBLIC).
        .prop("bucket", acc::PUBLIC, None, Some(Value::Null))
        .prop("data", acc::PUBLIC, Some("string"), None)
        .prop("datalen", acc::PUBLIC, Some("int"), None)
        .prop("dataLength", acc::PUBLIC, Some("int"), None)
        .register(m.vm());
    BUCKET_CLASS.with(|c| *c.borrow_mut() = Some(bucket));
    m.function_static("stream_bucket_make_writeable($brigade): ?StreamBucket", stream_bucket_make_writeable)
        .function_static("stream_bucket_prepend($brigade, StreamBucket $bucket): void", stream_bucket_prepend)
        .function_static("stream_bucket_append($brigade, StreamBucket $bucket): void", stream_bucket_append)
        .function_static("stream_bucket_new($stream, string $buffer): StreamBucket", stream_bucket_new)
        .function_static("stream_get_filters(): array", stream_get_filters)
        .function_static("stream_filter_register(string $filter_name, string $class): bool", stream_filter_register)
        .function_static(
            "stream_filter_prepend($stream, string $filter_name, int $mode = 0, mixed $params = UNKNOWN)",
            stream_filter_prepend,
        )
        .function_static(
            "stream_filter_append($stream, string $filter_name, int $mode = 0, mixed $params = UNKNOWN)",
            stream_filter_append,
        )
        .function_static("stream_filter_remove($stream_filter): bool", stream_filter_remove);
    let _ = VecDeque::<u8>::new;
}
