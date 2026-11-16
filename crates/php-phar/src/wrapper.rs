// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `phar://` stream and directory wrapper (`stream.c`, `dirstream.c`)
//! for `php-streams`.
//!
//! Register it with `php_streams::wrapper::register("phar", Rc::new(PharWrapper))`
//! (the VM does this at startup).

use std::any::Any;
use std::rc::Rc;

use php_streams::context::ContextRef;
use php_streams::diag::StreamErrorCode as Code;
use php_streams::stat::{mode as m, StatBuf};
use php_streams::stream::{OpFailed, Stream, StreamOps, StreamState, SEEK_CUR, SEEK_END, SEEK_SET};
use php_streams::wrapper::{
    wrapper_warn, OpenError, Wrapper, STREAM_OPEN_FOR_INCLUDE, URL_STAT_QUIET,
};

use crate::archive::{Archive, ArchiveRef, Entry, FpType};
use crate::consts::{ENT_COMPRESSION_MASK, ENT_PERM_DEF_DIR, ENT_PERM_MASK, TAR_DIR};
use crate::registry::{self as reg, EntryData, EntryRef};
use crate::util::{is_magic_phar, s};

const LABEL: &str = "phar";

/// `strerror()` text of an I/O error.
pub fn strerror(e: &std::io::Error) -> String {
    e.raw_os_error()
        .map_or_else(|| e.to_string(), php_streams::sys::strerror)
}

/// A parsed `phar://` URL (`php_url` with scheme/host/path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PharUrl {
    /// The archive path or alias (`resource->host`).
    pub host: Vec<u8>,
    /// The entry path, starting with `/` (`resource->path`).
    pub path: Vec<u8>,
}

/// A failure message of [`parse_url`] (`None` when quiet or no message).
type UrlError = Option<(Code, String)>;

/// `phar_parse_url()`.
pub fn parse_url(filename: &[u8], mode: &str, options: u32) -> Result<PharUrl, UrlError> {
    let quiet = options & URL_STAT_QUIET != 0;
    let say = |code: Code, msg: String| if quiet { None } else { Some((code, msg)) };
    if filename.len() < 7 || !filename[..7].eq_ignore_ascii_case(b"phar://") {
        return Err(None);
    }
    let first = mode.as_bytes().first().copied().unwrap_or(b'r');
    if first == b'a' {
        return Err(say(
            Code::ModeNotSupported,
            "phar error: open mode append not supported".into(),
        ));
    }
    let (host, path) = match reg::split_fname(filename, 2, if first == b'w' { 2 } else { 0 }) {
        Ok(x) => x,
        Err(e) => {
            return Err(match e.no_ext {
                Some(arch) => say(
                    Code::InvalidPath,
                    format!(
                        "phar error: no directory in \"{}\", must have at least phar://{}/ for root directory (always use full path to a new phar)",
                        s(filename),
                        s(&arch)
                    ),
                ),
                None => say(
                    Code::InvalidUrl,
                    format!("phar error: invalid url or non-existent phar \"{}\"", s(filename)),
                ),
            })
        }
    };
    let second = mode.as_bytes().get(1).copied().unwrap_or(0);
    if first == b'w' || (first == b'r' && second == b'+') {
        let pphar = reg::find_by_fname(&host);
        if reg::readonly() && !pphar.is_some_and(|p| p.borrow().is_data) {
            return Err(say(
                Code::Readonly,
                "phar error: write operations disabled by the php.ini setting phar.readonly".into(),
            ));
        }
        match reg::open_or_create_filename(&host, None, false, true) {
            Ok(ar) => {
                if ar.borrow().is_persistent && reg::copy_on_write(&ar).is_none() {
                    return Err(None);
                }
            }
            Err(e) => return Err(e.and_then(|e| say(Code::OpenFailed, e))),
        }
    } else if let Err(e) = reg::open_from_filename(&host, None, true) {
        return Err(e.and_then(|e| say(Code::NotFound, e)));
    }
    Ok(PharUrl { host, path })
}

/// `phar_dostat()`.
pub fn dostat(phar: &Archive, entry: Option<&Entry>, is_temp_dir: bool) -> StatBuf {
    let mut sb = StatBuf::default();
    match entry {
        Some(e) if !is_temp_dir => {
            let ts = i64::from(e.timestamp);
            sb.mode = i64::from(e.flags & ENT_PERM_MASK);
            if e.is_dir {
                sb.mode |= m::S_IFDIR;
            } else {
                sb.size = i64::from(e.uncompressed_filesize);
                sb.mode |= m::S_IFREG;
            }
            sb.mtime = ts;
            sb.atime = ts;
            sb.ctime = ts;
            sb.ino = i64::from(e.inode);
        }
        _ => {
            let ts = i64::from(phar.max_timestamp);
            sb.mode = 0o777 | m::S_IFDIR;
            sb.mtime = ts;
            sb.atime = ts;
            sb.ctime = ts;
        }
    }
    if !phar.is_writeable {
        sb.mode = (sb.mode & 0o555) | (sb.mode & !0o777);
    }
    sb.nlink = 1;
    sb.rdev = -1;
    sb.dev = 0xc;
    sb.blksize = -1;
    sb.blocks = -1;
    sb
}

/// The phar file stream (`phar_ops`).
#[derive(Debug)]
pub struct PharStream {
    data: Option<EntryData>,
    tmp_cache: Option<Vec<u8>>,
}

impl PharStream {
    /// The archive this stream belongs to.
    pub fn archive(&self) -> Option<ArchiveRef> {
        self.data.as_ref().map(|d| Rc::clone(&d.archive))
    }

    /// The entry this stream reads (`internal_file`).
    pub fn entry(&self) -> Option<Entry> {
        self.data.as_ref().and_then(|d| d.with_entry(Clone::clone))
    }

    /// The source entry after following links, and its bytes from `zero`.
    fn content(&mut self) -> (Vec<u8>, u32, bool) {
        let Some(d) = &self.data else {
            return (Vec::new(), 0, true);
        };
        let ar = Rc::clone(&d.archive);
        let entry = d.with_entry(Clone::clone).unwrap_or_default();
        let src = match (&d.entry, &entry.symlink) {
            (EntryRef::Manifest(k), Some(_)) => {
                let a = ar.borrow();
                a.link_source(k)
                    .and_then(|l| a.manifest.get(&l).cloned())
                    .unwrap_or(entry)
            }
            _ => entry,
        };
        if src.is_deleted {
            return (Vec::new(), 0, true);
        }
        let zero = usize::try_from(d.zero).unwrap_or(0);
        let bytes = match src.fp_type {
            FpType::Mod | FpType::Ufp => src
                .data
                .as_ref()
                .map(|b| {
                    b.borrow()
                        .get(zero..)
                        .map(<[u8]>::to_vec)
                        .unwrap_or_default()
                })
                .unwrap_or_default(),
            FpType::Tmp => {
                if self.tmp_cache.is_none() {
                    self.tmp_cache = src.tmp.as_deref().and_then(read_external);
                }
                self.tmp_cache.clone().unwrap_or_default()
            }
            FpType::Fp => ar
                .borrow_mut()
                .open_archive_fp()
                .map(|img| img.get(zero..).map(<[u8]>::to_vec).unwrap_or_default())
                .unwrap_or_default(),
        };
        (bytes, src.uncompressed_filesize, false)
    }
}

impl StreamOps for PharStream {
    fn label(&self) -> &'static str {
        "phar stream"
    }

    fn read(&mut self, st: &mut StreamState, buf: &mut [u8]) -> Result<usize, OpFailed> {
        let (bytes, size, deleted) = self.content();
        if deleted {
            st.eof = true;
            return Err(OpFailed);
        }
        let Some(d) = self.data.as_mut() else {
            return Err(OpFailed);
        };
        let pos = usize::try_from(d.position).unwrap_or(usize::MAX);
        let avail = (size as usize).saturating_sub(pos);
        let n = buf.len().min(avail).min(bytes.len().saturating_sub(pos));
        if n > 0 {
            buf[..n].copy_from_slice(&bytes[pos..pos + n]);
        }
        d.position += n as u64;
        st.eof = d.position == u64::from(size);
        Ok(n)
    }

    fn write(&mut self, st: &mut StreamState, buf: &[u8]) -> Result<usize, OpFailed> {
        let Some(d) = self.data.as_mut() else {
            return Err(OpFailed);
        };
        let needs_buffer = d.with_entry(|e| e.data.is_none()).unwrap_or(false);
        if needs_buffer {
            // PHP writes straight into the archive fp of an entry opened
            // for reading; give the entry its own copy of the content.
            let (bytes, size, _) = self.content();
            let Some(d) = self.data.as_mut() else {
                return Err(OpFailed);
            };
            let mut c = bytes;
            c.truncate(size as usize);
            d.with_entry_mut(|e| {
                e.data = Some(Rc::new(std::cell::RefCell::new(c)));
                e.fp_type = FpType::Mod;
                e.offset = 0;
            });
            d.zero = 0;
            self.tmp_cache = None;
        }
        let Some(d) = self.data.as_mut() else {
            return Err(OpFailed);
        };
        let pos = d.position + d.zero;
        let mut fail = None;
        let fname = d.archive.borrow().fname.clone();
        d.with_entry_mut(|e| {
            let Some(content) = e.data.clone() else {
                fail = Some(e.filename.clone());
                return;
            };
            let mut c = content.borrow_mut();
            let p = usize::try_from(pos).unwrap_or(usize::MAX);
            if c.len() < p {
                c.resize(p, 0);
            }
            let end = p + buf.len();
            if c.len() < end {
                c.resize(end, 0);
            }
            c[p..end].copy_from_slice(buf);
        });
        if let Some(name) = fail {
            st.warn(
                Code::WriteFailed,
                format!(
                    "phar error: Could not write {} characters to \"{}\" in phar \"{}\"",
                    buf.len(),
                    s(&name),
                    s(&fname)
                ),
            );
            return Err(OpFailed);
        }
        d.position += buf.len() as u64;
        let newpos = d.position;
        d.with_entry_mut(|e| {
            if newpos > u64::from(e.uncompressed_filesize) {
                e.uncompressed_filesize = crate::util::to_u32(usize::try_from(newpos).unwrap_or(0));
            }
            e.compressed_filesize = e.uncompressed_filesize;
            e.old_flags = e.flags;
            e.is_modified = true;
        });
        Ok(buf.len())
    }

    fn flush(&mut self, st: &mut StreamState) -> i32 {
        let Some(d) = self.data.as_mut() else {
            return 0;
        };
        let modified = d.with_entry(|e| e.is_modified).unwrap_or(false);
        if !modified {
            return 0;
        }
        let now = crate::sys::now32();
        d.with_entry_mut(|e| e.timestamp = now);
        match reg::flush(&d.archive) {
            Ok(()) => 0,
            Err(e) => {
                st.warn(Code::FlushFailed, e);
                -1
            }
        }
    }

    fn close(&mut self, st: &mut StreamState, _preserve: bool) -> i32 {
        let r = self.flush(st);
        if let Some(d) = self.data.take() {
            reg::entry_delref(d);
        }
        r
    }

    fn can_seek(&self) -> bool {
        true
    }

    fn seek(&mut self, _st: &mut StreamState, offset: i64, whence: i32, newpos: &mut i64) -> bool {
        let (_, size, _) = self.content();
        let Some(d) = self.data.as_mut() else {
            return false;
        };
        let zero = i64::try_from(d.zero).unwrap_or(0);
        let pos = i64::try_from(d.position).unwrap_or(0);
        let temp = match whence {
            SEEK_END => zero.wrapping_add(i64::from(size)).wrapping_add(offset),
            SEEK_CUR => zero.wrapping_add(pos).wrapping_add(offset),
            SEEK_SET => zero.wrapping_add(offset),
            _ => 0,
        };
        if temp > zero + i64::from(size) || temp < zero {
            return false;
        }
        d.position = u64::try_from(temp - zero).unwrap_or(0);
        *newpos = temp - zero;
        true
    }

    fn stat(&mut self, _st: &mut StreamState) -> Option<StatBuf> {
        let d = self.data.as_ref()?;
        let a = d.archive.borrow();
        let e = d.entry.with(&a, Clone::clone)?;
        Some(dostat(&a, Some(&e), false))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Reads a mounted file: plain paths directly, URLs (`phar://...`)
/// through the streams layer.
fn read_external(path: &[u8]) -> Option<Vec<u8>> {
    if crate::util::find(path, b"://").is_some() {
        php_streams::file::file_get_contents(path, false, None, 0, None)
            .ok()
            .flatten()
    } else {
        crate::sys::read_file(path).ok()
    }
}

/// A phar directory stream (`phar_dir_ops`).
#[derive(Debug, Default)]
pub struct PharDir {
    names: Vec<Vec<u8>>,
    pos: usize,
}

impl PharDir {
    /// The listed names.
    pub fn names(&self) -> &[Vec<u8>] {
        &self.names
    }
}

impl StreamOps for PharDir {
    fn label(&self) -> &'static str {
        "phar dir"
    }

    fn read(&mut self, _st: &mut StreamState, _buf: &mut [u8]) -> Result<usize, OpFailed> {
        Err(OpFailed)
    }

    // `phar_dir_write()` exists (and always fails), so no "not writable"
    // notice is raised.
    fn write(&mut self, _st: &mut StreamState, _buf: &[u8]) -> Result<usize, OpFailed> {
        Err(OpFailed)
    }

    fn readdir(&mut self, _st: &mut StreamState) -> Option<Vec<u8>> {
        let n = self.names.get(self.pos)?.clone();
        self.pos += 1;
        if n.len() >= 256 {
            return None;
        }
        // `d_name` is a C string
        Some(crate::util::cstr(&n).to_vec())
    }

    fn can_seek(&self) -> bool {
        true
    }

    fn seek(&mut self, _st: &mut StreamState, offset: i64, whence: i32, newpos: &mut i64) -> bool {
        let (whence, offset) = if whence == SEEK_END {
            (SEEK_SET, crate::util::to_i64(self.names.len()) + offset)
        } else {
            (whence, offset)
        };
        if whence == SEEK_SET {
            self.pos = 0;
        }
        if offset < 0 {
            return false;
        }
        *newpos = 0;
        while *newpos < offset && self.pos < self.names.len() {
            self.pos += 1;
            *newpos += 1;
        }
        true
    }

    fn flush(&mut self, _st: &mut StreamState) -> i32 {
        -1
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// `phar_make_dirstream()`: the sorted unique names directly inside `dir`
/// (`"/"` for the root).
pub fn make_dirstream(dir: &[u8], phar: &Archive) -> Vec<Vec<u8>> {
    let mut data = crate::omap::OrderedMap::<()>::new();
    if (dir == b"/" && phar.manifest.is_empty()) || is_magic_phar(dir) {
        return Vec::new();
    }
    let dirlen = dir.len();
    for (key, _) in phar.manifest.iter() {
        let keylen = key.len();
        if keylen <= dirlen && (keylen == 0 || keylen < dirlen || key.starts_with(dir)) {
            continue;
        }
        let entry: &[u8] = if dir.first() == Some(&b'/') {
            if is_magic_phar(key) {
                continue;
            }
            match key.iter().position(|&c| c == b'/') {
                Some(p) => &key[..p],
                None => key,
            }
        } else {
            if !key.starts_with(dir) || key.get(dirlen) != Some(&b'/') {
                continue;
            }
            let rest = &key[dirlen + 1..];
            match rest.iter().position(|&c| c == b'/') {
                Some(p) => &rest[..p],
                None => rest,
            }
        };
        if !entry.is_empty() {
            data.update(entry.to_vec(), ());
        }
    }
    data.sort_keys();
    data.keys()
}

fn dir_stream(names: Vec<Vec<u8>>) -> Stream {
    Stream::new(Box::new(PharDir { names, pos: 0 }), "r")
}

/// Serialises a context option value for the `metadata` context option.
fn serialize_context_value(v: &php_streams::value::Value) -> Option<(php_runtime::Value, Vec<u8>)> {
    fn conv(v: &php_streams::value::Value) -> Option<php_runtime::Value> {
        use php_streams::value::{Key, Value as V};
        Some(match v {
            V::Null => php_runtime::Value::Null,
            V::Bool(b) => php_runtime::Value::Bool(*b),
            V::Int(i) => php_runtime::Value::Int(*i),
            V::Float(f) => php_runtime::Value::Float(*f),
            V::Str(b) => php_runtime::Value::string(b.as_slice()),
            V::Array(items) => {
                let mut arr = php_runtime::ZArray::new();
                for (k, val) in items {
                    let val = conv(val)?;
                    let t = arr.make_mut();
                    match k {
                        Key::Int(i) => {
                            t.insert(*i, val);
                        }
                        Key::Str(sk) => {
                            t.insert(php_runtime::ZStr::from(sk.as_slice()), val);
                        }
                    }
                }
                php_runtime::Value::Array(arr)
            }
            _ => return None,
        })
    }
    let value = conv(v)?;
    let mut ctx = php_runtime::CollectingContext::default();
    let bytes = php_runtime::serialize::serialize(&value, &mut ctx).ok()?;
    Some((value, bytes))
}

/// The `phar://` wrapper (`php_stream_phar_wrapper`).
#[derive(Debug, Default, Clone, Copy)]
pub struct PharWrapper;

impl PharWrapper {
    #[allow(clippy::too_many_lines)]
    fn open_url(
        path: &[u8],
        mode: &str,
        options: u32,
        opened_path: &mut Option<Vec<u8>>,
        context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        let url = parse_url(path, mode, options).map_err(|e| match e {
            Some((_, m)) => OpenError::msg(m),
            None => OpenError::silent(),
        })?;
        let internal = url.path[1..].to_vec();
        let first = mode.as_bytes().first().copied().unwrap_or(b'r');
        let second = mode.as_bytes().get(1).copied().unwrap_or(0);
        if first == b'w' || (first == b'r' && second == b'+') {
            let data = reg::get_or_create_entry_data(
                &url.host,
                &internal,
                mode,
                0,
                true,
                crate::sys::entry_now32(),
            )
            .map_err(|e| {
                OpenError::msg(e.unwrap_or_else(|| {
                    format!(
                        "phar error: file \"{}\" could not be created in phar \"{}\"",
                        s(&internal),
                        s(&url.host)
                    )
                }))
            })?;
            let mut data = data;
            if let Some(ctx) = context {
                let c = ctx.borrow();
                let empty = data
                    .with_entry(|e| e.uncompressed_filesize == 0 && e.compressed_filesize == 0)
                    .unwrap_or(false);
                if let Some(php_streams::value::Value::Int(comp)) = c.get("phar", "compress") {
                    let comp = *comp;
                    if empty && comp & !i64::from(ENT_COMPRESSION_MASK) == 0 {
                        data.with_entry_mut(|e| {
                            e.flags &= !ENT_COMPRESSION_MASK;
                            e.flags |= crate::util::lo32(comp);
                        });
                    }
                }
                if let Some(meta) = c.get("phar", "metadata") {
                    if let Some((value, bytes)) = serialize_context_value(meta) {
                        data.with_entry_mut(|e| {
                            e.metadata = crate::archive::Metadata {
                                serialized: Some(bytes),
                                value: Some(value),
                            };
                        });
                        data.archive.borrow_mut().is_modified = true;
                    }
                }
            }
            let mut p = b"phar://".to_vec();
            p.extend_from_slice(&data.archive.borrow().fname);
            p.push(b'/');
            p.extend_from_slice(&data.with_entry(|e| e.filename.clone()).unwrap_or_default());
            *opened_path = Some(p);
            return Ok(Stream::new(
                Box::new(PharStream {
                    data: Some(data),
                    tmp_cache: None,
                }),
                mode,
            ));
        }
        if internal.is_empty() && options & STREAM_OPEN_FOR_INCLUDE != 0 {
            let phar = reg::get_archive(&url.host, None)
                .ok()
                .flatten()
                .ok_or_else(|| {
                    OpenError::msg(format!("file {} is not a valid phar archive", s(&url.host)))
                })?;
            let (is_tz, fname) = {
                let a = phar.borrow();
                (a.is_tar || a.is_zip, a.fname.clone())
            };
            let data = if is_tz {
                match reg::get_entry_data(&url.host, b".phar/stub.php", "r", 0, false) {
                    Ok(Some(d)) => d,
                    Ok(None) | Err(None) => {
                        return Err(OpenError::msg(format!(
                            "phar error: \"{}\" is not a file in phar \"{}\"",
                            s(&internal),
                            s(&url.host)
                        )))
                    }
                    Err(Some(e)) => return Err(OpenError::msg(e)),
                }
            } else {
                if phar.borrow_mut().open_archive_fp().is_none() {
                    return Err(OpenError::msg(format!(
                        "phar error: could not reopen phar \"{}\"",
                        s(&url.host)
                    )));
                }
                let halt = phar.borrow().halt_offset;
                let entry = Entry {
                    is_temp_dir: true,
                    fp_type: FpType::Fp,
                    uncompressed_filesize: crate::util::to_u32(usize::try_from(halt).unwrap_or(0)),
                    compressed_filesize: crate::util::to_u32(usize::try_from(halt).unwrap_or(0)),
                    is_crc_checked: true,
                    fp_refcount: 1,
                    ..Entry::default()
                };
                reg::addref(&phar);
                EntryData {
                    archive: Rc::clone(&phar),
                    entry: EntryRef::TempDir(Box::new(entry)),
                    position: 0,
                    zero: 0,
                }
            };
            *opened_path = Some(fname);
            return Ok(Stream::new(
                Box::new(PharStream {
                    data: Some(data),
                    tmp_cache: None,
                }),
                mode,
            ));
        }
        let data = match reg::get_entry_data(&url.host, &internal, "r", 0, false) {
            Ok(Some(d)) => d,
            Ok(None) | Err(None) => {
                return Err(OpenError::msg(format!(
                    "phar error: \"{}\" is not a file in phar \"{}\"",
                    s(&internal),
                    s(&url.host)
                )))
            }
            Err(Some(e)) => return Err(OpenError::msg(e)),
        };
        let crc_checked = data.with_entry(|e| e.is_crc_checked).unwrap_or(true);
        if !crc_checked {
            if let Some(k) = data.entry.key() {
                let r = data.archive.borrow_mut().check_crc(k);
                if let Err(e) = r {
                    reg::entry_delref(data);
                    return Err(OpenError::msg(e));
                }
            }
        }
        if !reg::cwd_init() && options & STREAM_OPEN_FOR_INCLUDE != 0 {
            let (is_tz, name) = {
                let a = data.archive.borrow();
                (
                    a.is_tar || a.is_zip,
                    data.with_entry(|e| e.filename.clone()).unwrap_or_default(),
                )
            };
            if is_tz && name == b".phar/stub.php" {
                reg::set_cwd(None, false);
            } else if let Some(p) = name.iter().rposition(|&c| c == b'/') {
                reg::set_cwd(Some(name[..p].to_vec()), true);
            } else {
                reg::set_cwd(None, true);
            }
        }
        let mut p = b"phar://".to_vec();
        p.extend_from_slice(&data.archive.borrow().fname);
        p.push(b'/');
        p.extend_from_slice(&data.with_entry(|e| e.filename.clone()).unwrap_or_default());
        *opened_path = Some(p);
        Ok(Stream::new(
            Box::new(PharStream {
                data: Some(data),
                tmp_cache: None,
            }),
            mode,
        ))
    }

    fn stat_url(url: &[u8], flags: u32) -> Option<StatBuf> {
        let r = parse_url(url, "r", flags | URL_STAT_QUIET).ok()?;
        let internal = &r.path[1..];
        let phar = reg::get_archive(&r.host, None).ok().flatten()?;
        if internal.is_empty() {
            return Some(dostat(&phar.borrow(), None, true));
        }
        {
            let a = phar.borrow();
            if let Some(e) = a.manifest.get(internal) {
                return Some(dostat(&a, Some(e), false));
            }
            if a.virtual_dirs.contains_key(internal) {
                return Some(dostat(&a, None, true));
            }
        }
        let mounts = phar.borrow().mounted_dirs.keys();
        for key in mounts {
            if key.len() >= internal.len() || !internal.starts_with(&key) {
                continue;
            }
            let tmp = {
                let a = phar.borrow();
                let e = a.manifest.get(&key)?;
                if !e.is_mounted {
                    return None;
                }
                e.tmp.clone()?
            };
            let mut test = tmp;
            test.extend_from_slice(&internal[key.len()..]);
            if php_streams::open::stat_path(&test, 0, None).is_none() {
                continue;
            }
            if !reg::mount_entry(&phar, &test, internal) {
                return None;
            }
            let a = phar.borrow();
            let e = a.manifest.get(internal)?;
            return Some(dostat(&a, Some(e), false));
        }
        None
    }
}

fn warn(options: u32, code: Code, msg: impl Into<Vec<u8>>) {
    wrapper_warn(LABEL, options, code, msg);
}

impl Wrapper for PharWrapper {
    fn label(&self) -> &'static str {
        LABEL
    }

    fn open(
        &self,
        path: &[u8],
        mode: &str,
        options: u32,
        opened_path: &mut Option<Vec<u8>>,
        context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        Self::open_url(path, mode, options, opened_path, context)
    }

    fn url_stat(
        &self,
        path: &[u8],
        flags: u32,
        _context: Option<&ContextRef>,
    ) -> Option<Result<StatBuf, ()>> {
        Some(Self::stat_url(path, flags).ok_or(()))
    }

    fn has_dir_opener(&self) -> bool {
        true
    }

    fn opendir(
        &self,
        path: &[u8],
        options: u32,
        _context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        let r = match parse_url(path, "r", options) {
            Ok(r) => r,
            Err(e) => {
                let mut err = OpenError::default();
                if let Some((_, m)) = e {
                    err.messages.push(m.into_bytes());
                }
                err.messages
                    .push(format!("phar url \"{}\" is unknown", s(path)).into_bytes());
                return Err(err);
            }
        };
        let phar = match reg::get_archive(&r.host, None) {
            Ok(Some(p)) => p,
            Ok(None) => {
                return Err(OpenError::msg(format!(
                    "phar file \"{}\" is unknown",
                    s(&r.host)
                )))
            }
            Err(e) => return Err(OpenError::msg(e)),
        };
        if r.path == b"/" {
            return Ok(dir_stream(make_dirstream(b"/", &phar.borrow())));
        }
        let internal = &r.path[1..];
        let a = phar.borrow();
        match a.manifest.get(internal) {
            Some(e) if !e.is_dir => Err(OpenError::silent()),
            Some(e) => {
                if e.is_mounted {
                    let tmp = e.tmp.clone().unwrap_or_default();
                    drop(a);
                    return php_streams::open::opendir(&tmp, options, None)
                        .ok_or_else(OpenError::silent);
                }
                Ok(dir_stream(make_dirstream(internal, &a)))
            }
            None => {
                if a.manifest.iter().any(|(k, _)| k.starts_with(internal)) {
                    Ok(dir_stream(make_dirstream(internal, &a)))
                } else {
                    Err(OpenError::silent())
                }
            }
        }
    }

    fn has_unlink(&self) -> bool {
        true
    }

    fn has_rename(&self) -> bool {
        true
    }

    fn unlink(&self, url: &[u8], options: u32, _context: Option<&ContextRef>) -> Option<bool> {
        let r = match parse_url(url, "rb", options) {
            Ok(r) => r,
            Err(e) => {
                if let Some((c, m)) = e {
                    warn(options, c, m);
                }
                warn(options, Code::UnlinkFailed, "phar error: unlink failed");
                return Some(false);
            }
        };
        let pphar = reg::find_by_fname(&r.host);
        if reg::readonly() && !pphar.is_some_and(|p| p.borrow().is_data) {
            warn(
                options,
                Code::Readonly,
                "phar error: write operations disabled by the php.ini setting phar.readonly",
            );
            return Some(false);
        }
        let internal = r.path[1..].to_vec();
        let data = match reg::get_entry_data(&r.host, &internal, "r", 0, true) {
            Ok(Some(d)) => d,
            Ok(None) | Err(None) => {
                warn(
                    options,
                    Code::NotFound,
                    format!("unlink of \"{}\" failed, file does not exist", s(url)),
                );
                return Some(false);
            }
            Err(Some(e)) => {
                warn(
                    options,
                    Code::UnlinkFailed,
                    format!("unlink of \"{}\" failed: {e}", s(url)),
                );
                return Some(false);
            }
        };
        if data.with_entry(|e| e.fp_refcount).unwrap_or(0) > 1 {
            warn(
                options,
                Code::UnlinkFailed,
                format!(
                    "phar error: \"{}\" in phar \"{}\", has open file pointers, cannot unlink",
                    s(&internal),
                    s(&r.host)
                ),
            );
            reg::entry_delref(data);
            return Some(false);
        }
        if let Err(e) = reg::entry_remove(data) {
            warn(options, Code::UnlinkFailed, e);
        }
        php_streams::state::clear_stat_cache();
        Some(true)
    }

    #[allow(clippy::too_many_lines)]
    fn rename(
        &self,
        from: &[u8],
        to: &[u8],
        options: u32,
        _context: Option<&ContextRef>,
    ) -> Option<bool> {
        let (f, t) = (s(from), s(to));
        let Ok(rf) = parse_url(from, "wb", options | URL_STAT_QUIET) else {
            warn(
                options,
                Code::InvalidUrl,
                format!("phar error: cannot rename \"{f}\" to \"{t}\": invalid or non-writable url \"{f}\""),
            );
            return Some(false);
        };
        let pfrom = reg::get_archive(&rf.host, None).ok().flatten();
        if reg::readonly() && !pfrom.is_some_and(|p| p.borrow().is_data) {
            warn(
                options,
                Code::Readonly,
                "phar error: Write operations disabled by the php.ini setting phar.readonly",
            );
            return Some(false);
        }
        let Ok(rt) = parse_url(to, "wb", options | URL_STAT_QUIET) else {
            warn(
                options,
                Code::InvalidUrl,
                format!("phar error: cannot rename \"{f}\" to \"{t}\": invalid or non-writable url \"{t}\""),
            );
            return Some(false);
        };
        let pto = reg::get_archive(&rt.host, None).ok().flatten();
        if reg::readonly() && !pto.is_some_and(|p| p.borrow().is_data) {
            warn(
                options,
                Code::Readonly,
                "phar error: Write operations disabled by the php.ini setting phar.readonly",
            );
            return Some(false);
        }
        if rf.host != rt.host {
            warn(
                options,
                Code::RenameFailed,
                format!("phar error: cannot rename \"{f}\" to \"{t}\", not within the same phar archive"),
            );
            return Some(false);
        }
        let phar = match reg::get_archive(&rf.host, None) {
            Ok(Some(p)) => p,
            Ok(None) => {
                warn(
                    options,
                    Code::RenameFailed,
                    format!("phar error: cannot rename \"{f}\" to \"{t}\": "),
                );
                return Some(false);
            }
            Err(e) => {
                warn(
                    options,
                    Code::RenameFailed,
                    format!("phar error: cannot rename \"{f}\" to \"{t}\": {e}"),
                );
                return Some(false);
            }
        };
        let phar = if phar.borrow().is_persistent {
            if let Some(p) = reg::copy_on_write(&phar) {
                p
            } else {
                warn(
                    options,
                    Code::RenameFailed,
                    format!("phar error: cannot rename \"{f}\" to \"{t}\": could not make cached phar writeable"),
                );
                return Some(false);
            }
        } else {
            phar
        };
        let from_in = rf.path[1..].to_vec();
        let to_in = rt.path[1..].to_vec();
        let mut is_modified = false;
        let existing = phar.borrow().manifest.get(&from_in).cloned();
        let is_dir;
        if let Some(entry) = existing {
            if entry.is_deleted {
                warn(
                    options,
                    Code::NotFound,
                    format!("phar error: cannot rename \"{f}\" to \"{t}\" from extracted phar archive, source has been deleted"),
                );
                return Some(false);
            }
            if rf.path == rt.path {
                return Some(true);
            }
            let content = phar.borrow_mut().read_entry(&from_in);
            let mut new = entry.clone();
            new.filename.clone_from(&to_in);
            {
                let mut a = phar.borrow_mut();
                if let Some(old) = a.manifest.get_mut(&from_in) {
                    old.is_deleted = true;
                    old.data = None;
                    old.metadata = crate::archive::Metadata::default();
                    old.symlink = None;
                    old.tmp = None;
                }
            }
            match content {
                Ok(c) => {
                    new.symlink = None;
                    if new.is_tar {
                        new.tar_type = crate::consts::TAR_FILE;
                    }
                    new.data = Some(Rc::new(std::cell::RefCell::new(c)));
                    new.fp_type = FpType::Mod;
                    new.offset = 0;
                }
                Err(e) => {
                    warn(
                        options,
                        Code::RenameFailed,
                        format!("phar error: cannot rename \"{f}\" to \"{t}\": {e}"),
                    );
                    return Some(false);
                }
            }
            new.is_modified = true;
            is_dir = new.is_dir;
            phar.borrow_mut().manifest.update(to_in.clone(), new);
            is_modified = true;
        } else {
            is_dir = phar.borrow().virtual_dirs.contains_key(&from_in);
            if !is_dir {
                warn(
                    options,
                    Code::NotFound,
                    format!("phar error: cannot rename \"{f}\" to \"{t}\" from extracted phar archive, source does not exist"),
                );
                return Some(false);
            }
        }
        if is_dir {
            let mut a = phar.borrow_mut();
            let flen = from_in.len();
            let renames: Vec<Vec<u8>> = a
                .manifest
                .iter()
                .filter(|(k, e)| {
                    !e.is_deleted && k.len() > flen && k.starts_with(&from_in) && k[flen] == b'/'
                })
                .map(|(k, _)| k.clone())
                .collect();
            for k in renames {
                let mut nk = to_in.clone();
                nk.extend_from_slice(&k[flen..]);
                if a.manifest.rename(&k, nk.clone()) {
                    if let Some(e) = a.manifest.get_mut(&nk) {
                        e.filename.clone_from(&nk);
                        e.is_modified = true;
                    }
                    is_modified = true;
                }
            }
            let rename_in = |map: &mut crate::omap::OrderedMap<()>| {
                let keys: Vec<Vec<u8>> = map
                    .keys()
                    .into_iter()
                    .filter(|k| k.starts_with(&from_in) && (k.len() == flen || k[flen] == b'/'))
                    .collect();
                for k in keys {
                    let mut nk = to_in.clone();
                    nk.extend_from_slice(&k[flen..]);
                    map.rename(&k, nk);
                }
            };
            rename_in(&mut a.virtual_dirs);
            rename_in(&mut a.mounted_dirs);
        }
        if is_modified {
            if let Err(e) = reg::flush(&phar) {
                warn(
                    options,
                    Code::RenameFailed,
                    format!("phar error: cannot rename \"{f}\" to \"{t}\": {e}"),
                );
                return Some(false);
            }
        }
        php_streams::state::clear_stat_cache();
        Some(true)
    }

    #[allow(clippy::too_many_lines)]
    fn mkdir(
        &self,
        url: &[u8],
        _mode: i64,
        options: u32,
        _context: Option<&ContextRef>,
    ) -> Option<bool> {
        let u = s(url);
        let Ok((arch, _)) = reg::split_fname(url, 2, 2) else {
            warn(
                options,
                Code::NotFound,
                format!("phar error: cannot create directory \"{u}\", no phar archive specified"),
            );
            return Some(false);
        };
        let phar = reg::get_archive(&arch, None).ok().flatten();
        if reg::readonly() && !phar.is_some_and(|p| p.borrow().is_data) {
            warn(
                options,
                Code::Readonly,
                format!("phar error: cannot create directory \"{u}\", write operations disabled"),
            );
            return Some(false);
        }
        let r = match parse_url(url, "w", options) {
            Ok(r) => r,
            Err(e) => {
                if let Some((c, m)) = e {
                    warn(options, c, m);
                }
                return Some(false);
            }
        };
        let inner = r.path[1..].to_vec();
        let host = s(&r.host);
        let phar = match reg::get_archive(&r.host, None) {
            Ok(Some(p)) => p,
            Ok(None) => {
                warn(options, Code::MkdirFailed, format!("phar error: cannot create directory \"{}\" in phar \"{host}\", error retrieving phar information: ", s(&inner)));
                return Some(false);
            }
            Err(e) => {
                warn(options, Code::MkdirFailed, format!("phar error: cannot create directory \"{}\" in phar \"{host}\", error retrieving phar information: {e}", s(&inner)));
                return Some(false);
            }
        };
        match reg::get_entry_info_dir(&phar, &inner, 2, true) {
            Ok(_) => {
                warn(options, Code::AlreadyExists, format!("phar error: cannot create directory \"{}\" in phar \"{host}\", directory already exists", s(&inner)));
                return Some(false);
            }
            Err(Some(e)) => {
                warn(
                    options,
                    Code::MkdirFailed,
                    format!(
                        "phar error: cannot create directory \"{}\" in phar \"{host}\", {e}",
                        s(&inner)
                    ),
                );
                return Some(false);
            }
            Err(None) => {}
        }
        match reg::get_entry_info_dir(&phar, &inner, 0, true) {
            Ok(_) => {
                warn(options, Code::AlreadyExists, format!("phar error: cannot create directory \"{}\" in phar \"{host}\", file already exists", s(&inner)));
                return Some(false);
            }
            Err(Some(e)) => {
                warn(
                    options,
                    Code::MkdirFailed,
                    format!(
                        "phar error: cannot create directory \"{}\" in phar \"{host}\", {e}",
                        s(&inner)
                    ),
                );
                return Some(false);
            }
            Err(None) => {}
        }
        let (is_zip, is_tar, fname) = {
            let a = phar.borrow();
            (a.is_zip, a.is_tar, a.fname.clone())
        };
        let entry = Entry {
            filename: inner.clone(),
            is_zip,
            is_tar,
            tar_type: if is_tar { TAR_DIR } else { 0 },
            is_dir: true,
            is_modified: true,
            is_crc_checked: true,
            flags: ENT_PERM_DEF_DIR,
            old_flags: ENT_PERM_DEF_DIR,
            ..Entry::default()
        };
        if !phar.borrow_mut().manifest.add(inner.clone(), entry) {
            warn(options, Code::MkdirFailed, format!("phar error: cannot create directory \"{}\" in phar \"{}\", adding to manifest failed", s(&inner), s(&fname)));
            return Some(false);
        }
        if let Err(e) = reg::flush(&phar) {
            warn(
                options,
                Code::MkdirFailed,
                format!(
                    "phar error: cannot create directory \"{}\" in phar \"{}\", {e}",
                    s(&inner),
                    s(&fname)
                ),
            );
            phar.borrow_mut().manifest.remove(&inner);
            return Some(false);
        }
        phar.borrow_mut().add_virtual_dirs(&inner);
        php_streams::state::clear_stat_cache();
        Some(true)
    }

    #[allow(clippy::too_many_lines)]
    fn rmdir(&self, url: &[u8], options: u32, _context: Option<&ContextRef>) -> Option<bool> {
        let u = s(url);
        let Ok((arch, _)) = reg::split_fname(url, 2, 2) else {
            warn(
                options,
                Code::NotFound,
                format!("phar error: cannot remove directory \"{u}\", no phar archive specified, or phar archive does not exist"),
            );
            return Some(false);
        };
        let phar = reg::get_archive(&arch, None).ok().flatten();
        if reg::readonly() && !phar.is_some_and(|p| p.borrow().is_data) {
            warn(
                options,
                Code::Readonly,
                format!("phar error: cannot rmdir directory \"{u}\", write operations disabled"),
            );
            return Some(false);
        }
        let r = match parse_url(url, "w", options) {
            Ok(r) => r,
            Err(e) => {
                if let Some((c, m)) = e {
                    warn(options, c, m);
                }
                return Some(false);
            }
        };
        let inner = r.path[1..].to_vec();
        let host = s(&r.host);
        let phar = match reg::get_archive(&r.host, None) {
            Ok(Some(p)) => p,
            Ok(None) => {
                warn(options, Code::RmdirFailed, format!("phar error: cannot remove directory \"{}\" in phar \"{host}\", error retrieving phar information: ", s(&inner)));
                return Some(false);
            }
            Err(e) => {
                warn(options, Code::RmdirFailed, format!("phar error: cannot remove directory \"{}\" in phar \"{host}\", error retrieving phar information: {e}", s(&inner)));
                return Some(false);
            }
        };
        let entry = match reg::get_entry_info_dir(&phar, &inner, 2, true) {
            Ok(e) => e,
            Err(Some(e)) => {
                warn(
                    options,
                    Code::RmdirFailed,
                    format!(
                        "phar error: cannot remove directory \"{}\" in phar \"{host}\", {e}",
                        s(&inner)
                    ),
                );
                return Some(false);
            }
            Err(None) => {
                warn(options, Code::NotFound, format!("phar error: cannot remove directory \"{}\" in phar \"{host}\", directory does not exist", s(&inner)));
                return Some(false);
            }
        };
        let deleted = entry
            .with(&phar.borrow(), |e| e.is_deleted)
            .unwrap_or(false);
        if !deleted {
            let a = phar.borrow();
            let plen = inner.len();
            let has_child = |k: &[u8]| k.starts_with(&inner) && k.get(plen) == Some(&b'/');
            if a.manifest.iter().any(|(k, _)| has_child(k))
                || a.virtual_dirs.iter().any(|(k, ())| has_child(k))
            {
                drop(a);
                warn(
                    options,
                    Code::RmdirFailed,
                    "phar error: Directory not empty",
                );
                return Some(false);
            }
        }
        match entry {
            EntryRef::TempDir(_) => {
                phar.borrow_mut().virtual_dirs.remove(&inner);
            }
            EntryRef::Manifest(k) => {
                let fname = phar.borrow().fname.clone();
                if let Some(e) = phar.borrow_mut().manifest.get_mut(&k) {
                    e.is_deleted = true;
                    e.is_modified = true;
                }
                if let Err(e) = reg::flush(&phar) {
                    warn(
                        options,
                        Code::RmdirFailed,
                        format!(
                            "phar error: cannot remove directory \"{}\" in phar \"{}\", {e}",
                            s(&k),
                            s(&fname)
                        ),
                    );
                    return Some(false);
                }
                // even if a still-referenced deleted entry lingers, the
                // directory must not stat as an implicit one any more
                phar.borrow_mut().virtual_dirs.remove(&k);
            }
        }
        php_streams::state::clear_stat_cache();
        Some(true)
    }
}

/// The open stream's entry, if `stream` is a phar stream.
pub fn stream_entry(stream: &Stream) -> Option<Entry> {
    stream.ops_as::<PharStream>().and_then(PharStream::entry)
}
