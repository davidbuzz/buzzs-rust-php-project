//! The polling core behind `Io\Poll` (`main/poll/`): the `poll(2)` and
//! `epoll(7)` backends with `PHP_POLL_*` event bits, one-shot and
//! edge-triggered registrations and `php_poll_error` codes.

use std::collections::BTreeMap;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::time::Duration;

use crate::sys;

/// `PHP_POLL_READ`.
pub const READ: u32 = 0x01;
/// `PHP_POLL_WRITE`.
pub const WRITE: u32 = 0x02;
/// `PHP_POLL_ERROR`.
pub const ERROR: u32 = 0x04;
/// `PHP_POLL_HUP`.
pub const HUP: u32 = 0x08;
/// `PHP_POLL_RDHUP`.
pub const RDHUP: u32 = 0x10;
/// `PHP_POLL_ONESHOT`.
pub const ONESHOT: u32 = 0x20;
/// `PHP_POLL_ET`.
pub const ET: u32 = 0x40;

/// `php_poll_error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollError {
    /// `PHP_POLL_ERR_NONE`.
    None = 0,
    /// Generic system error.
    System = 1,
    /// `ENOMEM`.
    NoMem = 2,
    /// `EINVAL`, `EBADF`.
    Invalid = 3,
    /// `EEXIST`.
    Exists = 4,
    /// `ENOENT`.
    NotFound = 5,
    /// `ETIME`, `ETIMEDOUT`.
    Timeout = 6,
    /// `EINTR`.
    Interrupted = 7,
    /// `EACCES`, `EPERM`.
    Permission = 8,
    /// `EMFILE`, `ENFILE`.
    TooBig = 9,
    /// `EAGAIN`.
    Again = 10,
    /// `ENOSYS`, `ENOTSUP`.
    NoSupport = 11,
}

impl PollError {
    /// `php_poll_errno_to_error()`.
    pub fn from_errno(e: i32) -> Self {
        match e {
            0 => PollError::None,
            libc::ENOMEM => PollError::NoMem,
            libc::EINVAL | libc::EBADF => PollError::Invalid,
            libc::EEXIST => PollError::Exists,
            libc::ENOENT => PollError::NotFound,
            libc::ETIME | libc::ETIMEDOUT => PollError::Timeout,
            libc::EINTR => PollError::Interrupted,
            libc::EACCES | libc::EPERM => PollError::Permission,
            libc::EMFILE | libc::ENFILE => PollError::TooBig,
            libc::EAGAIN => PollError::Again,
            libc::ENOSYS | libc::ENOTSUP => PollError::NoSupport,
            _ => PollError::System,
        }
    }

    /// The `FailedPollOperationException::ERROR_*` code.
    pub fn code(self) -> i64 {
        self as i64
    }
}

/// `php_poll_backend_type` (the `Io\Poll\Backend` cases after `Auto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    /// `Backend::Auto`.
    Auto,
    /// `poll(2)`.
    Poll,
    /// `epoll(7)`.
    Epoll,
    /// BSD `kqueue` (unavailable here).
    Kqueue,
    /// Solaris event ports (unavailable here).
    EventPorts,
    /// Windows `WSAPoll` (unavailable here).
    WsaPoll,
}

impl BackendType {
    /// The enum case name.
    pub fn name(self) -> &'static str {
        match self {
            BackendType::Auto => "Auto",
            BackendType::Poll => "Poll",
            BackendType::Epoll => "Epoll",
            BackendType::Kqueue => "Kqueue",
            BackendType::EventPorts => "EventPorts",
            BackendType::WsaPoll => "WSAPoll",
        }
    }

    /// Parses an enum case name.
    pub fn from_name(n: &str) -> Option<Self> {
        Some(match n {
            "Auto" => BackendType::Auto,
            "Poll" => BackendType::Poll,
            "Epoll" => BackendType::Epoll,
            "Kqueue" => BackendType::Kqueue,
            "EventPorts" => BackendType::EventPorts,
            "WSAPoll" => BackendType::WsaPoll,
            _ => return None,
        })
    }

    /// `php_poll_is_backend_available()`.
    pub fn is_available(self) -> bool {
        match self {
            BackendType::Auto | BackendType::Poll => true,
            BackendType::Epoll => sys::epoll_create().is_ok(),
            _ => false,
        }
    }

    /// `php_poll_backend_supports_edge_triggering()`.
    pub fn supports_edge_triggering(self) -> bool {
        matches!(self, BackendType::Auto | BackendType::Epoll)
            && self.resolve().is_some()
            && self.resolve() == Some(BackendType::Epoll)
    }

    /// The concrete backend `Auto` selects.
    fn resolve(self) -> Option<Self> {
        match self {
            BackendType::Auto => Some(if BackendType::Epoll.is_available() {
                BackendType::Epoll
            } else {
                BackendType::Poll
            }),
            b if b.is_available() => Some(b),
            _ => None,
        }
    }

    /// `Backend::getAvailableBackends()` order.
    pub fn available() -> Vec<Self> {
        [
            BackendType::Poll,
            BackendType::Epoll,
            BackendType::Kqueue,
            BackendType::EventPorts,
            BackendType::WsaPoll,
        ]
        .into_iter()
        .filter(|b| b.is_available())
        .collect()
    }
}

/// One reported event (`php_poll_event`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollEvent {
    /// The descriptor.
    pub fd: RawFd,
    /// `PHP_POLL_*` bits that fired.
    pub revents: u32,
}

#[derive(Debug)]
enum Backend {
    Poll(BTreeMap<RawFd, u32>),
    Epoll { fd: OwnedFd, count: usize },
}

/// A polling context (`php_poll_ctx`).
#[derive(Debug)]
pub struct PollCtx {
    backend: Backend,
    kind: BackendType,
}

fn epoll_native(events: u32) -> u32 {
    let mut n = 0u32;
    for (bit, native) in [
        (READ, libc::EPOLLIN),
        (WRITE, libc::EPOLLOUT),
        (ERROR, libc::EPOLLERR),
        (HUP, libc::EPOLLHUP),
        (RDHUP, libc::EPOLLRDHUP),
        (ONESHOT, libc::EPOLLONESHOT),
        (ET, libc::EPOLLET),
    ] {
        if events & bit != 0 {
            n |= native.cast_unsigned();
        }
    }
    n
}

fn epoll_from_native(native: u32) -> u32 {
    let mut e = 0u32;
    for (bit, n) in [
        (READ, libc::EPOLLIN),
        (WRITE, libc::EPOLLOUT),
        (ERROR, libc::EPOLLERR),
        (HUP, libc::EPOLLHUP),
        (RDHUP, libc::EPOLLRDHUP),
    ] {
        if native & n.cast_unsigned() != 0 {
            e |= bit;
        }
    }
    e
}

fn poll_native(events: u32) -> i16 {
    let mut n = 0i16;
    for (bit, native) in [
        (READ, libc::POLLIN),
        (WRITE, libc::POLLOUT),
        (ERROR, libc::POLLERR),
        (HUP, libc::POLLHUP),
        (RDHUP, libc::POLLRDHUP),
    ] {
        if events & bit != 0 {
            n |= native;
        }
    }
    n
}

fn poll_from_native(native: i16) -> u32 {
    let mut e = 0u32;
    for (bit, n) in [
        (READ, libc::POLLIN),
        (WRITE, libc::POLLOUT),
        (ERROR, libc::POLLERR),
        (HUP, libc::POLLHUP),
        (RDHUP, libc::POLLRDHUP),
    ] {
        if native & n != 0 {
            e |= bit;
        }
    }
    e
}

/// `php_poll_timespec_to_ms()`: rounds up, caps at `i32::MAX`; `None`
/// waits forever.
fn timeout_ms(t: Option<Duration>) -> i32 {
    let Some(t) = t else { return -1 };
    if t.as_secs() >= u64::try_from((i32::MAX - 1000) / 1000).unwrap_or(0) {
        return i32::MAX;
    }
    let ms = t.as_secs() * 1000 + u64::from(t.subsec_nanos().div_ceil(1_000_000));
    i32::try_from(ms).unwrap_or(i32::MAX)
}

impl PollCtx {
    /// `php_poll_create()` + `php_poll_init()`: `Ok(None)` when the backend
    /// is unavailable.
    ///
    /// # Errors
    /// The initialisation failure.
    pub fn new(kind: BackendType) -> Result<Option<Self>, PollError> {
        let Some(kind) = kind.resolve() else {
            return Ok(None);
        };
        let backend = match kind {
            BackendType::Epoll => Backend::Epoll {
                fd: sys::epoll_create().map_err(PollError::from_errno)?,
                count: 0,
            },
            _ => Backend::Poll(BTreeMap::new()),
        };
        Ok(Some(PollCtx { backend, kind }))
    }

    /// The concrete backend.
    pub fn backend(&self) -> BackendType {
        self.kind
    }

    /// `php_poll_add()`.
    ///
    /// # Errors
    /// The backend failure (`NoSupport` for files that cannot be polled).
    pub fn add(&mut self, fd: RawFd, events: u32) -> Result<(), PollError> {
        if fd < 0 {
            return Err(PollError::Invalid);
        }
        match &mut self.backend {
            Backend::Poll(t) => {
                if events & ET != 0 {
                    return Err(PollError::NoSupport);
                }
                if t.contains_key(&fd) {
                    return Err(PollError::Exists);
                }
                t.insert(fd, events);
                Ok(())
            }
            Backend::Epoll { fd: ep, count } => {
                match sys::epoll_ctl(
                    ep.as_raw_fd(),
                    libc::EPOLL_CTL_ADD,
                    fd,
                    epoll_native(events),
                ) {
                    Ok(()) => {
                        *count += 1;
                        Ok(())
                    }
                    // Regular files and devices such as /dev/null.
                    Err(libc::EPERM) => Err(PollError::NoSupport),
                    Err(e) => Err(PollError::from_errno(e)),
                }
            }
        }
    }

    /// `php_poll_modify()`.
    ///
    /// # Errors
    /// `NotFound` when `fd` is not registered (e.g. a fired one-shot on the
    /// poll backend).
    pub fn modify(&mut self, fd: RawFd, events: u32) -> Result<(), PollError> {
        if fd < 0 {
            return Err(PollError::Invalid);
        }
        match &mut self.backend {
            Backend::Poll(t) => {
                if events & ET != 0 {
                    return Err(PollError::NoSupport);
                }
                let e = t.get_mut(&fd).ok_or(PollError::NotFound)?;
                *e = events;
                Ok(())
            }
            Backend::Epoll { fd: ep, .. } => sys::epoll_ctl(
                ep.as_raw_fd(),
                libc::EPOLL_CTL_MOD,
                fd,
                epoll_native(events),
            )
            .map_err(PollError::from_errno),
        }
    }

    /// `php_poll_remove()`.
    ///
    /// # Errors
    /// `NotFound` when `fd` is not registered.
    pub fn remove(&mut self, fd: RawFd) -> Result<(), PollError> {
        if fd < 0 {
            return Err(PollError::Invalid);
        }
        match &mut self.backend {
            Backend::Poll(t) => t.remove(&fd).map(|_| ()).ok_or(PollError::NotFound),
            Backend::Epoll { fd: ep, count } => {
                sys::epoll_ctl(ep.as_raw_fd(), libc::EPOLL_CTL_DEL, fd, 0)
                    .map_err(PollError::from_errno)?;
                *count = count.saturating_sub(1);
                Ok(())
            }
        }
    }

    /// `php_poll_get_suitable_max_events()`.
    pub fn suitable_max_events(&self) -> usize {
        let n = match &self.backend {
            Backend::Poll(t) => t.len(),
            Backend::Epoll { count, .. } => *count,
        };
        n.max(1)
    }

    /// `php_poll_wait()`.
    ///
    /// # Errors
    /// The failed system call (`Interrupted` for `EINTR`).
    pub fn wait(
        &mut self,
        max_events: usize,
        timeout: Option<Duration>,
    ) -> Result<Vec<PollEvent>, PollError> {
        let ms = timeout_ms(timeout);
        match &mut self.backend {
            Backend::Epoll { fd, .. } => {
                let evs = sys::epoll_wait(fd.as_raw_fd(), max_events, ms)
                    .map_err(PollError::from_errno)?;
                Ok(evs
                    .into_iter()
                    .map(|(fd, n)| PollEvent {
                        fd,
                        revents: epoll_from_native(n),
                    })
                    .collect())
            }
            Backend::Poll(t) => {
                if t.is_empty() {
                    if let Some(d) = timeout.filter(|d| !d.is_zero()) {
                        std::thread::sleep(d);
                    }
                    return Ok(Vec::new());
                }
                let fds: Vec<(RawFd, i16)> = t
                    .iter()
                    .map(|(&fd, &e)| (fd, poll_native(e & !(ET | ONESHOT))))
                    .collect();
                let rev = sys::poll_fds(&fds, ms).map_err(PollError::from_errno)?;
                let mut out = Vec::new();
                for (&(fd, _), &r) in fds.iter().zip(&rev) {
                    if r == 0 || out.len() >= max_events {
                        continue;
                    }
                    if r & libc::POLLNVAL != 0 {
                        t.remove(&fd);
                        continue;
                    }
                    out.push(PollEvent {
                        fd,
                        revents: poll_from_native(r),
                    });
                }
                for e in &out {
                    if t.get(&e.fd).is_some_and(|ev| ev & ONESHOT != 0) && e.revents != 0 {
                        t.remove(&e.fd);
                    }
                }
                Ok(out)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    #[test]
    fn epoll_and_poll_backends() {
        for kind in [BackendType::Epoll, BackendType::Poll] {
            let mut ctx = PollCtx::new(kind).expect("init").expect("available");
            assert_eq!(ctx.backend(), kind);
            let (a, mut b) = UnixStream::pair().expect("pair");
            ctx.add(a.as_raw_fd(), READ).expect("add");
            assert_eq!(ctx.add(a.as_raw_fd(), READ).is_err(), true);
            assert!(ctx.wait(4, Some(Duration::ZERO)).expect("wait").is_empty());
            b.write_all(b"x").expect("write");
            let evs = ctx.wait(4, Some(Duration::from_millis(100))).expect("wait");
            assert_eq!(
                evs,
                vec![PollEvent {
                    fd: a.as_raw_fd(),
                    revents: READ
                }]
            );
            ctx.remove(a.as_raw_fd()).expect("remove");
            assert_eq!(ctx.remove(a.as_raw_fd()), Err(PollError::NotFound));
        }
        assert!(PollCtx::new(BackendType::WsaPoll).expect("init").is_none());
        assert_eq!(timeout_ms(Some(Duration::from_micros(1500))), 2);
        assert_eq!(PollError::from_errno(libc::EINTR).code(), 7);
    }
}
