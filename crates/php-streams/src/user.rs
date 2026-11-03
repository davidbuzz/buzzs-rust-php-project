//! User-space stream wrappers (`main/streams/userspace.c`).
//!
//! `stream_wrapper_register()` binds a protocol to a PHP class. The VM
//! implements [`UserWrapperClass`] (instantiation) and [`UserStreamObject`]
//! (method calls); this module implements PHP's calling protocol on top:
//! which methods are called with which arguments, how return values are
//! interpreted, and the exact warnings for missing methods and bad returns.

use std::fmt;
use std::os::fd::RawFd;
use std::rc::Rc;

use crate::context::ContextRef;
use crate::diag::{Severity, StreamErrorCode};
use crate::stat::StatBuf;
use crate::state;
use crate::stream::{
    flags, BufferMode, CastAs, OpFailed, Stream, StreamOps, StreamOption, StreamState, OPT_ERR,
    OPT_NOTIMPL, OPT_OK, SEEK_SET,
};
use crate::sys;
use crate::value::{Key, UserCall, Value};
use crate::wrapper::{
    wrapper_warn, MetadataOp, OpenError, Wrapper, REPORT_ERRORS, STREAM_OPEN_FOR_INCLUDE,
};

/// The wrapper/ops label of user-space wrappers.
pub const LABEL: &str = "user-space";

/// `PHP_STREAM_IS_URL` flag of `stream_wrapper_register()`.
pub const IS_URL: i64 = 1;

/// Method names called on wrapper objects.
pub mod methods {
    #![allow(missing_docs)]
    pub const OPEN: &str = "stream_open";
    pub const CLOSE: &str = "stream_close";
    pub const READ: &str = "stream_read";
    pub const WRITE: &str = "stream_write";
    pub const FLUSH: &str = "stream_flush";
    pub const SEEK: &str = "stream_seek";
    pub const TELL: &str = "stream_tell";
    pub const EOF: &str = "stream_eof";
    pub const STAT: &str = "stream_stat";
    pub const STATURL: &str = "url_stat";
    pub const UNLINK: &str = "unlink";
    pub const RENAME: &str = "rename";
    pub const MKDIR: &str = "mkdir";
    pub const RMDIR: &str = "rmdir";
    pub const DIR_OPEN: &str = "dir_opendir";
    pub const DIR_READ: &str = "dir_readdir";
    pub const DIR_REWIND: &str = "dir_rewinddir";
    pub const DIR_CLOSE: &str = "dir_closedir";
    pub const LOCK: &str = "stream_lock";
    pub const SET_OPTION: &str = "stream_set_option";
    pub const TRUNCATE: &str = "stream_truncate";
    pub const METADATA: &str = "stream_metadata";
    pub const CAST: &str = "stream_cast";
}

/// A user wrapper object (an instance of the registered class).
pub trait UserStreamObject: fmt::Debug {
    /// `zend_call_method_if_exists()`: calls `method` with `args`. By-ref
    /// arguments (the 4th argument of `stream_open`) are written back into
    /// `args`. Returns [`UserCall::NotImplemented`] when the method does not
    /// exist and [`UserCall::Threw`] when it threw.
    fn call(&mut self, method: &str, args: &mut [Value]) -> UserCall;

    /// `zend_is_callable_ex()` for `method` on this object.
    fn is_callable(&self, method: &str) -> bool;

    /// Resolves a stream resource returned by `stream_cast()` to a file
    /// descriptor (`php_stream_cast` on the returned stream).
    fn cast_stream(&mut self, _stream_id: u64, _kind: CastAs) -> Option<RawFd> {
        None
    }
}

/// A class registered with `stream_wrapper_register()`.
pub trait UserWrapperClass: fmt::Debug {
    /// The class name (for messages).
    fn class_name(&self) -> String;

    /// `user_stream_create_object()`: instantiates the class, sets the
    /// `context` property and calls the constructor. `None` when an
    /// exception was thrown.
    fn instantiate(&self, context: Option<&ContextRef>) -> Option<Box<dyn UserStreamObject>>;
}

/// The wrapper created by `stream_wrapper_register()`.
#[derive(Debug)]
pub struct UserWrapper {
    class: Rc<dyn UserWrapperClass>,
    is_url: bool,
}

impl UserWrapper {
    /// Creates a wrapper for `class`; `flags` is the third argument of
    /// `stream_wrapper_register()`.
    pub fn new(class: Rc<dyn UserWrapperClass>, flags: i64) -> Self {
        UserWrapper {
            class,
            is_url: flags & IS_URL != 0,
        }
    }

    /// The class name.
    pub fn class_name(&self) -> String {
        self.class.class_name()
    }
}

/// Result of `stream_wrapper_register()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterResult {
    /// Registered.
    Ok,
    /// Failed (a warning was emitted).
    Failed,
}

/// `stream_wrapper_register($protocol, $class, $flags)`.
pub fn register(protocol: &[u8], class: Rc<dyn UserWrapperClass>, flags: i64) -> RegisterResult {
    let name = class.class_name();
    let w = Rc::new(UserWrapper::new(class, flags));
    if crate::wrapper::register_volatile(protocol, w) {
        return RegisterResult::Ok;
    }
    let p = String::from_utf8_lossy(protocol);
    if crate::wrapper::find(protocol).is_some() {
        wrapper_warn(
            LABEL,
            REPORT_ERRORS,
            StreamErrorCode::WrapperRegistrationFailed,
            format!("Protocol {p}:// is already defined."),
        );
    } else {
        wrapper_warn(
            LABEL,
            REPORT_ERRORS,
            StreamErrorCode::WrapperRegistrationFailed,
            format!("Invalid protocol scheme specified. Unable to register wrapper class {name} to {p}://"),
        );
    }
    RegisterResult::Failed
}

/// `stream_wrapper_unregister($protocol)`.
pub fn unregister(protocol: &[u8]) -> bool {
    let label =
        crate::wrapper::find(protocol).map_or(crate::wrapper::DEFAULT_WRAPPER_NAME, |w| w.label());
    if !crate::wrapper::unregister_volatile(protocol) {
        wrapper_warn(
            label,
            REPORT_ERRORS,
            StreamErrorCode::WrapperUnregistrationFailed,
            format!(
                "Unable to unregister protocol {}://",
                String::from_utf8_lossy(protocol)
            ),
        );
        return false;
    }
    true
}

/// `stream_wrapper_restore($protocol)`.
pub fn restore(protocol: &[u8]) -> bool {
    let p = String::from_utf8_lossy(protocol).into_owned();
    let Some(global) = crate::wrapper::find_global(protocol) else {
        wrapper_warn(
            LABEL,
            REPORT_ERRORS,
            StreamErrorCode::WrapperNotFound,
            format!("{p}:// never existed, nothing to restore"),
        );
        return false;
    };
    let unchanged = !crate::wrapper::has_volatile()
        || crate::wrapper::find(protocol).is_some_and(|w| Rc::ptr_eq(&w, &global));
    if unchanged {
        crate::wrapper::wrapper_error(
            global.label(),
            REPORT_ERRORS,
            Severity::Notice,
            false,
            StreamErrorCode::WrapperRestorationFailed,
            format!("{p}:// was never changed, nothing to restore"),
        );
        return true;
    }
    crate::wrapper::unregister_volatile(protocol);
    if !crate::wrapper::register_volatile(protocol, global.clone()) {
        wrapper_warn(
            global.label(),
            REPORT_ERRORS,
            StreamErrorCode::WrapperRestorationFailed,
            format!("Unable to restore original {p}:// wrapper"),
        );
        return false;
    }
    true
}

impl Wrapper for UserWrapper {
    fn label(&self) -> &'static str {
        LABEL
    }

    fn is_url(&self) -> bool {
        self.is_url
    }

    fn open(
        &self,
        path: &[u8],
        mode: &str,
        options: u32,
        opened_path: &mut Option<Vec<u8>>,
        context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        if state::user_stream_current_filename().as_deref() == Some(path) {
            return Err(OpenError::msg("infinite recursion prevented"));
        }
        state::set_user_stream_current_filename(Some(path.to_vec()));
        let old_include = state::in_user_include();
        if !self.is_url
            && options & STREAM_OPEN_FOR_INCLUDE != 0
            && !state::settings().allow_url_include
        {
            state::set_in_user_include(true);
        }
        let result = self.do_open(path, mode, options, opened_path, context);
        state::set_user_stream_current_filename(None);
        state::set_in_user_include(old_include);
        result
    }

    fn url_stat(
        &self,
        path: &[u8],
        flags: u32,
        context: Option<&ContextRef>,
    ) -> Option<Result<StatBuf, ()>> {
        let Some(mut obj) = self.class.instantiate(context) else {
            return Some(Err(()));
        };
        let mut args = [Value::Str(path.to_vec()), Value::Int(i64::from(flags))];
        match obj.call(methods::STATURL, &mut args) {
            UserCall::NotImplemented => {
                wrapper_warn(
                    LABEL,
                    REPORT_ERRORS,
                    StreamErrorCode::NotImplemented,
                    format!(
                        "{}::{} is not implemented!",
                        self.class_name(),
                        methods::STATURL
                    ),
                );
                Some(Err(()))
            }
            UserCall::Returned(v @ Value::Array(_)) => Some(Ok(StatBuf::from_value(&v))),
            UserCall::Threw | UserCall::Returned(_) => Some(Err(())),
        }
    }

    fn has_dir_opener(&self) -> bool {
        true
    }

    fn opendir(
        &self,
        path: &[u8],
        options: u32,
        context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        if state::user_stream_current_filename().as_deref() == Some(path) {
            return Err(OpenError::msg("infinite recursion prevented"));
        }
        state::set_user_stream_current_filename(Some(path.to_vec()));
        let r = (|| {
            let mut obj = self
                .class
                .instantiate(context)
                .ok_or_else(OpenError::exception)?;
            let mut args = [Value::Str(path.to_vec()), Value::Int(i64::from(options))];
            match obj.call(methods::DIR_OPEN, &mut args) {
                UserCall::NotImplemented => Err(OpenError::msg(format!(
                    "\"{}::{}\" is not implemented",
                    self.class_name(),
                    methods::DIR_OPEN
                ))),
                UserCall::Threw => Err(OpenError {
                    exception: true,
                    ..OpenError::default()
                }),
                UserCall::Returned(v) if v.is_true() => {
                    let ops = UserStreamOps {
                        obj,
                        class: self.class_name(),
                        dir: true,
                    };
                    Ok(Stream::new(Box::new(ops), "r"))
                }
                UserCall::Returned(_) => Err(OpenError::msg(format!(
                    "\"{}::{}\" call failed",
                    self.class_name(),
                    methods::DIR_OPEN
                ))),
            }
        })();
        state::set_user_stream_current_filename(None);
        r
    }

    fn has_unlink(&self) -> bool {
        true
    }

    fn has_rename(&self) -> bool {
        true
    }

    fn unlink(&self, path: &[u8], _options: u32, context: Option<&ContextRef>) -> Option<bool> {
        Some(self.simple_op(methods::UNLINK, vec![Value::Str(path.to_vec())], context))
    }

    fn rename(
        &self,
        from: &[u8],
        to: &[u8],
        _options: u32,
        context: Option<&ContextRef>,
    ) -> Option<bool> {
        Some(self.simple_op(
            methods::RENAME,
            vec![Value::Str(from.to_vec()), Value::Str(to.to_vec())],
            context,
        ))
    }

    fn mkdir(
        &self,
        path: &[u8],
        mode: i64,
        options: u32,
        context: Option<&ContextRef>,
    ) -> Option<bool> {
        Some(self.simple_op(
            methods::MKDIR,
            vec![
                Value::Str(path.to_vec()),
                Value::Int(mode),
                Value::Int(i64::from(options)),
            ],
            context,
        ))
    }

    fn rmdir(&self, path: &[u8], options: u32, context: Option<&ContextRef>) -> Option<bool> {
        Some(self.simple_op(
            methods::RMDIR,
            vec![Value::Str(path.to_vec()), Value::Int(i64::from(options))],
            context,
        ))
    }

    fn metadata(&self, path: &[u8], op: &MetadataOp, context: Option<&ContextRef>) -> Option<bool> {
        let arg = match op {
            MetadataOp::Touch(t) => Value::Array(match t {
                Some((m, a)) => vec![(Key::Int(0), Value::Int(*m)), (Key::Int(1), Value::Int(*a))],
                None => Vec::new(),
            }),
            MetadataOp::Owner(v) | MetadataOp::Group(v) | MetadataOp::Access(v) => Value::Int(*v),
            MetadataOp::OwnerName(n) | MetadataOp::GroupName(n) => Value::Str(n.clone()),
        };
        Some(self.simple_op(
            methods::METADATA,
            vec![Value::Str(path.to_vec()), Value::Int(op.option()), arg],
            context,
        ))
    }
}

impl UserWrapper {
    fn do_open(
        &self,
        path: &[u8],
        mode: &str,
        options: u32,
        opened_path: &mut Option<Vec<u8>>,
        context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        let mut obj = self
            .class
            .instantiate(context)
            .ok_or_else(OpenError::exception)?;
        let mut args = [
            Value::Str(path.to_vec()),
            Value::str(mode),
            Value::Int(i64::from(options)),
            Value::Null,
        ];
        match obj.call(methods::OPEN, &mut args) {
            UserCall::NotImplemented => Err(OpenError::msg(format!(
                "\"{}::{}\" is not implemented",
                self.class_name(),
                methods::OPEN
            ))),
            UserCall::Threw => Err(OpenError {
                exception: true,
                ..OpenError::default()
            }),
            UserCall::Returned(v) if v.is_true() => {
                if let Value::Str(p) = &args[3] {
                    *opened_path = Some(p.clone());
                }
                let ops = UserStreamOps {
                    obj,
                    class: self.class_name(),
                    dir: false,
                };
                Ok(Stream::new(Box::new(ops), mode))
            }
            UserCall::Returned(_) => Err(OpenError::msg(format!(
                "\"{}::{}\" call failed",
                self.class_name(),
                methods::OPEN
            ))),
        }
    }

    fn simple_op(&self, method: &str, mut args: Vec<Value>, context: Option<&ContextRef>) -> bool {
        let Some(mut obj) = self.class.instantiate(context) else {
            return false;
        };
        match obj.call(method, &mut args) {
            UserCall::NotImplemented => {
                wrapper_warn(
                    LABEL,
                    REPORT_ERRORS,
                    StreamErrorCode::NotImplemented,
                    format!("{}::{method} is not implemented!", self.class_name()),
                );
                false
            }
            UserCall::Returned(Value::Bool(b)) => b,
            _ => false,
        }
    }
}

/// Stream ops for user-space streams (`php_stream_userspace_ops` and
/// `php_stream_userspace_dir_ops`).
#[derive(Debug)]
pub struct UserStreamOps {
    obj: Box<dyn UserStreamObject>,
    class: String,
    dir: bool,
}

thread_local! {
    /// Streams running a user-space method with `PHP_STREAM_FLAG_NO_FCLOSE`
    /// set (read, write, eof, seek, tell, cast, set_option).
    static GUARDED: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Whether stream `id` is inside a user-space call that forbids
/// `fclose()` (its flags are not reachable while the stream is in use).
pub fn in_guarded_call(id: u64) -> bool {
    // User filters run with the flag set too (`userfilter_filter`).
    GUARDED.with(|g| g.borrow().contains(&id)) || crate::filter::user::filtering_eof(id).is_some()
}

impl UserStreamOps {
    /// The wrapper object (`wrapper_data` in the meta data).
    pub fn object(&self) -> &dyn UserStreamObject {
        &*self.obj
    }

    /// Mutable access to the wrapper object.
    pub fn object_mut(&mut self) -> &mut dyn UserStreamObject {
        &mut *self.obj
    }

    fn guarded(&mut self, st: &mut StreamState, method: &str, args: &mut [Value]) -> UserCall {
        let orig = st.flags & flags::NO_FCLOSE;
        st.flags |= flags::NO_FCLOSE;
        GUARDED.with(|g| g.borrow_mut().push(st.id));
        let r = self.obj.call(method, args);
        GUARDED.with(|g| {
            g.borrow_mut().pop();
        });
        st.flags &= !flags::NO_FCLOSE;
        st.flags |= orig;
        r
    }

    fn not_impl(&self, st: &StreamState, method: &str, suffix: &str) {
        st.warn(
            StreamErrorCode::NotImplemented,
            format!("{}::{method} is not implemented!{suffix}", self.class),
        );
    }

    fn check_liveness(&mut self, st: &mut StreamState) -> i64 {
        match self.obj.call(methods::EOF, &mut []) {
            UserCall::NotImplemented => {
                self.not_impl(st, methods::EOF, " Assuming EOF");
                OPT_ERR
            }
            UserCall::Threw | UserCall::Returned(Value::Bool(true)) => OPT_ERR,
            UserCall::Returned(Value::Bool(false)) => OPT_OK,
            UserCall::Returned(v) => {
                st.warn(
                    StreamErrorCode::UserspaceInvalidReturn,
                    format!(
                        "{}::{} value must be of type bool, {} given",
                        self.class,
                        methods::EOF,
                        v.type_name()
                    ),
                );
                OPT_ERR
            }
        }
    }
}

impl StreamOps for UserStreamOps {
    fn label(&self) -> &'static str {
        if self.dir {
            "user-space-dir"
        } else {
            "user-space"
        }
    }

    fn is_userspace(&self) -> bool {
        !self.dir
    }

    fn read(&mut self, st: &mut StreamState, buf: &mut [u8]) -> Result<usize, OpFailed> {
        if self.dir {
            return Err(OpFailed);
        }
        let count = buf.len();
        let mut args = [Value::Int(i64::try_from(count).unwrap_or(i64::MAX))];
        let ret = self.guarded(st, methods::READ, &mut args);
        let data = match ret {
            UserCall::NotImplemented => {
                self.not_impl(st, methods::READ, "");
                return Err(OpFailed);
            }
            UserCall::Threw | UserCall::Returned(Value::Bool(false)) => return Err(OpFailed),
            UserCall::Returned(v) => v.to_bytes().ok_or(OpFailed)?,
        };
        let mut didread = data.len();
        if didread > count {
            st.warn_nt(
                StreamErrorCode::UserspaceInvalidReturn,
                format!(
                    "{}::{} - read {} bytes more data than requested ({didread} read, {count} max) - excess data will be lost",
                    self.class,
                    methods::READ,
                    didread - count
                ),
            );
            didread = count;
        }
        buf[..didread].copy_from_slice(&data[..didread]);
        match self.guarded(st, methods::EOF, &mut []) {
            UserCall::NotImplemented => {
                self.not_impl(st, methods::EOF, " Assuming EOF");
                st.eof = true;
                return Err(OpFailed);
            }
            UserCall::Threw => {
                st.eof = true;
                return Err(OpFailed);
            }
            UserCall::Returned(v) => {
                if v.is_true() {
                    st.eof = true;
                }
            }
        }
        Ok(didread)
    }

    fn can_write(&self) -> bool {
        !self.dir
    }

    fn write(&mut self, st: &mut StreamState, buf: &[u8]) -> Result<usize, OpFailed> {
        let mut args = [Value::Str(buf.to_vec())];
        let ret = self.guarded(st, methods::WRITE, &mut args);
        let didwrite = match ret {
            UserCall::NotImplemented => {
                self.not_impl(st, methods::WRITE, "");
                return Err(OpFailed);
            }
            UserCall::Threw | UserCall::Returned(Value::Bool(false)) => return Err(OpFailed),
            UserCall::Returned(v) => v.to_long(),
        };
        let count = i64::try_from(buf.len()).unwrap_or(i64::MAX);
        if didwrite > count {
            st.warn_nt(
                StreamErrorCode::UserspaceInvalidReturn,
                format!(
                    "{}::{} wrote {} bytes more data than requested ({didwrite} written, {count} max)",
                    self.class,
                    methods::WRITE,
                    didwrite - count
                ),
            );
            return Ok(buf.len());
        }
        usize::try_from(didwrite).map_err(|_| OpFailed)
    }

    fn close(&mut self, _st: &mut StreamState, _preserve: bool) -> i32 {
        let m = if self.dir {
            methods::DIR_CLOSE
        } else {
            methods::CLOSE
        };
        let _ = self.obj.call(m, &mut []);
        0
    }

    fn flush(&mut self, _st: &mut StreamState) -> i32 {
        if self.dir {
            return 0;
        }
        if self.obj.call(methods::FLUSH, &mut []).is_true() {
            0
        } else {
            -1
        }
    }

    fn can_seek(&self) -> bool {
        true
    }

    fn seek(&mut self, st: &mut StreamState, offset: i64, whence: i32, newpos: &mut i64) -> bool {
        if self.dir {
            let _ = self.obj.call(methods::DIR_REWIND, &mut []);
            return true;
        }
        let mut args = [Value::Int(offset), Value::Int(i64::from(whence))];
        match self.guarded(st, methods::SEEK, &mut args) {
            UserCall::NotImplemented => {
                st.flags |= flags::NO_SEEK;
                return false;
            }
            r if !r.is_true() => return false,
            _ => {}
        }
        match self.guarded(st, methods::TELL, &mut []) {
            UserCall::Returned(Value::Int(p)) => {
                *newpos = p;
                true
            }
            UserCall::NotImplemented => {
                self.not_impl(st, methods::TELL, "");
                false
            }
            _ => false,
        }
    }

    fn stat(&mut self, st: &mut StreamState) -> Option<StatBuf> {
        if self.dir {
            return None;
        }
        match self.obj.call(methods::STAT, &mut []) {
            UserCall::NotImplemented => {
                self.not_impl(st, methods::STAT, "");
                None
            }
            UserCall::Returned(v @ Value::Array(_)) => Some(StatBuf::from_value(&v)),
            _ => None,
        }
    }

    #[allow(clippy::too_many_lines)] // a direct port of the C function
    fn set_option(&mut self, st: &mut StreamState, opt: &mut StreamOption<'_>) -> i64 {
        if self.dir {
            return OPT_NOTIMPL;
        }
        match opt {
            StreamOption::CheckLiveness => self.check_liveness(st),
            StreamOption::Locking(op) => {
                let op = *op;
                let mut lock = 0i64;
                if op & sys::LOCK_NB != 0 {
                    lock |= 4;
                }
                match op & !sys::LOCK_NB {
                    x if x == sys::LOCK_SH => lock |= 1,
                    x if x == sys::LOCK_EX => lock |= 2,
                    x if x == sys::LOCK_UN => lock |= 3,
                    _ => {}
                }
                match self.obj.call(methods::LOCK, &mut [Value::Int(lock)]) {
                    UserCall::NotImplemented => {
                        if op == 0 {
                            return OPT_OK;
                        }
                        self.not_impl(st, methods::LOCK, "");
                        OPT_ERR
                    }
                    UserCall::Threw => OPT_ERR,
                    UserCall::Returned(Value::Bool(b)) => i64::from(!b),
                    UserCall::Returned(_) => OPT_NOTIMPL,
                }
            }
            StreamOption::LockSupported => {
                match self.obj.call(methods::LOCK, &mut [Value::Int(0)]) {
                    UserCall::NotImplemented => OPT_OK,
                    UserCall::Threw => OPT_ERR,
                    UserCall::Returned(Value::Bool(b)) => i64::from(!b),
                    UserCall::Returned(_) => OPT_NOTIMPL,
                }
            }
            StreamOption::TruncateSupported => {
                if self.obj.is_callable(methods::TRUNCATE) {
                    OPT_OK
                } else {
                    OPT_ERR
                }
            }
            StreamOption::TruncateSetSize(size) => {
                if *size < 0 {
                    return OPT_ERR;
                }
                match self.obj.call(methods::TRUNCATE, &mut [Value::Int(*size)]) {
                    UserCall::NotImplemented => {
                        self.not_impl(st, methods::TRUNCATE, "");
                        OPT_ERR
                    }
                    UserCall::Returned(Value::Bool(true)) => OPT_OK,
                    UserCall::Threw | UserCall::Returned(Value::Bool(false)) => OPT_ERR,
                    UserCall::Returned(v) => {
                        st.warn(
                            StreamErrorCode::UserspaceInvalidReturn,
                            format!(
                                "{}::{} value must be of type bool, {} given",
                                self.class,
                                methods::TRUNCATE,
                                v.type_name()
                            ),
                        );
                        OPT_ERR
                    }
                }
            }
            StreamOption::ReadBuffer(..)
            | StreamOption::WriteBuffer(..)
            | StreamOption::ReadTimeout(..)
            | StreamOption::Blocking(_) => {
                let mut args = match opt {
                    StreamOption::ReadTimeout(sec, usec) => {
                        [Value::Int(4), Value::Int(*sec), Value::Int(*usec)]
                    }
                    StreamOption::ReadBuffer(m, size) => [
                        Value::Int(2),
                        Value::Int(buffer_mode(*m)),
                        Value::Int(size_arg(*size)),
                    ],
                    StreamOption::WriteBuffer(m, size) => [
                        Value::Int(3),
                        Value::Int(buffer_mode(*m)),
                        Value::Int(size_arg(*size)),
                    ],
                    StreamOption::Blocking(b) => {
                        [Value::Int(1), Value::Int(i64::from(*b)), Value::Null]
                    }
                    _ => unreachable!(),
                };
                match self.obj.call(methods::SET_OPTION, &mut args) {
                    UserCall::NotImplemented => {
                        self.not_impl(st, methods::SET_OPTION, "");
                        OPT_ERR
                    }
                    r if r.is_true() => OPT_OK,
                    _ => OPT_ERR,
                }
            }
            StreamOption::ChunkSize(_)
            | StreamOption::Sync(_)
            | StreamOption::SyncSupported
            | StreamOption::MetaData(_) => OPT_NOTIMPL,
        }
    }

    fn cast(&mut self, st: &mut StreamState, kind: CastAs, report_errors: bool) -> Option<RawFd> {
        if self.dir {
            return None;
        }
        let arg = match kind {
            CastAs::FdForSelect => 3,
            _ => 0,
        };
        let r = self.guarded(st, methods::CAST, &mut [Value::Int(arg)]);
        match r {
            UserCall::NotImplemented => {
                if report_errors {
                    self.not_impl(st, methods::CAST, "");
                }
                None
            }
            UserCall::Returned(v) if v.is_true() => match v {
                Value::Stream(id) if id == st.id => {
                    if report_errors {
                        st.warn(
                            StreamErrorCode::UserspaceInvalidReturn,
                            format!("{}::{} must not return itself", self.class, methods::CAST),
                        );
                    }
                    None
                }
                Value::Stream(id) => self.obj.cast_stream(id, kind),
                _ => {
                    if report_errors {
                        st.warn(
                            StreamErrorCode::UserspaceInvalidReturn,
                            format!(
                                "{}::{} must return a stream resource",
                                self.class,
                                methods::CAST
                            ),
                        );
                    }
                    None
                }
            },
            _ => None,
        }
    }

    fn readdir(&mut self, st: &mut StreamState) -> Option<Vec<u8>> {
        if !self.dir {
            return None;
        }
        match self.obj.call(methods::DIR_READ, &mut []) {
            UserCall::NotImplemented => {
                self.not_impl(st, methods::DIR_READ, "");
                None
            }
            UserCall::Threw | UserCall::Returned(Value::Bool(_)) => None,
            UserCall::Returned(v) => {
                // PHP >= 8.3.10 keeps the whole name (GH-14930).
                v.to_bytes()
            }
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn buffer_mode(m: BufferMode) -> i64 {
    match m {
        BufferMode::None => 0,
        BufferMode::Line => 1,
        BufferMode::Full => 2,
    }
}

fn size_arg(size: Option<usize>) -> i64 {
    size.map_or(8192, |s| i64::try_from(s).unwrap_or(i64::MAX))
}

/// Re-exported for convenience: rewinding a user directory calls
/// `dir_rewinddir()`.
pub fn rewind_dir(stream: &mut Stream) {
    stream.seek(0, SEEK_SET);
}

/// Message helper for the VM: the class name of a user stream.
pub fn class_of(stream: &Stream) -> Option<String> {
    stream.ops_as::<UserStreamOps>().map(|o| o.class.clone())
}

#[cfg(test)]
mod tests;
