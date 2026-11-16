// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/sysvmsg` (ref: `ext/sysvmsg/sysvmsg.c`).

use crate::{c_int, key_from_arg, strerror, sys, xlong, SysvError, SysvResult};

/// `MSG_IPC_NOWAIT`.
pub const MSG_IPC_NOWAIT: i64 = 1;
/// `MSG_NOERROR`.
pub const MSG_NOERROR: i64 = 2;
/// `MSG_EXCEPT`.
pub const MSG_EXCEPT: i64 = 4;
/// `MSG_EAGAIN` (`EAGAIN`).
pub const MSG_EAGAIN: i64 = libc::EAGAIN as i64;
/// `MSG_ENOMSG` (`ENOMSG`).
pub const MSG_ENOMSG: i64 = libc::ENOMSG as i64;

/// Default `$permissions` of `msg_get_queue()`.
pub const DEFAULT_PERMISSIONS: i64 = 0o666;

/// Warning `msg_receive()` emits when `unserialize()` of the received
/// message fails (it then assigns `$message = false` and returns `false`;
/// `$received_message_type` and `$error_code` keep their success values).
pub const MESSAGE_CORRUPTED: &str = "Message corrupted";

/// Names and values of the constants ext/sysvmsg registers.
pub const CONSTANTS: [(&str, i64); 5] = [
    ("MSG_IPC_NOWAIT", MSG_IPC_NOWAIT),
    ("MSG_EAGAIN", MSG_EAGAIN),
    ("MSG_ENOMSG", MSG_ENOMSG),
    ("MSG_NOERROR", MSG_NOERROR),
    ("MSG_EXCEPT", MSG_EXCEPT),
];

/// A `$message` for `msg_send(..., serialize: false)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Payload<'a> {
    /// A PHP string, sent verbatim.
    Str(&'a [u8]),
    /// An int, sent as its decimal text.
    Int(i64),
    /// A float, sent as `spprintf("%F")` renders it.
    Float(f64),
    /// A bool, sent as `"1"`/`"0"`.
    Bool(bool),
}

impl Payload<'_> {
    /// The bytes PHP puts in `mtext`.
    pub fn to_bytes(&self) -> Vec<u8> {
        match *self {
            Self::Str(s) => s.to_vec(),
            Self::Int(i) => i.to_string().into_bytes(),
            Self::Bool(b) => {
                if b {
                    b"1".to_vec()
                } else {
                    b"0".to_vec()
                }
            }
            Self::Float(f) => spprintf_upper_f(f).into_bytes(),
        }
    }
}

/// `spprintf("%F", d)`: `nan`/`inf` (no sign) for non-finite values,
/// otherwise fixed notation with 6 decimals and a `-` only for values
/// below zero (so `-0.0` prints as `0.000000`).
fn spprintf_upper_f(d: f64) -> String {
    if d.is_nan() {
        "nan".into()
    } else if d.is_infinite() {
        "inf".into()
    } else if d == 0.0 {
        "0.000000".into()
    } else {
        format!("{d:.6}")
    }
}

/// The `TypeError` for an unsupported non-serialized `$message`;
/// `type_name` is `zend_zval_value_name()` (e.g. `"null"`, `"array"`).
pub fn payload_type_error(type_name: &str) -> SysvError {
    SysvError::TypeError(format!(
        "Argument #3 ($message) must be of type string|int|float|bool, {type_name} given"
    ))
}

/// A message returned by [`MessageQueue::receive`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    /// The message's type (`$received_message_type`).
    pub message_type: i64,
    /// The raw `mtext` (unserialize it when `$unserialize` is true).
    pub data: Vec<u8>,
}

/// A failed `msgsnd()`: PHP warns with [`SendFailure::message`], assigns
/// `$error_code = errno` and returns `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendFailure {
    /// The `errno` from `msgsnd()`.
    pub errno: i32,
}

impl SendFailure {
    /// `"msgsnd failed: %s"`.
    pub fn message(&self) -> String {
        format!("msgsnd failed: {}", strerror(self.errno))
    }
}

/// The `msg_stat_queue()` array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueStat {
    pub uid: i64,
    pub gid: i64,
    pub mode: i64,
    pub stime: i64,
    pub rtime: i64,
    pub ctime: i64,
    pub qnum: i64,
    pub qbytes: i64,
    pub lspid: i64,
    pub lrpid: i64,
}

impl QueueStat {
    /// The PHP array entries, in order.
    pub fn entries(&self) -> [(&'static str, i64); 10] {
        [
            ("msg_perm.uid", self.uid),
            ("msg_perm.gid", self.gid),
            ("msg_perm.mode", self.mode),
            ("msg_stime", self.stime),
            ("msg_rtime", self.rtime),
            ("msg_ctime", self.ctime),
            ("msg_qnum", self.qnum),
            ("msg_qbytes", self.qbytes),
            ("msg_lspid", self.lspid),
            ("msg_lrpid", self.lrpid),
        ]
    }
}

/// The keys of `msg_set_queue()`'s `$data` PHP looks at (values already
/// converted with `zval_get_long()`); other keys are ignored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueUpdate {
    /// `"msg_perm.uid"`.
    pub uid: Option<i64>,
    /// `"msg_perm.gid"`.
    pub gid: Option<i64>,
    /// `"msg_perm.mode"`.
    pub mode: Option<i64>,
    /// `"msg_qbytes"`.
    pub qbytes: Option<i64>,
}

impl QueueUpdate {
    /// The array keys consulted, in the order PHP reads them.
    pub const KEYS: [&'static str; 4] = [
        "msg_perm.uid",
        "msg_perm.gid",
        "msg_perm.mode",
        "msg_qbytes",
    ];
}

/// A `SysvMessageQueue`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageQueue {
    key: i32,
    id: i32,
}

impl MessageQueue {
    /// `msg_get_queue(int $key, int $permissions = 0666)`.
    ///
    /// Attaches to an existing queue or creates it (`IPC_PRIVATE` always
    /// creates a new one).  Errors: `ValueError` for an out-of-range key,
    /// `Warning("Failed for key 0x%lx: %s")` when creation fails.
    pub fn get(key: i64, permissions: i64) -> SysvResult<Self> {
        let k = key_from_arg(key)?;
        let existing = if k == libc::IPC_PRIVATE {
            None
        } else {
            sys::msgget(k, 0).ok()
        };
        let id = match existing {
            Some(id) => id,
            None => sys::msgget(k, libc::IPC_CREAT | libc::IPC_EXCL | c_int(permissions)).map_err(
                |e| SysvError::Warning(format!("Failed for key 0x{}: {}", xlong(key), strerror(e))),
            )?,
        };
        Ok(Self { key: k, id })
    }

    /// `msg_queue_exists(int $key)`; always `false` for `IPC_PRIVATE`.
    pub fn exists(key: i64) -> SysvResult<bool> {
        let k = key_from_arg(key)?;
        Ok(k != libc::IPC_PRIVATE && sys::msgget(k, 0).is_ok())
    }

    /// The `key_t` the queue was requested with.
    pub fn key(&self) -> i32 {
        self.key
    }

    /// The `msqid`.
    pub fn id(&self) -> i32 {
        self.id
    }

    /// `msg_send()`: sends `message` (already serialized, or
    /// [`Payload::to_bytes`] when `$serialize` is false) with type
    /// `message_type`; `blocking == false` adds `IPC_NOWAIT`.
    pub fn send(
        &self,
        message_type: i64,
        message: &[u8],
        blocking: bool,
    ) -> Result<(), SendFailure> {
        let flags = if blocking { 0 } else { libc::IPC_NOWAIT };
        sys::msgsnd(self.id, message_type, message, flags).map_err(|errno| SendFailure { errno })
    }

    /// `msg_receive()`.
    ///
    /// Errors: `ValueError` when `max_message_size <= 0`;
    /// `Failed(errno)` when `msgrcv()` fails — PHP then assigns
    /// `$received_message_type = 0`, `$message = false`,
    /// `$error_code = errno`.  On success `$error_code = 0`.
    pub fn receive(
        &self,
        desired_type: i64,
        max_message_size: i64,
        flags: i64,
    ) -> SysvResult<Received> {
        if max_message_size <= 0 {
            return Err(SysvError::ValueError(
                "Argument #4 ($max_message_size) must be greater than 0".into(),
            ));
        }
        let mut real = 0;
        if flags & MSG_EXCEPT != 0 {
            real |= libc::MSG_EXCEPT;
        }
        if flags & MSG_NOERROR != 0 {
            real |= libc::MSG_NOERROR;
        }
        if flags & MSG_IPC_NOWAIT != 0 {
            real |= libc::IPC_NOWAIT;
        }
        let max = self.receive_buffer_size(max_message_size);
        sys::msgrcv(self.id, max, desired_type, real)
            .map(|(message_type, data)| Received { message_type, data })
            .map_err(SysvError::Failed)
    }

    /// PHP allocates `$max_message_size` bytes up front.  No message can be
    /// larger than the larger of the queue's byte limit and `MSGMAX`, so a
    /// buffer capped there behaves identically without letting a huge
    /// `$max_message_size` exhaust memory.
    fn receive_buffer_size(&self, requested: i64) -> usize {
        const FLOOR: usize = 1 << 20;
        let requested = usize::try_from(requested).unwrap_or(usize::MAX);
        let qbytes = sys::msg_stat(self.id)
            .ok()
            .and_then(|s| usize::try_from(s.qbytes).ok())
            .unwrap_or(0);
        let msgmax = std::fs::read_to_string("/proc/sys/kernel/msgmax")
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(0);
        requested.min(qbytes.max(msgmax).max(FLOOR))
    }

    /// `msg_remove_queue()`.
    pub fn remove(&self) -> bool {
        sys::msg_remove(self.id).is_ok()
    }

    /// `msg_stat_queue()`; `None` is PHP's `false`.
    pub fn stat(&self) -> Option<QueueStat> {
        sys::msg_stat(self.id).ok().map(|s| QueueStat {
            uid: s.uid,
            gid: s.gid,
            mode: s.mode,
            stime: s.stime,
            rtime: s.rtime,
            ctime: s.ctime,
            qnum: s.qnum,
            qbytes: s.qbytes,
            lspid: s.lspid,
            lrpid: s.lrpid,
        })
    }

    /// `msg_set_queue()`.
    pub fn set(&self, update: &QueueUpdate) -> bool {
        sys::msg_set(self.id, update.uid, update.gid, update.mode, update.qbytes).is_ok()
    }
}
