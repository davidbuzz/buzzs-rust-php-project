//! The per-request phar registry (`PHAR_G`): the file-name and alias maps,
//! the `phar.cache_list` manifest cache, INI settings, archive opening and
//! creation (`phar_open_from_filename()`, `phar_open_or_create_filename()`,
//! `phar_create_or_parse_filename()`), lookup (`phar_get_archive()`,
//! `phar_detect_phar_fname_ext()`, `phar_split_fname()`), entry access
//! (`phar_get_entry_info_dir()`, `phar_get_entry_data()`,
//! `phar_get_or_create_entry_data()`) and flushing (`phar_flush_ex()`).

use std::cell::RefCell;
use std::rc::Rc;

use crate::archive::{Archive, ArchiveRef, Entry, FpType};
use crate::consts::{
    ENT_PERM_DEF_DIR, ENT_PERM_DEF_FILE, FILE_COMPRESSED_BZ2, FILE_COMPRESSED_GZ,
    FILE_COMPRESSION_MASK, TAR_DIR, TAR_FILE,
};
use crate::omap::OrderedMap;
use crate::phar_format::Detected;
use crate::util::{find, fix_filepath, is_magic_phar, path_check, s, validate_alias};

#[derive(Debug, Clone)]
struct Last {
    name: Vec<u8>,
    archive: ArchiveRef,
    alias: Option<Vec<u8>>,
}

/// The phar globals.
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)] // mirrors PHAR_G
struct State {
    fname_map: OrderedMap<ArchiveRef>,
    alias_map: OrderedMap<ArchiveRef>,
    cached_phars: OrderedMap<ArchiveRef>,
    cached_alias: OrderedMap<ArchiveRef>,
    manifest_cached: bool,
    last: Option<Last>,
    readonly: bool,
    readonly_orig: bool,
    require_hash: bool,
    require_hash_orig: bool,
    cwd: Option<Vec<u8>>,
    cwd_init: bool,
    mung_list: u32,
    intercepted: bool,
    openssl_privatekey: Vec<u8>,
    has_zlib: bool,
    has_bz2: bool,
    request_done: bool,
}

impl Default for State {
    fn default() -> Self {
        State {
            fname_map: OrderedMap::new(),
            alias_map: OrderedMap::new(),
            cached_phars: OrderedMap::new(),
            cached_alias: OrderedMap::new(),
            manifest_cached: false,
            last: None,
            readonly: true,
            readonly_orig: true,
            require_hash: true,
            require_hash_orig: true,
            cwd: None,
            cwd_init: false,
            mung_list: 0,
            intercepted: false,
            openssl_privatekey: Vec::new(),
            has_zlib: true,
            has_bz2: true,
            request_done: false,
        }
    }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

// ------------------------------------------------------------------ INI

/// `phar.readonly`.
pub fn readonly() -> bool {
    with(|s| s.readonly)
}

/// `phar.require_hash`.
pub fn require_hash() -> bool {
    with(|s| s.require_hash)
}

/// `phar_ini_modify_handler` for `phar.readonly` / `phar.require_hash`.
/// At startup the value also becomes the "original"; at runtime a value
/// that was originally on cannot be turned off (returns `false`, PHP's
/// `FAILURE`).
pub fn set_ini(name: &str, value: bool, startup: bool) -> bool {
    with(|s| {
        let is_readonly = name == "phar.readonly";
        let old = if is_readonly {
            s.readonly_orig
        } else {
            s.require_hash_orig
        };
        if startup {
            if is_readonly {
                s.readonly_orig = value;
            } else {
                s.require_hash_orig = value;
            }
        } else if old && !value {
            return false;
        }
        if is_readonly {
            s.readonly = value;
            for (_, a) in s.fname_map.iter() {
                let mut a = a.borrow_mut();
                if !a.is_data {
                    a.is_writeable = !value;
                }
            }
        } else {
            s.require_hash = value;
        }
        true
    })
}

/// Whether zlib/bz2 support is enabled (`PHAR_G(has_zlib/has_bz2)`).
pub fn set_compression_support(zlib: bool, bz2: bool) {
    with(|s| {
        s.has_zlib = zlib;
        s.has_bz2 = bz2;
    });
}

/// `PHAR_G(has_zlib)`.
pub fn has_zlib() -> bool {
    with(|s| s.has_zlib)
}

/// `PHAR_G(has_bz2)`.
pub fn has_bz2() -> bool {
    with(|s| s.has_bz2)
}

/// The private key last passed to `setSignatureAlgorithm()`.
pub fn set_openssl_privatekey(key: Vec<u8>) {
    with(|s| s.openssl_privatekey = key);
}

fn openssl_privatekey() -> Vec<u8> {
    with(|s| s.openssl_privatekey.clone())
}

/// `Phar::interceptFileFuncs()`.
pub fn intercept_functions() {
    with(|s| s.intercepted = true);
}

/// Whether file functions are intercepted.
pub fn intercepted() -> bool {
    with(|s| s.intercepted)
}

/// The `Phar::mungServer()` list.
pub fn mung_list() -> u32 {
    with(|s| s.mung_list)
}

/// Adds to the `Phar::mungServer()` list.
pub fn add_mung(bits: u32) {
    with(|s| s.mung_list |= bits);
}

/// The phar-relative "current directory" (`PHAR_G(cwd)`).
pub fn cwd() -> Option<Vec<u8>> {
    with(|s| s.cwd.clone())
}

/// Sets `PHAR_G(cwd)`; `cwd_init` records whether it was initialised.
pub fn set_cwd(cwd: Option<Vec<u8>>, init: bool) {
    with(|s| {
        s.cwd = cwd;
        s.cwd_init = init;
    });
}

/// `PHAR_G(cwd_init)`.
pub fn cwd_init() -> bool {
    with(|s| s.cwd_init)
}

/// Request shutdown: forgets every open archive (the manifest cache
/// survives) and resets per-request flags.
pub fn reset_request() {
    with(|s| {
        s.request_done = true;
        s.fname_map.clear();
        s.alias_map.clear();
        s.last = None;
        s.cwd = None;
        s.cwd_init = false;
        s.mung_list = 0;
        s.intercepted = false;
        s.openssl_privatekey.clear();
        s.readonly = s.readonly_orig;
        s.require_hash = s.require_hash_orig;
        s.request_done = false;
    });
}

/// The open archive registered under `fname` (`phar_fname_map`).
pub fn find_by_fname(fname: &[u8]) -> Option<ArchiveRef> {
    with(|s| s.fname_map.get(fname).cloned())
}

/// The archive registered for `alias` (`phar_alias_map`).
pub fn find_by_alias(alias: &[u8]) -> Option<ArchiveRef> {
    with(|s| s.alias_map.get(alias).cloned())
}

/// Number of registered archives.
pub fn archive_count() -> usize {
    with(|s| s.fname_map.len())
}

/// Whether any archive is known (open or cached) — the interceptors' fast
/// path check.
pub fn any_archive() -> bool {
    with(|s| !s.fname_map.is_empty() || s.manifest_cached)
}

fn invalidate_last() {
    with(|s| s.last = None);
}

/// Removes an archive from the file-name map (with the destructor's alias
/// cleanup and refcount drop).
fn fname_map_del(fname: &[u8]) -> bool {
    let removed = with(|s| {
        let a = s.fname_map.remove(fname)?;
        let keys: Vec<Vec<u8>> = s
            .alias_map
            .iter()
            .filter(|(_, v)| Rc::ptr_eq(v, &a))
            .map(|(k, _)| k.clone())
            .collect();
        for k in keys {
            s.alias_map.remove(&k);
        }
        if s.last.as_ref().is_some_and(|l| Rc::ptr_eq(&l.archive, &a)) {
            s.last = None;
        }
        Some(a)
    });
    match removed {
        Some(a) => {
            a.borrow_mut().refcount -= 1;
            true
        }
        None => false,
    }
}

/// `phar_free_alias()`: drops an unreferenced archive so its alias can be
/// reused. `false` when it is still referenced.
pub fn free_alias(ar: &ArchiveRef) -> bool {
    let (refcount, persistent, fname) = {
        let a = ar.borrow();
        (a.refcount, a.is_persistent, a.fname.clone())
    };
    if refcount != 0 || persistent {
        return false;
    }
    if !fname_map_del(&fname) {
        return false;
    }
    invalidate_last();
    true
}

/// Increments the reference count (an object or stream now uses it).
pub fn addref(ar: &ArchiveRef) {
    let mut a = ar.borrow_mut();
    if !a.is_persistent {
        a.refcount += 1;
    }
}

/// `phar_archive_delref()`. Returns `true` when the archive was dropped
/// from the registry.
pub fn delref(ar: &ArchiveRef) -> bool {
    let (persistent, refcount, fname) = {
        let mut a = ar.borrow_mut();
        if a.is_persistent {
            return false;
        }
        a.refcount -= 1;
        (a.is_persistent, a.refcount, a.fname.clone())
    };
    let _ = persistent;
    if refcount < 0 {
        let done = with(|s| s.request_done);
        if !done {
            fname_map_del(&fname);
        }
        return true;
    }
    if refcount == 0 {
        invalidate_last();
        let remove = {
            let mut a = ar.borrow_mut();
            let mut remove = a.manifest.is_empty();
            if a.fp.is_some() && (a.flags & FILE_COMPRESSION_MASK == 0 || a.alias.is_none()) {
                a.fp = None;
                remove |= a.flags & FILE_COMPRESSION_MASK != 0;
            }
            remove
        };
        if remove {
            fname_map_del(&fname);
            return true;
        }
    }
    false
}

// --------------------------------------------------------------- lookup

/// `phar_get_archive()`: finds an open archive by file name or alias,
/// binding `alias` to it when given. `Err` carries the "alias is already
/// used" message.
#[allow(clippy::too_many_lines)]
pub fn get_archive(fname: &[u8], alias: Option<&[u8]>) -> Result<Option<ArchiveRef>, String> {
    let alias = alias.filter(|a| !a.is_empty());
    let overloaded = |a: &[u8], owner: &[u8]| {
        format!(
            "alias \"{}\" is already used for archive \"{}\" cannot be overloaded with \"{}\"",
            s(a),
            s(owner),
            s(fname)
        )
    };
    // last-used cache
    let last = with(|s| s.last.clone());
    if let Some(l) = &last {
        if l.name == fname {
            let archive = Rc::clone(&l.archive);
            if let Some(a) = alias {
                let (temp, cur_alias) = {
                    let ar = archive.borrow();
                    (ar.is_temporary_alias, ar.alias.clone())
                };
                if !temp && cur_alias.as_deref() != Some(a) {
                    return Err(overloaded(a, &archive.borrow().fname));
                }
                with(|s| {
                    if let Some(ca) = cur_alias.as_ref().filter(|c| !c.is_empty()) {
                        s.alias_map.remove(ca);
                    }
                    s.alias_map.add(a.to_vec(), Rc::clone(&archive));
                    if let Some(l) = s.last.as_mut() {
                        l.alias = Some(a.to_vec());
                    }
                });
            }
            return Ok(Some(archive));
        }
    }
    let set_last = |a: &ArchiveRef, alias: Option<Vec<u8>>| {
        let name = a.borrow().fname.clone();
        with(|s| {
            s.last = Some(Last {
                name,
                archive: Rc::clone(a),
                alias,
            });
        });
    };
    if let Some(a) = alias {
        let found = match &last {
            Some(l) if l.alias.as_deref() == Some(a) => Some(Rc::clone(&l.archive)),
            _ => with(|s| {
                s.alias_map.get(a).cloned().or_else(|| {
                    if s.manifest_cached {
                        s.cached_alias.get(a).cloned()
                    } else {
                        None
                    }
                })
            }),
        };
        if let Some(archive) = found {
            let owner = archive.borrow().fname.clone();
            if owner != fname {
                let err = overloaded(a, &owner);
                if free_alias(&archive) {
                    return Ok(None);
                }
                return Err(err);
            }
            set_last(&archive, Some(a.to_vec()));
            return Ok(Some(archive));
        }
    }
    if fname.is_empty() {
        return Ok(None);
    }
    if let Some(archive) = find_by_fname(fname) {
        if let Some(a) = alias {
            let (temp, cur_alias) = {
                let ar = archive.borrow();
                (ar.is_temporary_alias, ar.alias.clone())
            };
            if !temp && cur_alias.as_deref() != Some(a) {
                return Err(overloaded(a, &archive.borrow().fname));
            }
            with(|s| {
                if let Some(ca) = cur_alias.as_ref().filter(|c| !c.is_empty()) {
                    s.alias_map.remove(ca);
                }
                s.alias_map.add(a.to_vec(), Rc::clone(&archive));
            });
        }
        let al = archive.borrow().alias.clone();
        set_last(&archive, al);
        return Ok(Some(archive));
    }
    let cached = with(|s| {
        if s.manifest_cached {
            s.cached_phars.get(fname).cloned()
        } else {
            None
        }
    });
    if let Some(archive) = cached {
        if let Some(a) = alias {
            let ar = archive.borrow();
            if !ar.is_temporary_alias && ar.alias.as_deref() != Some(a) {
                return Err(overloaded(a, &ar.fname));
            }
        }
        let al = archive.borrow().alias.clone();
        set_last(&archive, al);
        return Ok(Some(archive));
    }
    let by_alias = with(|s| {
        s.alias_map.get(fname).cloned().or_else(|| {
            if s.manifest_cached {
                s.cached_alias.get(fname).cloned()
            } else {
                None
            }
        })
    });
    if let Some(archive) = by_alias {
        let al = archive.borrow().alias.clone();
        set_last(&archive, al);
        return Ok(Some(archive));
    }
    let Some(real) = php_streams::paths::expand_filepath(fname) else {
        return Ok(None);
    };
    let found = with(|s| {
        s.fname_map.get(&real).cloned().or_else(|| {
            if s.manifest_cached {
                s.cached_phars.get(&real).cloned()
            } else {
                None
            }
        })
    });
    if let Some(archive) = found {
        if let Some(a) = alias {
            with(|s| {
                s.alias_map.add(a.to_vec(), Rc::clone(&archive));
            });
        }
        let al = archive.borrow().alias.clone();
        set_last(&archive, al);
        return Ok(Some(archive));
    }
    Ok(None)
}

/// Why [`detect_fname_ext`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtFail {
    /// A URL like `http://...` (`ext_len == -2`).
    Url,
    /// The first path segment is a registered alias (`ext_len == -1`);
    /// holds the offset of the `/` after it.
    Alias(usize),
    /// No acceptable extension; `true` when some extension was examined
    /// (`ext_str != NULL`).
    NotFound(bool),
}

fn stat_path(p: &[u8]) -> Option<php_streams::stat::StatBuf> {
    php_streams::stat::stat_path(p).ok()
}

/// `phar_analyze_path()`.
fn analyze_path(fname: &[u8], ext_end: usize, for_create: i32) -> bool {
    let filename = &fname[..ext_end];
    if let Some(real) = php_streams::paths::expand_filepath(filename) {
        let known = with(|s| {
            s.fname_map.contains_key(&real)
                || (s.manifest_cached && s.cached_phars.contains_key(&real))
        });
        if known {
            return true;
        }
    }
    if let Some(sb) = stat_path(filename) {
        if sb.is_dir() || for_create == 1 {
            return false;
        }
        return true;
    }
    if for_create == 0 {
        return false;
    }
    let slash = filename.iter().rposition(|&c| c == b'/');
    let dir = match slash {
        Some(p) => &filename[..p],
        None => filename,
    };
    if let Some(sb) = stat_path(dir) {
        sb.is_dir()
    } else {
        if slash.is_some() {
            return false;
        }
        let Some(mut real) = php_streams::paths::expand_filepath(dir) else {
            return false;
        };
        if let Some(p) = find(&real, dir) {
            real.truncate(p + ext_end);
        }
        let Some(p) = real.iter().rposition(|&c| c == b'/') else {
            return false;
        };
        real.truncate(p);
        stat_path(&real).is_some_and(|sb| sb.is_dir())
    }
}

/// `phar_check_str()`.
fn check_str(
    fname: &[u8],
    ext_start: usize,
    ext_len: usize,
    executable: i32,
    for_create: i32,
) -> bool {
    if ext_len >= 50 {
        return false;
    }
    let at = |i: usize| fname.get(i).copied().unwrap_or(0);
    let rest = &fname[ext_start..];
    let phar_pos = find(crate::util::cstr(rest), b".phar").map(|p| p + ext_start);
    let after_ok = |p: usize| matches!(at(p + 5), 0 | b'/' | b'.');
    if executable == 1 {
        let Some(pos) = phar_pos else { return false };
        if (pos != ext_start && at(pos - 1) == b'/')
            || ext_len.wrapping_sub(pos - ext_start) < 5
            || !after_ok(pos)
        {
            return false;
        }
        return analyze_path(fname, ext_start + ext_len, for_create);
    }
    let second = at(ext_start + 1);
    if executable == 0 {
        let has_phar = phar_pos.is_some_and(|pos| pos > 0 && at(pos - 1) != b'/' && after_ok(pos));
        if !has_phar && second != b'.' && second != b'/' && second != 0 {
            return analyze_path(fname, ext_start + ext_len, for_create);
        }
    } else if second != b'.' && second != b'/' && second != 0 {
        return analyze_path(fname, ext_start + ext_len, for_create);
    }
    false
}

/// `phar_detect_phar_fname_ext()`: finds the archive extension in a path.
/// `executable` is 0 (data), 1 (executable) or 2 (either); `for_create` 0,
/// 1 or 2 (2: either existing or creatable). Returns `(ext_start, ext_len)`.
pub fn detect_fname_ext(
    filename: &[u8],
    executable: i32,
    for_create: i32,
    is_complete: bool,
) -> Result<(usize, usize), ExtFail> {
    let len = filename.len();
    if len <= 1 {
        return Err(ExtFail::NotFound(false));
    }
    if let Some(pos) = filename.iter().position(|&c| c == b'/').filter(|&p| p != 0) {
        if filename[pos - 1] == b':' && pos < len - 1 && filename[pos + 1] == b'/' {
            return Err(ExtFail::Url);
        }
        let is_alias = with(|s| {
            s.alias_map.contains_key(&filename[..pos])
                || (s.manifest_cached && s.cached_alias.contains_key(&filename[..pos]))
        });
        if is_alias {
            return Err(ExtFail::Alias(pos));
        }
    }
    let known: Option<(usize, usize, bool)> = with(|s| {
        if s.fname_map.is_empty() && !s.manifest_cached {
            return None;
        }
        let check = |a: &ArchiveRef, key_len: usize| {
            let a = a.borrow();
            (key_len - a.ext_len.min(key_len), a.ext_len, a.is_data)
        };
        if is_complete {
            if let Some(a) = s.fname_map.get(filename) {
                return Some(check(a, len));
            }
            if s.manifest_cached {
                if let Some(a) = s.cached_phars.get(filename) {
                    return Some(check(a, len));
                }
            }
            return None;
        }
        let matches = |k: &[u8]| {
            k.len() <= len
                && filename.starts_with(k)
                && (len == k.len() || filename[k.len()] == b'/' || filename[k.len()] == 0)
        };
        for (k, a) in s.fname_map.iter() {
            if matches(k) {
                return Some(check(a, k.len()));
            }
        }
        if s.manifest_cached {
            for (k, a) in s.cached_phars.iter() {
                if matches(k) {
                    return Some(check(a, k.len()));
                }
            }
        }
        None
    });
    if let Some((start, elen, is_data)) = known {
        return match executable {
            2 => Ok((start, elen)),
            1 if !is_data => Ok((start, elen)),
            0 if is_data => Ok((start, elen)),
            _ => Err(ExtFail::NotFound(true)),
        };
    }
    let next_dot = |from: usize| {
        filename[from..]
            .iter()
            .position(|&c| c == b'.')
            .map(|p| p + from)
    };
    let mut pos = next_dot(1);
    let mut examined = false;
    while let Some(mut p) = pos {
        while p != 0 && (filename[p - 1] == b'/' || filename[p - 1] == 0) {
            match next_dot(p + 1) {
                Some(n) => p = n,
                None => return Err(ExtFail::NotFound(false)),
            }
        }
        let slash = filename[p..].iter().position(|&c| c == b'/').map(|x| x + p);
        let Some(slash) = slash else {
            let ext_len = crate::util::cstr(&filename[p..]).len();
            return if check_str(filename, p, ext_len, executable, for_create) {
                Ok((p, ext_len))
            } else {
                Err(ExtFail::NotFound(true))
            };
        };
        examined = true;
        if check_str(filename, p, slash - p, executable, for_create) {
            return Ok((p, slash - p));
        }
        pos = crate::util::cstr(&filename[p + 1..])
            .iter()
            .position(|&c| c == b'.')
            .map(|x| x + p + 1);
        if pos.is_some() {
            examined = false;
        }
    }
    Err(ExtFail::NotFound(examined))
}

/// Why [`split_fname`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitError {
    /// The path (without `phar://`) when no extension was found at all —
    /// used for the "no directory in" wrapper message.
    pub no_ext: Option<Vec<u8>>,
}

/// `phar_split_fname()`: splits `phar://path/to/a.phar/inner/file` into the
/// archive path and the normalised entry path (starting with `/`).
pub fn split_fname(
    filename: &[u8],
    executable: i32,
    for_create: i32,
) -> Result<(Vec<u8>, Vec<u8>), SplitError> {
    if filename.contains(&0) {
        return Err(SplitError { no_ext: None });
    }
    let f = if filename.len() >= 7 && filename[..7].eq_ignore_ascii_case(b"phar://") {
        &filename[7..]
    } else {
        filename
    };
    let (start, len) = match detect_fname_ext(f, executable, for_create, false) {
        Ok(x) => x,
        Err(ExtFail::Alias(pos)) => (pos, 0),
        Err(ExtFail::NotFound(false)) => {
            return Err(SplitError {
                no_ext: Some(f.to_vec()),
            })
        }
        Err(_) => return Err(SplitError { no_ext: None }),
    };
    let arch_len = start + len;
    let arch = f[..arch_len].to_vec();
    let entry = if arch_len < f.len() {
        fix_filepath(&f[arch_len..], None)
    } else {
        b"/".to_vec()
    };
    Ok((arch, entry))
}

// --------------------------------------------------------------- opening

/// `phar_open_parsed_phar()`.
fn open_parsed(
    fname: &[u8],
    alias: Option<&[u8]>,
    is_data: bool,
) -> Result<Option<ArchiveRef>, String> {
    let phar = get_archive(fname, alias)?;
    let Some(phar) = phar else {
        return Ok(None);
    };
    if alias.is_some() && phar.borrow().fname != fname {
        return Ok(None);
    }
    if !is_data {
        let a = phar.borrow();
        if a.halt_offset == 0
            && !a.is_brandnew
            && (a.is_tar || a.is_zip)
            && readonly()
            && !a.manifest.contains_key(b".phar/stub.php")
        {
            return Err(format!(
                "'{}' is not a phar archive. Use PharData::__construct() for a standard zip or tar archive",
                s(fname)
            ));
        }
    }
    Ok(Some(phar))
}

/// Registers a freshly parsed archive (format-specific alias rules).
fn register_phar(
    parsed: crate::phar_format::ParsedPhar,
    fname: &[u8],
) -> Result<ArchiveRef, String> {
    let ar = parsed.archive.into_ref();
    if parsed.register_alias {
        let alias = ar.borrow().alias_bytes().to_vec();
        if !validate_alias(&alias) {
            return Err(format!(
                "Cannot open archive \"{}\", invalid alias",
                s(fname)
            ));
        }
        if let Some(fd) = find_by_alias(&alias) {
            if !free_alias(&fd) {
                return Err(format!(
                    "Cannot open archive \"{}\", alias is already in use by existing archive",
                    s(fname)
                ));
            }
        }
        with(|s| s.alias_map.add(alias, Rc::clone(&ar)));
    } else {
        ar.borrow_mut().is_temporary_alias = true;
    }
    with(|s| s.fname_map.add(fname.to_vec(), Rc::clone(&ar)));
    Ok(ar)
}

fn register_tar(
    parsed: crate::tar_format::ParsedTar,
    fname: &[u8],
    alias: Option<&[u8]>,
) -> Result<ArchiveRef, String> {
    let ar = parsed.archive.into_ref();
    if !with(|s| s.fname_map.add(fname.to_vec(), Rc::clone(&ar))) {
        return Err(format!(
            "phar error: Unable to add tar-based phar \"{}\" to phar registry",
            s(fname)
        ));
    }
    let in_use = || {
        format!(
            "phar error: Unable to add tar-based phar \"{}\", alias is already in use",
            s(fname)
        )
    };
    if let Some(actual) = parsed.actual_alias {
        ar.borrow_mut().is_temporary_alias = false;
        if let Some(fd) = find_by_alias(&actual) {
            if !free_alias(&fd) {
                fname_map_del(fname);
                return Err(in_use());
            }
        }
        ar.borrow_mut().alias = Some(actual.clone());
        with(|s| s.alias_map.add(actual, Rc::clone(&ar)));
    } else {
        if let Some(a) = alias {
            if let Some(fd) = find_by_alias(a) {
                if !free_alias(&fd) {
                    fname_map_del(fname);
                    return Err(in_use());
                }
            }
            with(|s| s.alias_map.add(a.to_vec(), Rc::clone(&ar)));
            ar.borrow_mut().alias = Some(a.to_vec());
        } else {
            ar.borrow_mut().alias = Some(fname.to_vec());
        }
        ar.borrow_mut().is_temporary_alias = true;
    }
    Ok(ar)
}

fn register_zip(
    parsed: crate::zip_format::ParsedZip,
    fname: &[u8],
    alias: Option<&[u8]>,
) -> Result<ArchiveRef, String> {
    let ar = parsed.archive.into_ref();
    with(|s| s.fname_map.add(fname.to_vec(), Rc::clone(&ar)));
    if let Some(actual) = parsed.actual_alias {
        if !validate_alias(&actual) {
            fname_map_del(fname);
            return Err(format!(
                "phar error: invalid alias \"{}\" in zip-based phar \"{}\"",
                s(crate::util::cstr(&actual)),
                s(fname)
            ));
        }
        ar.borrow_mut().is_temporary_alias = false;
        if let Some(fd) = find_by_alias(&actual) {
            if !free_alias(&fd) {
                fname_map_del(fname);
                return Err(format!(
                    "phar error: Unable to add zip-based phar \"{}\" with implicit alias, alias is already in use",
                    s(fname)
                ));
            }
        }
        ar.borrow_mut().alias = Some(actual.clone());
        with(|s| s.alias_map.add(actual, Rc::clone(&ar)));
    } else {
        if let Some(a) = alias {
            if let Some(fd) = find_by_alias(a) {
                if !free_alias(&fd) {
                    fname_map_del(fname);
                    return Err(format!(
                        "phar error: Unable to add zip-based phar \"{}\" with explicit alias, alias is already in use",
                        s(fname)
                    ));
                }
            }
            with(|s| s.alias_map.add(a.to_vec(), Rc::clone(&ar)));
            ar.borrow_mut().alias = Some(a.to_vec());
        } else {
            ar.borrow_mut().alias = Some(fname.to_vec());
        }
        ar.borrow_mut().is_temporary_alias = true;
    }
    Ok(ar)
}

/// `phar_open_from_fp()`: detects the format of `data` (decompressing
/// whole-archive gzip/bzip2), parses and registers it.
pub fn open_from_bytes(
    data: Vec<u8>,
    fname: &[u8],
    alias: Option<&[u8]>,
) -> Result<ArchiveRef, String> {
    let f = s(fname);
    let mut data = data;
    let mut compression = 0u32;
    let mut recursion = 3;
    let req_hash = require_hash();
    loop {
        match crate::phar_format::detect(&data, fname, recursion > 0)? {
            Detected::Gzip => {
                if !has_zlib() {
                    return Err(format!("unable to decompress gzipped phar archive \"{f}\" to temporary file, enable zlib extension in php.ini"));
                }
                let Some(out) = crate::compress::gunzip(&data) else {
                    return Err(format!(
                        "unable to decompress gzipped phar archive \"{f}\" to temporary file"
                    ));
                };
                data = out;
                compression = FILE_COMPRESSED_GZ;
                recursion -= 1;
                if recursion == 0 {
                    return Err(format!("unable to decompress gzipped phar archive \"{f}\""));
                }
            }
            Detected::Bzip2 => {
                if !has_bz2() {
                    return Err(format!("unable to decompress bzipped phar archive \"{f}\" to temporary file, enable bz2 extension in php.ini"));
                }
                let (out, _) = crate::bz2::decompress(&data);
                data = out;
                compression = FILE_COMPRESSED_BZ2;
                recursion -= 1;
                if recursion == 0 {
                    return Err(format!("unable to decompress bzipped phar archive \"{f}\""));
                }
            }
            Detected::Zip => {
                let parsed = crate::zip_format::parse(Rc::new(data), fname, req_hash)?;
                return register_zip(parsed, fname, alias);
            }
            Detected::Tar => {
                let parsed = crate::tar_format::parse(Rc::new(data), fname, compression, req_hash)?;
                return register_tar(parsed, fname, alias);
            }
            Detected::Phar(halt) => {
                let parsed = crate::phar_format::parse(
                    Rc::new(data),
                    fname,
                    alias,
                    halt,
                    compression,
                    req_hash,
                )?;
                return register_phar(parsed, fname);
            }
        }
    }
}

/// [`open_from_bytes`] for data just read from `path`, remembering the
/// file's identity when the image mirrors it.
fn open_with_stamp(
    path: &[u8],
    data: Vec<u8>,
    actual: &[u8],
    alias: Option<&[u8]>,
) -> Result<ArchiveRef, String> {
    let stamp = crate::archive::DiskStamp::of(path);
    let ar = open_from_bytes(data, actual, alias)?;
    let mut a = ar.borrow_mut();
    if a.flags & FILE_COMPRESSION_MASK == 0 && a.fp_disk.is_none() {
        a.fp_disk = stamp;
    }
    drop(a);
    Ok(ar)
}

/// `phar_open_from_filename()`. `Err(None)` is a failure without message.
pub fn open_from_filename(
    fname: &[u8],
    alias: Option<&[u8]>,
    report_errors: bool,
) -> Result<ArchiveRef, Option<String>> {
    let is_data = find(fname, b".phar").is_none();
    match open_parsed(fname, alias, is_data) {
        Ok(Some(a)) => return Ok(a),
        Ok(None) => {}
        Err(e) => return Err(Some(e)),
    }
    if !php_streams::paths::check_open_basedir(fname) {
        return Err(None);
    }
    let Ok(data) = crate::sys::read_file(fname) else {
        return Err(
            report_errors.then(|| format!("unable to open phar for reading \"{}\"", s(fname)))
        );
    };
    let actual = php_streams::paths::expand_filepath(fname).unwrap_or_else(|| fname.to_vec());
    open_with_stamp(fname, data, &actual, alias).map_err(Some)
}

/// `phar_create_or_parse_filename()`.
pub fn create_or_parse_filename(
    fname: &[u8],
    alias: Option<&[u8]>,
    is_data: bool,
    report_errors: bool,
) -> Result<ArchiveRef, Option<String>> {
    if !php_streams::paths::check_open_basedir(fname) {
        return Err(None);
    }
    if let Ok(data) = crate::sys::read_file(fname) {
        if crate::sys::path(fname).is_file() {
            let actual =
                php_streams::paths::expand_filepath(fname).unwrap_or_else(|| fname.to_vec());
            let ar = open_with_stamp(fname, data, &actual, alias).map_err(Some)?;
            {
                let mut a = ar.borrow_mut();
                if a.is_data || !readonly() {
                    a.is_writeable = true;
                }
            }
            return Ok(ar);
        }
    }
    if readonly() && !is_data {
        return Err(report_errors.then(|| {
            format!(
                "creating archive \"{}\" disabled by the php.ini setting phar.readonly",
                s(fname)
            )
        }));
    }
    let Some(new_fname) = php_streams::paths::expand_filepath(fname) else {
        return Err(None);
    };
    let mut ar = Archive::new(new_fname.clone());
    ar.is_temporary_alias = alias.is_none();
    let alias = if is_data { None } else { alias };
    if is_data {
        ar.is_data = true;
        ar.is_tar = true;
        ar.alias = None;
    } else {
        ar.alias = Some(alias.map_or_else(|| new_fname.clone(), <[u8]>::to_vec));
    }
    let ar = ar.into_ref();
    with(|s| s.fname_map.add(new_fname.clone(), Rc::clone(&ar)));
    if !is_data {
        if let Some(a) = alias {
            if let Some(fd) = find_by_alias(a) {
                if !free_alias(&fd) {
                    fname_map_del(&new_fname);
                    return Err(Some(format!(
                        "phar error: phar \"{}\" cannot set alias \"{}\", already in use by another phar archive",
                        s(&new_fname),
                        s(a)
                    )));
                }
            }
        }
    }
    if let Some(a) = alias {
        if !with(|s| s.alias_map.add(a.to_vec(), Rc::clone(&ar))) {
            fname_map_del(&new_fname);
            return Err(report_errors.then(|| {
                format!(
                    "archive \"{}\" cannot be associated with alias \"{}\", already in use",
                    s(fname),
                    s(a)
                )
            }));
        }
    }
    Ok(ar)
}

fn open_or_create_kind(
    fname: &[u8],
    alias: Option<&[u8]>,
    is_data: bool,
    report_errors: bool,
    zip: bool,
) -> Result<ArchiveRef, Option<String>> {
    let ar = create_or_parse_filename(fname, alias, is_data, report_errors)?;
    let mut a = ar.borrow_mut();
    a.is_data = is_data;
    if (zip && a.is_zip) || (!zip && a.is_tar) {
        drop(a);
        return Ok(ar);
    }
    if a.is_brandnew {
        a.is_zip = zip;
        a.is_tar = !zip;
        drop(a);
        return Ok(ar);
    }
    Err(Some(if zip {
        format!("phar zip error: phar \"{}\" already exists as a regular phar and must be deleted from disk prior to creating as a zip-based phar", s(fname))
    } else {
        format!("phar tar error: \"{}\" already exists as a regular phar and must be deleted from disk prior to creating as a tar-based phar", s(fname))
    }))
}

/// `phar_open_or_create_filename()`: opens an existing archive or creates
/// a new one (format chosen by extension).
pub fn open_or_create_filename(
    fname: &[u8],
    alias: Option<&[u8]>,
    is_data: bool,
    report_errors: bool,
) -> Result<ArchiveRef, Option<String>> {
    let exec = i32::from(!is_data);
    let ext = match detect_fname_ext(fname, exec, 0, true) {
        Ok(e) => e,
        Err(_) => match detect_fname_ext(fname, exec, 1, true) {
            Ok(e) => e,
            Err(ExtFail::Url) => {
                return Err(Some(format!(
                    "Cannot create a phar archive from a URL like \"{}\". Phar objects can only be created from local files",
                    s(fname)
                )))
            }
            Err(_) => {
                return Err(Some(format!(
                    "Cannot create phar '{}', file extension (or combination) not recognised or the directory does not exist",
                    s(fname)
                )))
            }
        },
    };
    match open_parsed(fname, alias, is_data) {
        Ok(Some(test)) => {
            {
                let t = test.borrow();
                if t.is_data && !t.is_tar && !t.is_zip {
                    return Err(Some(format!(
                        "Cannot open '{}' as a PharData object. Use Phar::__construct() for executable archives",
                        s(fname)
                    )));
                }
                if readonly()
                    && !t.is_data
                    && (t.is_tar || t.is_zip)
                    && !t.manifest.contains_key(b".phar/stub.php")
                {
                    return Err(Some(format!(
                        "'{}' is not a phar archive. Use PharData::__construct() for a standard zip or tar archive",
                        s(fname)
                    )));
                }
            }
            if !readonly() || test.borrow().is_data {
                test.borrow_mut().is_writeable = true;
            }
            return Ok(test);
        }
        Ok(None) => {}
        Err(e) => return Err(Some(e)),
    }
    let ext_bytes = &fname[ext.0..ext.0 + ext.1];
    let has = |c: u8, rest: &[u8]| {
        ext.1 > 3
            && ext_bytes.iter().position(|&x| x == c).is_some_and(|z| {
                ext.1 - z >= 2 && fname.get(ext.0 + z + 1..ext.0 + z + 3) == Some(rest)
            })
    };
    if has(b'z', b"ip") {
        return open_or_create_kind(fname, alias, is_data, report_errors, true);
    }
    if has(b't', b"ar") {
        return open_or_create_kind(fname, alias, is_data, report_errors, false);
    }
    create_or_parse_filename(fname, alias, is_data, report_errors)
}

/// `phar_open_executed_filename()` (`Phar::mapPhar()` / `webPhar()`).
/// `halt_defined` is whether `__COMPILER_HALT_OFFSET__` exists.
pub fn open_executed_filename(
    executed: Option<&[u8]>,
    alias: Option<&[u8]>,
    halt_defined: bool,
) -> Result<(), Option<String>> {
    let Some(fname) = executed else {
        return Err(Some(
            "cannot initialize a phar outside of PHP execution".into(),
        ));
    };
    if let Ok(Some(_)) = open_parsed(fname, alias, false) {
        return Ok(());
    }
    if !halt_defined {
        return Err(Some("__HALT_COMPILER(); must be declared in a phar".into()));
    }
    if !php_streams::paths::check_open_basedir(fname) {
        return Err(None);
    }
    let Ok(data) = crate::sys::read_file(fname) else {
        return Err(Some(format!(
            "unable to open phar for reading \"{}\"",
            s(fname)
        )));
    };
    let actual = php_streams::paths::expand_filepath(fname).unwrap_or_else(|| fname.to_vec());
    open_with_stamp(fname, data, &actual, alias)
        .map(|_| ())
        .map_err(Some)
}

/// `phar.cache_list`: pre-parses the listed archives into the persistent
/// manifest cache. Returns `false` (and caches nothing) when one fails.
pub fn load_cache_list(list: &[u8]) -> bool {
    let paths: Vec<&[u8]> = list
        .split(|&c| c == b':')
        .filter(|p| !p.is_empty())
        .collect();
    if paths.is_empty() {
        return true;
    }
    let saved = with(|s| {
        (
            std::mem::take(&mut s.fname_map),
            std::mem::take(&mut s.alias_map),
        )
    });
    let mut ok = true;
    for p in &paths {
        if let Ok(a) = open_from_filename(p, None, false) {
            let mut a = a.borrow_mut();
            a.is_persistent = true;
            a.fp = None;
            for (_, e) in a.manifest.iter_mut() {
                e.is_persistent = true;
            }
        } else {
            ok = false;
            break;
        }
    }
    with(|s| {
        let f = std::mem::replace(&mut s.fname_map, saved.0);
        let a = std::mem::replace(&mut s.alias_map, saved.1);
        if ok {
            s.cached_phars = f;
            s.cached_alias = a;
            s.manifest_cached = true;
        }
    });
    ok
}

/// `phar_copy_on_write()`: replaces a cached (persistent) archive by a
/// writable per-request copy.
pub fn copy_on_write(ar: &ArchiveRef) -> Option<ArchiveRef> {
    if !ar.borrow().is_persistent {
        return Some(Rc::clone(ar));
    }
    let mut copy = ar.borrow().clone();
    copy.is_persistent = false;
    for (_, e) in copy.manifest.iter_mut() {
        e.is_persistent = false;
    }
    let fname = copy.fname.clone();
    let alias = copy.alias.clone();
    let new = copy.into_ref();
    if !with(|s| s.fname_map.add(fname.clone(), Rc::clone(&new))) {
        return None;
    }
    invalidate_last();
    if let Some(a) = alias.filter(|a| !a.is_empty()) {
        if !with(|s| s.alias_map.add(a, Rc::clone(&new))) {
            fname_map_del(&fname);
            return None;
        }
    }
    Some(new)
}

// --------------------------------------------------------------- flushing

/// Writes `image` to the archive file, applying whole-archive compression.
fn write_out(fname: &[u8], image: &[u8], flags: u32) -> Result<(), String> {
    let mut opened = None;
    let Some(mut out) = php_streams::open::open_wrapper(
        fname,
        "w+b",
        php_streams::wrapper::IGNORE_URL
            | php_streams::wrapper::STREAM_MUST_SEEK
            | php_streams::wrapper::REPORT_ERRORS,
        &mut opened,
        None,
    ) else {
        return Err(format!(
            "unable to open new phar \"{}\" for writing",
            s(fname)
        ));
    };
    let data = if flags & FILE_COMPRESSED_GZ != 0 {
        crate::compress::gzip(image).unwrap_or_default()
    } else if flags & FILE_COMPRESSED_BZ2 != 0 {
        crate::compress::bzip2(image).unwrap_or_default()
    } else {
        image.to_vec()
    };
    let _ = out.write(&data);
    out.close();
    php_streams::state::clear_stat_cache();
    Ok(())
}

/// `phar_flush_ex()`: saves the archive (honouring `startBuffering()`).
/// Silent no-ops (PHP's `EOF` without error) return `Ok`.
pub fn flush_ex(
    ar_ref: &ArchiveRef,
    user_stub: Option<&[u8]>,
    is_default_stub: bool,
) -> Result<(), String> {
    let key = openssl_privatekey();
    {
        let a = ar_ref.borrow();
        if a.is_persistent {
            let kind = if a.is_tar { "tar-based" } else { "zip-based" };
            return Err(format!(
                "internal error: attempt to flush cached {kind} phar \"{}\"",
                s(&a.fname)
            ));
        }
        if a.manifest.is_empty() && user_stub.is_none() {
            return Ok(());
        }
    }
    let (is_zip, is_tar) = {
        let a = ar_ref.borrow();
        (a.is_zip, a.is_tar)
    };
    let image = if is_zip {
        crate::zip_format::prepare_alias(&mut ar_ref.borrow_mut());
        let (fname, alias) = {
            let a = ar_ref.borrow();
            (a.fname.clone(), a.alias.clone())
        };
        if let Some(al) = alias.filter(|a| !a.is_empty()) {
            match get_archive(&fname, Some(&al)) {
                Ok(Some(_)) => {}
                Ok(None) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
        crate::zip_format::flush(&mut ar_ref.borrow_mut(), user_stub, is_default_stub, &key)?
    } else if is_tar {
        crate::tar_format::flush(&mut ar_ref.borrow_mut(), user_stub, is_default_stub, &key)?
    } else {
        if readonly() {
            return Ok(());
        }
        let mut a = ar_ref.borrow_mut();
        let old = if a.fp.is_some() && !a.is_brandnew {
            a.open_archive_fp()
        } else {
            crate::sys::read_file(&a.fname).ok().map(Rc::new)
        };
        crate::phar_format::flush(&mut a, user_stub, old.as_ref(), &key)?.image
    };
    let mut a = ar_ref.borrow_mut();
    a.fp = Some(Rc::new(image));
    a.fp_disk = None;
    if a.donotflush {
        return Ok(());
    }
    let fname = a.fname.clone();
    let flags = a.flags;
    let img = a.fp.clone().unwrap_or_default();
    drop(a);
    write_out(&fname, &img, flags)?;
    if flags & FILE_COMPRESSION_MASK == 0 {
        ar_ref.borrow_mut().fp_disk = crate::archive::DiskStamp::of(&fname);
    }
    Ok(())
}

/// `phar_flush()`.
pub fn flush(ar: &ArchiveRef) -> Result<(), String> {
    flush_ex(ar, None, false)
}

// ----------------------------------------------------------- entry access

/// A manifest entry or a synthesised directory.
#[derive(Debug, Clone)]
pub enum EntryRef {
    /// A manifest entry (by key).
    Manifest(Vec<u8>),
    /// A temporary directory entry (`is_temp_dir`), not in the manifest.
    TempDir(Box<Entry>),
}

impl EntryRef {
    /// Runs `f` on the entry.
    pub fn with<R>(&self, ar: &Archive, f: impl FnOnce(&Entry) -> R) -> Option<R> {
        match self {
            EntryRef::Manifest(k) => ar.manifest.get(k).map(f),
            EntryRef::TempDir(e) => Some(f(e)),
        }
    }

    /// The manifest key, if any.
    pub fn key(&self) -> Option<&[u8]> {
        match self {
            EntryRef::Manifest(k) => Some(k),
            EntryRef::TempDir(_) => None,
        }
    }
}

/// `phar_mount_entry()`: maps an external file or directory to `path`.
pub fn mount_entry(ar: &ArchiveRef, filename: &[u8], path: &[u8]) -> bool {
    let (check, path) = path_check(path);
    if check.is_err() || is_magic_phar(path) {
        return false;
    }
    let is_phar = filename.len() > 7 && filename.starts_with(b"phar://");
    let tmp = if is_phar {
        filename.to_vec()
    } else {
        php_streams::paths::expand_filepath(filename).unwrap_or_else(|| filename.to_vec())
    };
    if !is_phar && !php_streams::paths::check_open_basedir(&tmp) {
        return false;
    }
    let sb = if is_phar {
        php_streams::open::stat_path(&tmp, 0, None)
    } else {
        stat_path(&tmp)
    };
    let Some(sb) = sb else {
        return false;
    };
    let mut e = Entry {
        filename: path.to_vec(),
        is_mounted: true,
        is_crc_checked: true,
        fp_type: FpType::Tmp,
        tmp: Some(tmp),
        flags: crate::util::lo32(sb.mode),
        ..Entry::default()
    };
    let mut a = ar.borrow_mut();
    if sb.is_dir() {
        e.is_dir = true;
        if !a.mounted_dirs.add(path.to_vec(), ()) {
            return false;
        }
    } else {
        let size = crate::util::to_u32(usize::try_from(sb.size).unwrap_or(0));
        e.uncompressed_filesize = size;
        e.compressed_filesize = size;
    }
    a.manifest.add(path.to_vec(), e)
}

/// `phar_get_entry_info_dir()`. `dir`: 0 files only, 1 files or
/// directories (including implicit ones), 2 directories only. `Err(None)`
/// means "not found" without message.
#[allow(clippy::too_many_lines)]
pub fn get_entry_info_dir(
    ar: &ArchiveRef,
    path: &[u8],
    dir: u8,
    security: bool,
) -> Result<EntryRef, Option<String>> {
    let is_dir = path.last() == Some(&b'/');
    if security && is_magic_phar(path) {
        return Err(Some(
            "phar error: cannot directly access magic \".phar\" directory or files within it"
                .into(),
        ));
    }
    if path.is_empty() && dir == 0 {
        return Err(Some(
            "phar error: invalid path \"\" must not be empty".into(),
        ));
    }
    let (check, p) = path_check(path);
    if let crate::util::PathCheck::Err(e) = check {
        return Err(Some(format!(
            "phar error: invalid path \"{}\" contains {e}",
            s(path)
        )));
    }
    let mut p = p.to_vec();
    if is_dir {
        if p.len() <= 1 {
            return Err(None);
        }
        p.pop();
    }
    {
        let a = ar.borrow();
        if let Some(e) = a.manifest.get(&p) {
            if e.is_deleted {
                return Err(None);
            }
            if e.is_dir && dir == 0 {
                return Err(Some(format!(
                    "phar error: path \"{}\" is a directory",
                    s(&p)
                )));
            }
            if !e.is_dir && dir == 2 {
                return Err(Some(format!(
                    "phar error: path \"{}\" exists and is not a directory",
                    s(&p)
                )));
            }
            return Ok(EntryRef::Manifest(p));
        }
        if dir != 0 && a.virtual_dirs.contains_key(&p) {
            return Ok(EntryRef::TempDir(Box::new(Entry {
                filename: p.clone(),
                is_temp_dir: true,
                is_dir: true,
                ..Entry::default()
            })));
        }
    }
    let mounts: Vec<Vec<u8>> = ar.borrow().mounted_dirs.keys();
    for key in mounts {
        if key.len() >= p.len() || !p.starts_with(&key) {
            continue;
        }
        let tmp = {
            let a = ar.borrow();
            let Some(e) = a.manifest.get(&key) else {
                return Err(Some(format!(
                    "phar internal error: mounted path \"{}\" could not be retrieved from manifest",
                    s(&key)
                )));
            };
            match (&e.tmp, e.is_mounted) {
                (Some(t), true) => t.clone(),
                _ => {
                    return Err(Some(format!(
                        "phar internal error: mounted path \"{}\" is not properly initialized as a mounted path",
                        s(&key)
                    )))
                }
            }
        };
        let mut test = tmp;
        test.extend_from_slice(&p[key.len()..]);
        let Some(sb) = php_streams::open::stat_path(&test, 0, None) else {
            return Err(None);
        };
        if sb.is_dir() && dir == 0 {
            return Err(Some(format!(
                "phar error: path \"{}\" is a directory",
                s(&p)
            )));
        }
        if !sb.is_dir() && dir != 0 {
            return Err(Some(format!(
                "phar error: path \"{}\" exists and is not a directory",
                s(&p)
            )));
        }
        if !mount_entry(ar, &test, &p) {
            return Err(Some(format!(
                "phar error: path \"{}\" exists as file \"{}\" and could not be mounted",
                s(&p),
                s(&test)
            )));
        }
        if !ar.borrow().manifest.contains_key(&p) {
            return Err(Some(format!(
                "phar error: path \"{}\" exists as file \"{}\" and could not be retrieved after being mounted",
                s(&p),
                s(&test)
            )));
        }
        return Ok(EntryRef::Manifest(p));
    }
    Err(None)
}

/// An open handle on an entry (`phar_entry_data`).
#[derive(Debug)]
pub struct EntryData {
    /// The archive.
    pub archive: ArchiveRef,
    /// The entry.
    pub entry: EntryRef,
    /// Stream position.
    pub position: u64,
    /// Where offset 0 is in the entry's fp.
    pub zero: u64,
}

impl EntryData {
    /// Runs `f` on the entry.
    pub fn with_entry<R>(&self, f: impl FnOnce(&Entry) -> R) -> Option<R> {
        self.entry.with(&self.archive.borrow(), f)
    }

    /// Runs `f` on the (manifest) entry mutably.
    pub fn with_entry_mut<R>(&mut self, f: impl FnOnce(&mut Entry) -> R) -> Option<R> {
        match &mut self.entry {
            EntryRef::Manifest(k) => self.archive.borrow_mut().manifest.get_mut(k).map(f),
            EntryRef::TempDir(e) => Some(f(e)),
        }
    }
}

fn mode_flags(mode: &str) -> (bool, bool, bool, bool) {
    let m = mode.as_bytes();
    let first = m.first().copied().unwrap_or(b'r');
    let second = m.get(1).copied().unwrap_or(0);
    (
        first != b'r' || second == b'+',
        first == b'a',
        first != b'r',
        first == b'w',
    )
}

/// `phar_get_entry_data()`: opens an existing entry. `Ok(None)` means the
/// entry does not exist but may be created.
#[allow(clippy::too_many_lines)]
pub fn get_entry_data(
    fname: &[u8],
    path: &[u8],
    mode: &str,
    allow_dir: u8,
    security: bool,
) -> Result<Option<EntryData>, Option<String>> {
    let (for_write, for_append, for_create, for_trunc) = mode_flags(mode);
    let phar = match get_archive(fname, None) {
        Ok(Some(p)) => p,
        Ok(None) => return Err(None),
        Err(e) => return Err(Some(e)),
    };
    let ro = readonly();
    let is_data = phar.borrow().is_data;
    if for_write && ro && !is_data {
        return Err(Some(format!(
            "phar error: file \"{}\" in phar \"{}\" cannot be opened for writing, disabled by ini setting",
            s(path),
            s(fname)
        )));
    }
    if path.is_empty() {
        return Err(Some(format!(
            "phar error: file \"\" in phar \"{}\" must not be empty",
            s(fname)
        )));
    }
    let creating = for_create && (!ro || is_data);
    let mut phar = phar;
    let entry = loop {
        let r = get_entry_info_dir(&phar, path, allow_dir, security);
        let entry = match r {
            Ok(e) => e,
            Err(e) => {
                if creating {
                    return Ok(None);
                }
                return Err(e);
            }
        };
        if for_write && phar.borrow().is_persistent {
            match copy_on_write(&phar) {
                Some(p) => {
                    phar = p;
                    continue;
                }
                None => {
                    return Err(Some(format!(
                        "phar error: file \"{}\" in phar \"{}\" cannot be opened for writing, could not make cached phar writeable",
                        s(path),
                        s(fname)
                    )))
                }
            }
        }
        break entry;
    };
    let (is_modified, fp_refcount, is_deleted, is_dir, fp_type) = entry
        .with(&phar.borrow(), |e| {
            (
                e.is_modified,
                e.fp_refcount,
                e.is_deleted,
                e.is_dir,
                e.fp_type,
            )
        })
        .unwrap_or_default();
    if is_modified && !for_write {
        return Err(Some(format!(
            "phar error: file \"{}\" in phar \"{}\" cannot be opened for reading, writable file pointers are open",
            s(path),
            s(fname)
        )));
    }
    if fp_refcount > 0 && for_write {
        return Err(Some(format!(
            "phar error: file \"{}\" in phar \"{}\" cannot be opened for writing, readable file pointers are open",
            s(path),
            s(fname)
        )));
    }
    let key = entry.key().map(<[u8]>::to_vec);
    if is_deleted {
        if !for_create {
            return Err(None);
        }
        if let Some(k) = &key {
            if let Some(e) = phar.borrow_mut().manifest.get_mut(k) {
                e.is_deleted = false;
            }
        }
    }
    let mut data = EntryData {
        archive: Rc::clone(&phar),
        entry,
        position: 0,
        zero: 0,
    };
    if is_dir {
        if !phar.borrow().is_persistent {
            addref(&phar);
            data.with_entry_mut(|e| e.fp_refcount += 1);
        }
        return Ok(Some(data));
    }
    let Some(k) = key else {
        return Ok(Some(data));
    };
    if fp_type == FpType::Mod {
        if for_trunc {
            phar.borrow_mut().truncate_entry(&k);
        } else if for_append {
            data.position = 0;
        }
    } else if for_write {
        let is_tar = phar.borrow().is_tar;
        if let Some(e) = phar.borrow_mut().manifest.get_mut(&k) {
            if e.symlink.take().is_some() {
                e.tar_type = if e.is_tar || is_tar { TAR_FILE } else { 0 };
            }
        }
        if for_trunc {
            phar.borrow_mut().truncate_entry(&k);
        } else {
            phar.borrow_mut().separate_entry(&k).map_err(Some)?;
        }
    } else {
        phar.borrow_mut().open_entry_fp(&k, true).map_err(Some)?;
    }
    {
        let a = phar.borrow();
        let src = a.link_source(&k);
        let Some(src) = src else {
            return Err(None);
        };
        data.zero = a.manifest.get(&src).map_or(0, |e| e.offset);
    }
    if !phar.borrow().is_persistent {
        if let Some(e) = phar.borrow_mut().manifest.get_mut(&k) {
            e.fp_refcount += 1;
        }
        addref(&phar);
    }
    Ok(Some(data))
}

/// `phar_get_or_create_entry_data()`: opens or creates an entry for
/// writing. `allow_dir == 2` creates a directory.
pub fn get_or_create_entry_data(
    fname: &[u8],
    path: &[u8],
    mode: &str,
    allow_dir: u8,
    security: bool,
    timestamp: u32,
) -> Result<EntryData, Option<String>> {
    let is_dir = path.last() == Some(&b'/');
    let phar = match get_archive(fname, None) {
        Ok(Some(p)) => p,
        Ok(None) => return Err(None),
        Err(e) => return Err(Some(e)),
    };
    if let Some(d) = get_entry_data(fname, path, mode, allow_dir, security)? {
        return Ok(d);
    }
    let (check, checked) = path_check(path);
    if let crate::util::PathCheck::Err(e) = check {
        return Err(Some(format!(
            "phar error: invalid path \"{}\" contains {e}",
            s(path)
        )));
    }
    let mut p = checked.to_vec();
    let phar = if phar.borrow().is_persistent {
        copy_on_write(&phar).ok_or_else(|| {
            Some(format!(
                "phar error: file \"{}\" in phar \"{}\" cannot be created, could not make cached phar writeable",
                s(&p),
                s(fname)
            ))
        })?
    } else {
        phar
    };
    let mut e = Entry::new_file(Vec::new(), timestamp);
    e.fp_refcount = 1;
    if allow_dir == 2 {
        e.is_dir = true;
        e.flags = ENT_PERM_DEF_DIR;
        e.old_flags = ENT_PERM_DEF_DIR;
    } else {
        e.flags = ENT_PERM_DEF_FILE;
        e.old_flags = ENT_PERM_DEF_FILE;
    }
    if is_dir && !p.is_empty() {
        p.pop();
    }
    let mut a = phar.borrow_mut();
    a.add_virtual_dirs(&p);
    e.filename.clone_from(&p);
    e.is_zip = a.is_zip;
    if a.is_tar {
        e.is_tar = true;
        e.tar_type = if e.is_dir { TAR_DIR } else { TAR_FILE };
    }
    if !a.manifest.add(p.clone(), e) {
        return Err(Some(format!(
            "phar error: unable to add new entry \"{}\" to phar \"{}\"",
            s(&p),
            s(&a.fname)
        )));
    }
    a.refcount += 1;
    drop(a);
    Ok(EntryData {
        archive: phar,
        entry: EntryRef::Manifest(p),
        position: 0,
        zero: 0,
    })
}

/// `phar_entry_delref()`: releases an [`EntryData`].
pub fn entry_delref(mut data: EntryData) {
    let persistent = data.with_entry(|e| e.is_persistent).unwrap_or(false);
    if !persistent {
        data.with_entry_mut(|e| {
            e.fp_refcount = (e.fp_refcount - 1).max(0);
        });
    }
    delref(&data.archive);
}

/// `phar_entry_remove()`: deletes the entry (or marks it deleted while
/// still referenced) and flushes unless buffering.
pub fn entry_remove(data: EntryData) -> Result<(), String> {
    let phar = Rc::clone(&data.archive);
    let (refc, locks) = data
        .with_entry(|e| (e.fp_refcount, e.fileinfo_lock_count))
        .unwrap_or_default();
    if refc < 2 && locks == 0 {
        if let Some(k) = data.entry.key() {
            phar.borrow_mut().manifest.remove(k);
        }
        phar.borrow_mut().refcount -= 1;
    } else {
        let mut data = data;
        data.with_entry_mut(|e| e.is_deleted = true);
        entry_delref(data);
    }
    if !phar.borrow().donotflush {
        flush(&phar)?;
    }
    Ok(())
}

/// `phar_find_in_include_path()`: resolves `filename` relative to the
/// running phar (`executed` is the executing file name).
pub fn find_in_include_path(
    filename: &[u8],
    executed: &[u8],
    include_path: &[u8],
) -> Option<Vec<u8>> {
    let cwd = cwd()?;
    if executed.len() < 7 || !executed[..7].eq_ignore_ascii_case(b"phar://") {
        return None;
    }
    let (arch, _) = split_fname(executed, 1, 0).ok()?;
    if filename.first() == Some(&b'.') {
        let phar = get_archive(&arch, None).ok()??;
        let test = fix_filepath(filename, Some(&cwd));
        let a = phar.borrow();
        if test.first() == Some(&b'/') {
            if a.manifest.contains_key(&test[1..]) {
                let mut r = b"phar://".to_vec();
                r.extend_from_slice(&arch);
                r.extend_from_slice(&test);
                return Some(r);
            }
        } else if a.manifest.contains_key(&test) {
            let mut r = b"phar://".to_vec();
            r.extend_from_slice(&arch);
            r.push(b'/');
            r.extend_from_slice(&test);
            return Some(r);
        }
    }
    let mut path = b"phar://".to_vec();
    path.extend_from_slice(&arch);
    path.push(b'/');
    path.extend_from_slice(&cwd);
    path.push(b':');
    path.extend_from_slice(include_path);
    php_streams::paths::resolve_path(filename, &path)
}

// ---------------------------------------------------------- map helpers

/// Removes an alias mapping.
pub fn alias_map_remove(alias: &[u8]) {
    with(|s| {
        s.alias_map.remove(alias);
    });
}

/// `zend_hash_add_ptr(alias_map)`; `false` when the alias exists.
pub fn alias_map_add(alias: &[u8], ar: &ArchiveRef) -> bool {
    with(|s| s.alias_map.add(alias.to_vec(), Rc::clone(ar)))
}

/// `zend_hash_update_ptr(alias_map)`.
pub fn alias_map_update(alias: &[u8], ar: &ArchiveRef) {
    with(|s| s.alias_map.update(alias.to_vec(), Rc::clone(ar)));
}

/// `zend_hash_update_ptr(fname_map)`.
pub fn fname_map_update(fname: &[u8], ar: &ArchiveRef) {
    with(|s| s.fname_map.update(fname.to_vec(), Rc::clone(ar)));
}

/// `zend_hash_del(fname_map)` (with alias cleanup).
pub fn fname_map_remove(fname: &[u8]) -> bool {
    fname_map_del(fname)
}

/// Whether `fname` is in the `phar.cache_list` cache.
pub fn is_cached(fname: &[u8]) -> bool {
    with(|s| s.manifest_cached && s.cached_phars.contains_key(fname))
}

/// The cached (persistent) archive for `fname`.
pub fn cached(fname: &[u8]) -> Option<ArchiveRef> {
    with(|s| {
        if s.manifest_cached {
            s.cached_phars.get(fname).cloned()
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_semantics() {
        assert!(set_ini("phar.readonly", true, true));
        assert!(!set_ini("phar.readonly", false, false));
        assert!(readonly());
        assert!(set_ini("phar.readonly", false, true));
        assert!(set_ini("phar.readonly", true, false));
        assert!(set_ini("phar.readonly", false, false));
        assert!(!readonly());
        set_ini("phar.readonly", true, true);
    }

    #[test]
    fn detect_extensions() {
        let dir = std::env::temp_dir();
        let mut p = dir.to_string_lossy().as_bytes().to_vec();
        p.extend_from_slice(b"/phar_detect_test_nonexistent.phar");
        assert!(detect_fname_ext(&p, 1, 1, true).is_ok());
        assert!(detect_fname_ext(&p, 1, 0, true).is_err());
        let mut t = dir.to_string_lossy().as_bytes().to_vec();
        t.extend_from_slice(b"/x.tar");
        assert!(detect_fname_ext(&t, 0, 1, true).is_ok());
        assert!(detect_fname_ext(&t, 1, 1, true).is_err());
        assert_eq!(
            detect_fname_ext(b"http://x/y.phar", 1, 1, true),
            Err(ExtFail::Url)
        );
        assert_eq!(
            detect_fname_ext(b"nodot", 1, 1, true),
            Err(ExtFail::NotFound(false))
        );
    }
}
