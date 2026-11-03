//! Thin, safe wrappers over the BSD socket API and friends.
//!
//! This is the only module of the crate containing `unsafe` code.  Every
//! wrapper takes and returns owned Rust data; failures are the raw `errno`
//! (`Err(errno)`).  Socket addresses travel as [`SockAddr`], a
//! `sockaddr_storage` plus its length, built and decoded here.

#![allow(unsafe_code)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_uint, c_void};

extern "C" {
    fn sockatmark(fd: c_int) -> c_int;
    fn gethostbyname_r(
        name: *const c_char,
        ret: *mut libc::hostent,
        buf: *mut c_char,
        buflen: libc::size_t,
        result: *mut *mut libc::hostent,
        h_errnop: *mut c_int,
    ) -> c_int;
    fn __h_errno_location() -> *mut c_int;
    fn inet_pton(af: c_int, src: *const c_char, dst: *mut c_void) -> c_int;
    fn inet_ntop(af: c_int, src: *const c_void, dst: *mut c_char, size: libc::socklen_t) -> *const c_char;
}

/// Reads the calling thread's `errno`.
pub fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Sets the calling thread's `errno`.
pub fn set_errno(value: i32) {
    // SAFETY: `__errno_location` returns the calling thread's errno slot.
    unsafe { *libc::__errno_location() = value };
}

/// The calling thread's `h_errno`.
pub fn h_errno() -> i32 {
    // SAFETY: glibc returns the calling thread's h_errno slot.
    unsafe { *__h_errno_location() }
}

/// `strerror()` text (`"Unknown error N"` for unknown values).
pub fn strerror(code: i32) -> String {
    let mut buf = [0 as c_char; 1024];
    // SAFETY: `buf` is writable for its whole length; the XSI strerror_r
    // always NUL-terminates what it writes.
    unsafe { libc::strerror_r(code, buf.as_mut_ptr(), buf.len()) };
    cbuf_string(&buf)
}

/// `hstrerror()` text.
pub fn hstrerror(code: i32) -> String {
    // SAFETY: hstrerror returns a static string (never null in glibc).
    let p = unsafe { libc::hstrerror(code) };
    if p.is_null() {
        return String::new();
    }
    // SAFETY: non-null static NUL-terminated string.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn cbuf_string(buf: &[c_char]) -> String {
    #[allow(clippy::cast_sign_loss)]
    let bytes: Vec<u8> = buf.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn check(ret: c_int) -> Result<c_int, i32> {
    if ret < 0 {
        Err(errno())
    } else {
        Ok(ret)
    }
}

fn check_size(ret: isize) -> Result<usize, i32> {
    usize::try_from(ret).map_err(|_| errno())
}

// ------------------------------------------------------------- addresses

/// A socket address (`sockaddr_storage` + length).
#[derive(Clone, Copy)]
pub struct SockAddr {
    storage: libc::sockaddr_storage,
    len: libc::socklen_t,
}

impl std::fmt::Debug for SockAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SockAddr")
            .field("family", &self.family())
            .field("len", &self.len)
            .finish()
    }
}

/// `offsetof(struct sockaddr_un, sun_path)`.
pub const SUN_PATH_OFFSET: usize = 2;
/// `sizeof(sun_path)`.
pub const SUN_PATH_LEN: usize = 108;

#[allow(clippy::cast_possible_truncation)]
fn socklen(n: usize) -> libc::socklen_t {
    n as libc::socklen_t
}

impl SockAddr {
    /// An all-zero address with room for any family.
    pub fn empty() -> Self {
        Self {
            // SAFETY: sockaddr_storage is plain old data; zero is valid.
            storage: unsafe { std::mem::zeroed() },
            len: socklen(std::mem::size_of::<libc::sockaddr_storage>()),
        }
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: sockaddr_storage is plain bytes of this size.
        unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(&self.storage).cast::<u8>(),
                std::mem::size_of::<libc::sockaddr_storage>(),
            )
        }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in `bytes`; every bit pattern is valid.
        unsafe {
            std::slice::from_raw_parts_mut(
                std::ptr::from_mut(&mut self.storage).cast::<u8>(),
                std::mem::size_of::<libc::sockaddr_storage>(),
            )
        }
    }

    /// Builds an address from raw bytes (at most `sizeof(sockaddr_storage)`).
    pub fn from_raw(raw: &[u8], len: usize) -> Self {
        let mut a = Self::empty();
        let n = raw.len().min(a.bytes().len());
        a.bytes_mut()[..n].copy_from_slice(&raw[..n]);
        a.len = socklen(len.min(std::mem::size_of::<libc::sockaddr_storage>()));
        a
    }

    /// The raw bytes and length (for building control messages etc.).
    pub fn raw(&self) -> (&[u8], usize) {
        (self.bytes(), self.len as usize)
    }

    /// `sockaddr_in`.
    pub fn inet(ip: [u8; 4], port: u16) -> Self {
        let mut a = Self::empty();
        let b = a.bytes_mut();
        b[..2].copy_from_slice(&(libc::AF_INET as u16).to_ne_bytes());
        b[2..4].copy_from_slice(&port.to_be_bytes());
        b[4..8].copy_from_slice(&ip);
        a.len = socklen(std::mem::size_of::<libc::sockaddr_in>());
        a
    }

    /// `sockaddr_in6`.
    pub fn inet6(ip: [u8; 16], port: u16, flowinfo: u32, scope_id: u32) -> Self {
        let mut a = Self::empty();
        let b = a.bytes_mut();
        b[..2].copy_from_slice(&(libc::AF_INET6 as u16).to_ne_bytes());
        b[2..4].copy_from_slice(&port.to_be_bytes());
        b[4..8].copy_from_slice(&flowinfo.to_be_bytes());
        b[8..24].copy_from_slice(&ip);
        b[24..28].copy_from_slice(&scope_id.to_ne_bytes());
        a.len = socklen(std::mem::size_of::<libc::sockaddr_in6>());
        a
    }

    /// `sockaddr_un` with `path` (shorter than `sun_path`) copied in and the
    /// given length.
    pub fn unix(path: &[u8], len: usize) -> Self {
        let mut a = Self::empty();
        let b = a.bytes_mut();
        b[..2].copy_from_slice(&(libc::AF_UNIX as u16).to_ne_bytes());
        let n = path.len().min(SUN_PATH_LEN);
        b[SUN_PATH_OFFSET..SUN_PATH_OFFSET + n].copy_from_slice(&path[..n]);
        a.len = socklen(len);
        a
    }

    /// `sockaddr_ll` with only family and ifindex set.
    pub fn packet(ifindex: i32) -> Self {
        let mut a = Self::empty();
        let b = a.bytes_mut();
        b[..2].copy_from_slice(&(libc::AF_PACKET as u16).to_ne_bytes());
        b[4..8].copy_from_slice(&ifindex.to_ne_bytes());
        a.len = socklen(std::mem::size_of::<libc::sockaddr_ll>());
        a
    }

    /// `sa_family`.
    pub fn family(&self) -> i32 {
        i32::from(u16::from_ne_bytes([self.bytes()[0], self.bytes()[1]]))
    }

    /// Port of an INET/INET6 address (host order).
    pub fn port(&self) -> u16 {
        u16::from_be_bytes([self.bytes()[2], self.bytes()[3]])
    }

    /// IPv4 address bytes.
    pub fn ip4(&self) -> [u8; 4] {
        let mut o = [0u8; 4];
        o.copy_from_slice(&self.bytes()[4..8]);
        o
    }

    /// IPv6 address bytes.
    pub fn ip6(&self) -> [u8; 16] {
        let mut o = [0u8; 16];
        o.copy_from_slice(&self.bytes()[8..24]);
        o
    }

    /// `sin6_flowinfo` (host order).
    pub fn flowinfo(&self) -> u32 {
        let b = self.bytes();
        u32::from_be_bytes([b[4], b[5], b[6], b[7]])
    }

    /// `sin6_scope_id`.
    pub fn scope_id(&self) -> u32 {
        let b = self.bytes();
        u32::from_ne_bytes([b[24], b[25], b[26], b[27]])
    }

    /// The whole `sun_path` array.
    pub fn sun_path(&self) -> &[u8] {
        &self.bytes()[SUN_PATH_OFFSET..SUN_PATH_OFFSET + SUN_PATH_LEN]
    }

    /// `sll_ifindex`.
    pub fn ll_ifindex(&self) -> i32 {
        let b = self.bytes();
        i32::from_ne_bytes([b[4], b[5], b[6], b[7]])
    }

    /// Sets `sll_ifindex` (AF_PACKET bind keeps the rest from getsockname).
    pub fn set_ll_ifindex(&mut self, ifindex: i32) {
        self.bytes_mut()[4..8].copy_from_slice(&ifindex.to_ne_bytes());
    }

    /// Overrides the length.
    pub fn set_len(&mut self, len: usize) {
        self.len = socklen(len);
    }

    fn as_ptr(&self) -> *const libc::sockaddr {
        std::ptr::from_ref(&self.storage).cast()
    }

    fn as_mut_ptr(&mut self) -> *mut libc::sockaddr {
        std::ptr::from_mut(&mut self.storage).cast()
    }
}

/// `inet_pton(AF_INET)`.
pub fn pton4(s: &CStr) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    // SAFETY: valid C string; `out` has room for an in_addr.
    let r = unsafe { inet_pton(libc::AF_INET, s.as_ptr(), out.as_mut_ptr().cast()) };
    (r == 1).then_some(out)
}

/// `inet_pton(AF_INET6)`.
pub fn pton6(s: &CStr) -> Option<[u8; 16]> {
    let mut out = [0u8; 16];
    // SAFETY: valid C string; `out` has room for an in6_addr.
    let r = unsafe { inet_pton(libc::AF_INET6, s.as_ptr(), out.as_mut_ptr().cast()) };
    (r == 1).then_some(out)
}

fn ntop(af: c_int, src: &[u8]) -> String {
    let mut buf = [0 as c_char; 64];
    // SAFETY: `src` holds an in_addr/in6_addr for `af`; `buf` is larger
    // than INET6_ADDRSTRLEN.
    let p = unsafe { inet_ntop(af, src.as_ptr().cast(), buf.as_mut_ptr(), socklen(buf.len())) };
    if p.is_null() {
        String::new()
    } else {
        cbuf_string(&buf)
    }
}

/// `inet_ntop(AF_INET)`.
pub fn ntop4(ip: [u8; 4]) -> String {
    ntop(libc::AF_INET, &ip)
}

/// `inet_ntop(AF_INET6)`.
pub fn ntop6(ip: [u8; 16]) -> String {
    ntop(libc::AF_INET6, &ip)
}

/// `gethostbyname_r()`: the address type and first address, or `h_errno`.
pub fn gethostbyname(name: &CStr) -> Result<(i32, Vec<u8>), i32> {
    // SAFETY: hostent is plain old data; zero is valid.
    let mut he: libc::hostent = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as c_char; 16 * 1024];
    let mut result: *mut libc::hostent = std::ptr::null_mut();
    let mut herr: c_int = 0;
    // SAFETY: all pointers are valid for the sizes passed.
    let r = unsafe {
        gethostbyname_r(name.as_ptr(), &raw mut he, buf.as_mut_ptr(), buf.len(), &raw mut result, &raw mut herr)
    };
    if r != 0 || result.is_null() {
        return Err(if herr == 0 { 1 } else { herr });
    }
    #[allow(clippy::cast_sign_loss)]
    let len = he.h_length as usize;
    if he.h_addr_list.is_null() {
        return Err(4);
    }
    // SAFETY: h_addr_list is a NULL-terminated array of h_length-byte
    // addresses inside `buf`.
    let first = unsafe { *he.h_addr_list };
    if first.is_null() {
        return Err(4);
    }
    // SAFETY: `first` points to `len` bytes.
    let addr = unsafe { std::slice::from_raw_parts(first.cast::<u8>(), len) }.to_vec();
    Ok((he.h_addrtype, addr))
}

/// One `getaddrinfo()` result.
#[derive(Debug, Clone)]
pub struct AddrInfoEntry {
    pub flags: i32,
    pub family: i32,
    pub socktype: i32,
    pub protocol: i32,
    pub addr: SockAddr,
    pub canonname: Option<Vec<u8>>,
}

/// `getaddrinfo()`; `Err` carries the EAI code and `h_errno` afterwards.
pub fn getaddrinfo(
    host: &CStr,
    service: Option<&CStr>,
    flags: i32,
    family: i32,
    socktype: i32,
    protocol: i32,
) -> Result<Vec<AddrInfoEntry>, (i32, i32)> {
    // SAFETY: addrinfo is plain old data; zero is valid.
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_flags = flags;
    hints.ai_family = family;
    hints.ai_socktype = socktype;
    hints.ai_protocol = protocol;
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: valid C strings / null, valid hints and out pointer.
    let r = unsafe {
        libc::getaddrinfo(
            host.as_ptr(),
            service.map_or(std::ptr::null(), CStr::as_ptr),
            &raw const hints,
            &raw mut res,
        )
    };
    if r != 0 {
        return Err((r, h_errno()));
    }
    let mut out = Vec::new();
    let mut p = res;
    while !p.is_null() {
        // SAFETY: `p` walks the list returned by getaddrinfo.
        let ai = unsafe { &*p };
        let alen = ai.ai_addrlen as usize;
        let raw = if ai.ai_addr.is_null() {
            Vec::new()
        } else {
            // SAFETY: ai_addr points to ai_addrlen bytes.
            unsafe { std::slice::from_raw_parts(ai.ai_addr.cast::<u8>(), alen) }.to_vec()
        };
        let canonname = if ai.ai_canonname.is_null() {
            None
        } else {
            // SAFETY: NUL-terminated string owned by the list.
            Some(unsafe { CStr::from_ptr(ai.ai_canonname) }.to_bytes().to_vec())
        };
        out.push(AddrInfoEntry {
            flags: ai.ai_flags,
            family: ai.ai_family,
            socktype: ai.ai_socktype,
            protocol: ai.ai_protocol,
            addr: SockAddr::from_raw(&raw, alen),
            canonname,
        });
        p = ai.ai_next;
    }
    // SAFETY: `res` came from getaddrinfo and is freed once.
    unsafe { libc::freeaddrinfo(res) };
    Ok(out)
}

/// `if_nametoindex()` (0 when unknown).
pub fn if_nametoindex(name: &CStr) -> u32 {
    // SAFETY: valid C string.
    unsafe { libc::if_nametoindex(name.as_ptr()) }
}

/// `if_indextoname()`.
pub fn if_indextoname(index: u32) -> Option<Vec<u8>> {
    let mut buf = [0 as c_char; libc::IF_NAMESIZE];
    // SAFETY: `buf` has IF_NAMESIZE bytes as required.
    let p = unsafe { libc::if_indextoname(index, buf.as_mut_ptr()) };
    if p.is_null() {
        None
    } else {
        Some(cbuf_string(&buf).into_bytes())
    }
}

/// `php_if_index_to_addr4`'s ioctls: `SIOCGIFNAME` then `SIOCGIFADDR` on
/// `fd`; `Err(errno)` on failure.
pub fn if_index_to_addr4(fd: c_int, index: u32) -> Result<[u8; 4], i32> {
    // SAFETY: ifreq is plain old data; zero is valid.
    let mut req: libc::ifreq = unsafe { std::mem::zeroed() };
    #[allow(clippy::cast_possible_wrap)]
    {
        req.ifr_ifru.ifru_ifindex = index as c_int;
    }
    // SAFETY: SIOCGIFNAME reads ifr_ifindex and writes ifr_name.
    check(unsafe { libc::ioctl(fd, libc::SIOCGIFNAME, &raw mut req) })?;
    // SAFETY: SIOCGIFADDR reads ifr_name and writes ifr_addr.
    check(unsafe { libc::ioctl(fd, libc::SIOCGIFADDR, &raw mut req) })?;
    // SAFETY: after SIOCGIFADDR the union holds a sockaddr (AF_INET).
    let sa = unsafe { req.ifr_ifru.ifru_addr };
    #[allow(clippy::cast_sign_loss)]
    Ok([sa.sa_data[2] as u8, sa.sa_data[3] as u8, sa.sa_data[4] as u8, sa.sa_data[5] as u8])
}

/// The interface index owning an IPv4 address (via `getifaddrs`).
pub fn addr4_to_if_index(ip: [u8; 4]) -> Result<Option<u32>, i32> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: out pointer is valid.
    check(unsafe { libc::getifaddrs(&raw mut list) })?;
    let mut found = None;
    let mut p = list;
    while !p.is_null() {
        // SAFETY: walking the list returned by getifaddrs.
        let ifa = unsafe { &*p };
        if !ifa.ifa_addr.is_null() {
            // SAFETY: ifa_addr points to a sockaddr of its family.
            let fam = i32::from(unsafe { (*ifa.ifa_addr).sa_family });
            if fam == libc::AF_INET {
                // SAFETY: AF_INET → sockaddr_in.
                let sin = unsafe { &*ifa.ifa_addr.cast::<libc::sockaddr_in>() };
                if sin.sin_addr.s_addr.to_ne_bytes() == ip {
                    // SAFETY: ifa_name is a NUL-terminated string.
                    let idx = unsafe { libc::if_nametoindex(ifa.ifa_name) };
                    found = Some(idx);
                    break;
                }
            }
        }
        p = ifa.ifa_next;
    }
    // SAFETY: `list` came from getifaddrs and is freed once.
    unsafe { libc::freeifaddrs(list) };
    Ok(found)
}

// --------------------------------------------------------------- sockets

/// `socket(2)`.
pub fn socket(domain: c_int, ty: c_int, protocol: c_int) -> Result<c_int, i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe { libc::socket(domain, ty, protocol) })
}

/// `socketpair(2)`.
pub fn socketpair(domain: c_int, ty: c_int, protocol: c_int) -> Result<(c_int, c_int), i32> {
    let mut fds = [0 as c_int; 2];
    // SAFETY: `fds` has room for two descriptors.
    check(unsafe { libc::socketpair(domain, ty, protocol, fds.as_mut_ptr()) })?;
    Ok((fds[0], fds[1]))
}

/// `close(2)`.
pub fn close(fd: c_int) {
    // SAFETY: closing a descriptor we own.
    unsafe { libc::close(fd) };
}

/// `bind(2)`.
pub fn bind(fd: c_int, addr: &SockAddr) -> Result<(), i32> {
    // SAFETY: `addr` is valid for `addr.len` bytes.
    check(unsafe { libc::bind(fd, addr.as_ptr(), addr.len) }).map(|_| ())
}

/// `connect(2)`.
pub fn connect(fd: c_int, addr: &SockAddr) -> Result<(), i32> {
    // SAFETY: `addr` is valid for `addr.len` bytes.
    check(unsafe { libc::connect(fd, addr.as_ptr(), addr.len) }).map(|_| ())
}

/// `listen(2)`.
pub fn listen(fd: c_int, backlog: c_int) -> Result<(), i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe { libc::listen(fd, backlog) }).map(|_| ())
}

/// `accept4(2)`.
pub fn accept4(fd: c_int, flags: c_int) -> Result<(c_int, SockAddr), i32> {
    let mut a = SockAddr::empty();
    let mut len = a.len;
    // SAFETY: `a` has room for any address; `len` holds its size.
    let r = check(unsafe { libc::accept4(fd, a.as_mut_ptr(), &raw mut len, flags) })?;
    a.len = len;
    Ok((r, a))
}

/// `getsockname(2)`.
pub fn getsockname(fd: c_int) -> Result<SockAddr, i32> {
    let mut a = SockAddr::empty();
    let mut len = a.len;
    // SAFETY: as for accept4.
    check(unsafe { libc::getsockname(fd, a.as_mut_ptr(), &raw mut len) })?;
    a.len = len;
    Ok(a)
}

/// `getpeername(2)`.
pub fn getpeername(fd: c_int) -> Result<SockAddr, i32> {
    let mut a = SockAddr::empty();
    let mut len = a.len;
    // SAFETY: as for accept4.
    check(unsafe { libc::getpeername(fd, a.as_mut_ptr(), &raw mut len) })?;
    a.len = len;
    Ok(a)
}

/// `write(2)`.
pub fn write(fd: c_int, data: &[u8]) -> Result<usize, i32> {
    // SAFETY: `data` is readable for its length.
    check_size(unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) })
}

/// `recv(2)` into `buf`.
pub fn recv(fd: c_int, buf: &mut [u8], flags: c_int) -> Result<usize, i32> {
    // SAFETY: `buf` is writable for its length.
    check_size(unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), flags) })
}

/// `send(2)`.
pub fn send(fd: c_int, data: &[u8], flags: c_int) -> Result<usize, i32> {
    // SAFETY: `data` is readable for its length.
    check_size(unsafe { libc::send(fd, data.as_ptr().cast(), data.len(), flags) })
}

/// `recvfrom(2)`; `addr` is the initial address buffer (family preset as
/// PHP does) and `addr_len` the length passed in.
pub fn recvfrom(fd: c_int, buf: &mut [u8], flags: c_int, mut addr: SockAddr, addr_len: usize) -> Result<(usize, SockAddr), i32> {
    let mut len = socklen(addr_len);
    // SAFETY: `buf` is writable; `addr` has room for `addr_len` bytes.
    let n = check_size(unsafe {
        libc::recvfrom(fd, buf.as_mut_ptr().cast(), buf.len(), flags, addr.as_mut_ptr(), &raw mut len)
    })?;
    addr.len = len;
    Ok((n, addr))
}

/// `sendto(2)`.
pub fn sendto(fd: c_int, data: &[u8], flags: c_int, addr: &SockAddr) -> Result<usize, i32> {
    // SAFETY: `data` readable; `addr` valid for its length.
    check_size(unsafe { libc::sendto(fd, data.as_ptr().cast(), data.len(), flags, addr.as_ptr(), addr.len) })
}

/// `getsockopt(2)` into `buf`; returns the length written.
pub fn getsockopt(fd: c_int, level: c_int, name: c_int, buf: &mut [u8]) -> Result<usize, i32> {
    let mut len = socklen(buf.len());
    // SAFETY: `buf` is writable for `len` bytes.
    check(unsafe { libc::getsockopt(fd, level, name, buf.as_mut_ptr().cast(), &raw mut len) })?;
    Ok(len as usize)
}

/// `setsockopt(2)`.
pub fn setsockopt(fd: c_int, level: c_int, name: c_int, value: &[u8]) -> Result<(), i32> {
    let p: *const c_void = if value.is_empty() { std::ptr::null() } else { value.as_ptr().cast() };
    // SAFETY: `value` readable for its length (or null with length 0).
    check(unsafe { libc::setsockopt(fd, level, name, p, socklen(value.len())) }).map(|_| ())
}

/// `fcntl(F_GETFL)`.
pub fn get_flags(fd: c_int) -> Result<c_int, i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe { libc::fcntl(fd, libc::F_GETFL) })
}

/// `php_set_sock_blocking()`.
pub fn set_blocking(fd: c_int, block: bool) -> Result<(), i32> {
    let flags = get_flags(fd)?;
    let new = if block { flags & !libc::O_NONBLOCK } else { flags | libc::O_NONBLOCK };
    // SAFETY: no memory-safety preconditions.
    check(unsafe { libc::fcntl(fd, libc::F_SETFL, new) }).map(|_| ())
}

/// `shutdown(2)`.
pub fn shutdown(fd: c_int, how: c_int) -> Result<(), i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe { libc::shutdown(fd, how) }).map(|_| ())
}

/// `sockatmark(3)`.
pub fn atmark(fd: c_int) -> Result<bool, i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe { sockatmark(fd) }).map(|r| r > 0)
}

/// Whether `fd` refers to a socket (`fstat` + `S_ISSOCK`).
pub fn is_socket(fd: c_int) -> Result<bool, i32> {
    // SAFETY: stat is plain old data; zero is valid.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `st` is writable.
    check(unsafe { libc::fstat(fd, &raw mut st) })?;
    Ok(st.st_mode & libc::S_IFMT == libc::S_IFSOCK)
}

/// `FD_SETSIZE`.
pub const FD_SETSIZE: i32 = 1024;

/// `select(2)` over three descriptor lists (descriptors `>= FD_SETSIZE` are
/// skipped, like `PHP_SAFE_FD_SET`); returns the count and the ready
/// descriptors of each list.
pub fn select(
    sets: [&[c_int]; 3],
    max_fd: c_int,
    timeout: Option<(i64, i64)>,
) -> Result<(i32, [Vec<c_int>; 3]), i32> {
    // SAFETY: fd_set is plain old data; FD_ZERO initialises it.
    let mut fds: [libc::fd_set; 3] = unsafe { std::mem::zeroed() };
    for (set, list) in fds.iter_mut().zip(sets.iter()) {
        // SAFETY: `set` is a valid fd_set.
        unsafe { libc::FD_ZERO(set) };
        for &fd in *list {
            if (0..FD_SETSIZE).contains(&fd) {
                // SAFETY: 0 <= fd < FD_SETSIZE.
                unsafe { libc::FD_SET(fd, set) };
            }
        }
    }
    let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
    let tvp = match timeout {
        Some((s, us)) => {
            tv.tv_sec = s;
            tv.tv_usec = us;
            &raw mut tv
        }
        None => std::ptr::null_mut(),
    };
    let [r, w, e] = &mut fds;
    // SAFETY: all sets are valid; tvp is null or valid.
    let n = check(unsafe { libc::select(max_fd + 1, r, w, e, tvp) })?;
    let mut out: [Vec<c_int>; 3] = Default::default();
    for (i, list) in sets.iter().enumerate() {
        out[i] = list
            .iter()
            .copied()
            // SAFETY: the set is valid and fd is in range.
            .filter(|&fd| (0..FD_SETSIZE).contains(&fd) && unsafe { libc::FD_ISSET(fd, &fds[i]) })
            .collect();
    }
    Ok((n, out))
}

// ------------------------------------------------------- sendmsg/recvmsg

/// `sizeof(struct cmsghdr)` rounded as `CMSG_LEN(0)`.
pub fn cmsg_len(data: usize) -> usize {
    #[allow(clippy::cast_possible_truncation)]
    // SAFETY: pure arithmetic macro.
    let r = unsafe { libc::CMSG_LEN(data as c_uint) };
    r as usize
}

/// `CMSG_SPACE(data)`.
pub fn cmsg_space(data: usize) -> usize {
    #[allow(clippy::cast_possible_truncation)]
    // SAFETY: pure arithmetic macro.
    let r = unsafe { libc::CMSG_SPACE(data as c_uint) };
    r as usize
}

/// `sendmsg(2)` from owned parts; `control` is a fully encoded control
/// buffer (see [`cmsg_len`]/[`cmsg_space`]).
pub fn sendmsg(fd: c_int, name: Option<&SockAddr>, iov: &[Vec<u8>], control: &[u8], flags: c_int) -> Result<usize, i32> {
    let mut iovecs: Vec<libc::iovec> = iov
        .iter()
        .map(|b| libc::iovec { iov_base: b.as_ptr().cast_mut().cast(), iov_len: b.len() })
        .collect();
    // SAFETY: msghdr is plain old data; zero is valid.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    let mut name_copy = name.copied();
    if let Some(n) = name_copy.as_mut() {
        msg.msg_name = n.as_mut_ptr().cast();
        msg.msg_namelen = n.len;
    }
    if !iovecs.is_empty() {
        msg.msg_iov = iovecs.as_mut_ptr();
        msg.msg_iovlen = iovecs.len();
    }
    let mut ctl = control.to_vec();
    if !ctl.is_empty() {
        msg.msg_control = ctl.as_mut_ptr().cast();
        msg.msg_controllen = ctl.len();
    }
    // SAFETY: every pointer in `msg` refers to live buffers of the stated
    // sizes; the kernel only reads them.
    check_size(unsafe { libc::sendmsg(fd, &raw const msg, flags) })
}

/// The result of [`recvmsg`].
#[derive(Debug)]
pub struct Received {
    pub len: usize,
    pub name: Option<SockAddr>,
    pub iov: Vec<Vec<u8>>,
    pub control: Vec<u8>,
    pub flags: i32,
}

/// `recvmsg(2)` into one buffer per `iov_sizes` entry, a name buffer when
/// `name_len` is given and a control buffer of `control_len` bytes.
pub fn recvmsg(fd: c_int, name_len: Option<usize>, iov_sizes: &[usize], control_len: usize, flags: c_int) -> Result<Received, i32> {
    let mut bufs: Vec<Vec<u8>> = iov_sizes.iter().map(|&n| vec![0u8; n]).collect();
    let mut iovecs: Vec<libc::iovec> = bufs
        .iter_mut()
        .map(|b| libc::iovec { iov_base: b.as_mut_ptr().cast(), iov_len: b.len() })
        .collect();
    // SAFETY: msghdr is plain old data; zero is valid.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    let mut name = SockAddr::empty();
    if let Some(n) = name_len {
        msg.msg_name = name.as_mut_ptr().cast();
        msg.msg_namelen = socklen(n.min(std::mem::size_of::<libc::sockaddr_storage>()));
    }
    if !iovecs.is_empty() {
        msg.msg_iov = iovecs.as_mut_ptr();
        msg.msg_iovlen = iovecs.len();
    }
    let mut ctl = vec![0u8; control_len];
    if control_len > 0 {
        msg.msg_control = ctl.as_mut_ptr().cast();
        msg.msg_controllen = control_len;
    }
    // SAFETY: every pointer in `msg` refers to live, writable buffers of
    // the stated sizes.
    let n = check_size(unsafe { libc::recvmsg(fd, &raw mut msg, flags) })?;
    ctl.truncate(msg.msg_controllen.min(control_len));
    name.len = msg.msg_namelen;
    Ok(Received {
        len: n,
        name: name_len.map(|_| name),
        iov: bufs,
        control: ctl,
        flags: msg.msg_flags,
    })
}

/// `getuid()`/`getgid()`/`getpid()` (credentials tests).
#[cfg(test)]
pub fn ids() -> (u32, u32, i32) {
    // SAFETY: no preconditions.
    unsafe { (libc::getuid(), libc::getgid(), libc::getpid()) }
}

/// A C string from bytes up to the first NUL (C's view of a PHP string).
pub fn cstr(bytes: &[u8]) -> CString {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    CString::new(&bytes[..end]).unwrap_or_default()
}

/// `SO_ATTACH_REUSEPORT_CBPF` with PHP's two-instruction program
/// (`LD W ABS SKF_AD_OFF + k; RET A`).
pub fn attach_reuseport_cbpf(fd: c_int, k: u32) -> Result<(), i32> {
    let mut prog = [
        libc::sock_filter { code: 0x20, jt: 0, jf: 0, k: (libc::SKF_AD_OFF as u32).wrapping_add(k) },
        libc::sock_filter { code: 0x16, jt: 0, jf: 0, k: 0 },
    ];
    let fprog = libc::sock_fprog { len: 2, filter: prog.as_mut_ptr() };
    // SAFETY: `fprog` points to `prog`, which outlives the call.
    check(unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ATTACH_REUSEPORT_CBPF,
            std::ptr::from_ref(&fprog).cast(),
            socklen(std::mem::size_of::<libc::sock_fprog>()),
        )
    })
    .map(|_| ())
}
