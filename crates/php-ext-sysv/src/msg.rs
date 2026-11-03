//! `ext/sysvmsg` (ref: `ext/sysvmsg/sysvmsg.c`).

use php_runtime::{ArrayKey, Value, ZArray, ZStr};
use php_sysv::msg::{MessageQueue, Payload, QueueUpdate, CONSTANTS, MESSAGE_CORRUPTED};
use php_sysv::SysvError;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::objects::{register_class, with_state, wrap};
use crate::{bytes, failure, serial, throw};

/// The class name.
const CLASS: &str = "SysvMessageQueue";

fn queue<R>(a: &Args, f: impl FnOnce(&MessageQueue) -> R) -> Option<R> {
    with_state::<MessageQueue, R>(a, 0, |q| f(q))
}

fn msg_get_queue(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match MessageQueue::get(a.int(0), a.int(1)) {
        Ok(q) => wrap(vm, CLASS, q),
        Err(e) => failure(vm, a, e),
    }
}

fn msg_queue_exists(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match MessageQueue::exists(a.int(0)) {
        Ok(b) => Ok(Value::Bool(b)),
        Err(e) => failure(vm, a, e),
    }
}

fn msg_remove_queue(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(queue(a, MessageQueue::remove).unwrap_or(false)))
}

fn msg_stat_queue(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(Some(stat)) = queue(a, MessageQueue::stat) else {
        return Ok(Value::Bool(false));
    };
    let entries = stat.entries();
    let mut arr = ZArray::with_capacity(entries.len());
    let t = arr.make_mut();
    for (k, v) in entries {
        t.insert(ArrayKey::Str(ZStr::from(k)), Value::Int(v));
    }
    Ok(Value::Array(arr))
}

fn msg_set_queue(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = a.array(1);
    let mut fields = [None; 4];
    for (slot, key) in fields.iter_mut().zip(QueueUpdate::KEYS) {
        if let Some(v) = data.get(&ArrayKey::Str(ZStr::from(key))) {
            *slot = Some(vm.value_to_int(&v.deref())?);
        }
    }
    let update = QueueUpdate {
        uid: fields[0],
        gid: fields[1],
        mode: fields[2],
        qbytes: fields[3],
    };
    Ok(Value::Bool(queue(a, |q| q.set(&update)).unwrap_or(false)))
}

fn msg_send(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let message = a.at(2);
    let data = if a.bool(3) {
        serial::serialize(vm, &message)?
    } else {
        let payload = match &message {
            Value::Str(s) => Payload::Str(s.as_bytes()),
            Value::Int(i) => Payload::Int(*i),
            Value::Float(f) => Payload::Float(*f),
            Value::Bool(b) => Payload::Bool(*b),
            other => {
                return Err(throw(
                    vm,
                    a,
                    php_sysv::msg::payload_type_error(&other.value_name()),
                ))
            }
        };
        payload.to_bytes()
    };
    let r = queue(a, |q| q.send(a.int(1), &data, a.bool(4)));
    match r {
        Some(Ok(())) => Ok(Value::Bool(true)),
        Some(Err(fail)) => {
            vm.php_warning(&fail.message())?;
            if a.has(5) {
                a.set_ref(5, Value::Int(i64::from(fail.errno)));
            }
            Ok(Value::Bool(false))
        }
        None => Ok(Value::Bool(false)),
    }
}

fn msg_receive(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = queue(a, |q| q.receive(a.int(1), a.int(3), a.int(6)));
    let Some(r) = r else {
        return Ok(Value::Bool(false));
    };
    match r {
        Ok(msg) => {
            a.set_ref(2, Value::Int(msg.message_type));
            if a.has(7) {
                a.set_ref(7, Value::Int(0));
            }
            if a.bool(5) {
                if let Some(v) = serial::unserialize(vm, &msg.data)? {
                    a.set_ref(4, v);
                    Ok(Value::Bool(true))
                } else {
                    vm.php_warning(MESSAGE_CORRUPTED)?;
                    a.set_ref(4, Value::Bool(false));
                    Ok(Value::Bool(false))
                }
            } else {
                a.set_ref(4, bytes(&msg.data));
                Ok(Value::Bool(true))
            }
        }
        Err(SysvError::Failed(errno)) => {
            a.set_ref(2, Value::Int(0));
            a.set_ref(4, Value::Bool(false));
            if a.has(7) {
                a.set_ref(7, Value::Int(i64::from(errno)));
            }
            Ok(Value::Bool(false))
        }
        Err(e) => failure(vm, a, e),
    }
}

/// Registers the class, functions and constants.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    register_class(m, CLASS);
    for (name, value) in CONSTANTS {
        m.constant(name, Value::Int(value));
    }
    m.function(
        "msg_get_queue(int $key, int $permissions = 438): SysvMessageQueue|false",
        msg_get_queue,
    )
    .function(
        "msg_send(SysvMessageQueue $queue, int $message_type, $message, bool $serialize = true, bool $blocking = true, &$error_code = null): bool",
        msg_send,
    )
    .function(
        "msg_receive(SysvMessageQueue $queue, int $desired_message_type, &$received_message_type, int $max_message_size, mixed &$message, bool $unserialize = true, int $flags = 0, &$error_code = null): bool",
        msg_receive,
    )
    .function("msg_remove_queue(SysvMessageQueue $queue): bool", msg_remove_queue)
    .function("msg_stat_queue(SysvMessageQueue $queue): array|false", msg_stat_queue)
    .function(
        "msg_set_queue(SysvMessageQueue $queue, array $data): bool",
        msg_set_queue,
    )
    .function("msg_queue_exists(int $key): bool", msg_queue_exists);
}
