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
use php_vm::Vm;

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
            if let Err(t) = vm
                .set_property(&obj, "filtername", bytes(filtername))
                .and_then(|()| vm.set_property(&obj, "params", p))
            {
                bridge::set_pending(t);
                return None;
            }
            Some(obj)
        })
        .flatten()
        .map(|obj| {
            let created = userwrap::call_method(&obj, "onCreate", &mut [], &[]);
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
            let stream = res::stream_value_by_id(stream_id).unwrap_or(Value::Null);
            if let Err(t) = vm.set_property(&obj, "stream", stream) {
                bridge::set_pending(t);
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
                Ok(v) => UserCall::Returned(bridge::to_lib(&v)),
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
            if !bridge::has_pending() {
                vm.set_property_raw(&obj, "stream", Value::Null);
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

fn bucket_object(vm: &mut Vm, bucket: Bucket) -> VmResult<Value> {
    let Some(cls) = BUCKET_CLASS.with(|c| c.borrow().clone()) else {
        return Ok(Value::Null);
    };
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
    let mut brigade = b.borrow_mut();
    if append {
        brigade.push_back(bucket);
    } else {
        brigade.push_front(bucket);
    }
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
    let factory = Rc::new(UserFilterFactory::new(Rc::new(FilterClass { class })));
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
        .prop("bucket", acc::PUBLIC | acc::READONLY, None, None)
        .prop("data", acc::PUBLIC | acc::READONLY, Some("string"), None)
        .prop("datalen", acc::PUBLIC | acc::READONLY, Some("int"), None)
        .prop("dataLength", acc::PUBLIC | acc::READONLY, Some("int"), None)
        .register(m.vm());
    BUCKET_CLASS.with(|c| *c.borrow_mut() = Some(bucket));
    m.function("stream_bucket_make_writeable($brigade): ?StreamBucket", stream_bucket_make_writeable)
        .function("stream_bucket_prepend($brigade, StreamBucket $bucket): void", stream_bucket_prepend)
        .function("stream_bucket_append($brigade, StreamBucket $bucket): void", stream_bucket_append)
        .function("stream_bucket_new($stream, string $buffer): StreamBucket", stream_bucket_new)
        .function("stream_get_filters(): array", stream_get_filters)
        .function("stream_filter_register(string $filter_name, string $class): bool", stream_filter_register)
        .function(
            "stream_filter_prepend($stream, string $filter_name, int $mode = 0, mixed $params = UNKNOWN)",
            stream_filter_prepend,
        )
        .function(
            "stream_filter_append($stream, string $filter_name, int $mode = 0, mixed $params = UNKNOWN)",
            stream_filter_append,
        )
        .function("stream_filter_remove($stream_filter): bool", stream_filter_remove);
    let _ = VecDeque::<u8>::new;
}
