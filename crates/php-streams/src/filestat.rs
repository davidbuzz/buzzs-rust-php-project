//! The stat family and file metadata functions (`ext/standard/filestat.c`).
//!
//! `stat()`/`lstat()` results are cached per path exactly like PHP's
//! `BG(CurrentStatFile)` / `BG(CurrentLStatFile)`; any write or metadata
//! operation clears the cache ([`crate::state::clear_stat_cache`]).

use crate::diag;
use crate::paths;
use crate::stat::{mode, StatBuf};
use crate::state;
use crate::sys;
use crate::util::{os_path, starts_with_ci};
use crate::value::Value;
use crate::wrapper::{
    self, MetadataOp, URL_STAT_IGNORE_OPEN_BASEDIR, URL_STAT_LINK, URL_STAT_QUIET,
};

/// The `php_stat()` operation (`FS_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsType {
    /// `fileperms()`.
    Perms,
    /// `fileinode()`.
    Inode,
    /// `filesize()`.
    Size,
    /// `fileowner()`.
    Owner,
    /// `filegroup()`.
    Group,
    /// `fileatime()`.
    Atime,
    /// `filemtime()`.
    Mtime,
    /// `filectime()`.
    Ctime,
    /// `filetype()`.
    Type,
    /// `is_writable()`.
    IsW,
    /// `is_readable()`.
    IsR,
    /// `is_executable()`.
    IsX,
    /// `is_file()`.
    IsFile,
    /// `is_dir()`.
    IsDir,
    /// `is_link()`.
    IsLink,
    /// `file_exists()`.
    Exists,
    /// `lstat()`.
    Lstat,
    /// `stat()`.
    Stat,
    /// `lfileperms` (internal: perms without following links).
    Lperms,
}

impl FsType {
    fn is_link_op(self) -> bool {
        matches!(
            self,
            FsType::Type | FsType::IsLink | FsType::Lstat | FsType::Lperms
        )
    }

    fn is_exists_check(self) -> bool {
        matches!(
            self,
            FsType::Exists
                | FsType::IsW
                | FsType::IsR
                | FsType::IsX
                | FsType::IsFile
                | FsType::IsDir
                | FsType::IsLink
                | FsType::Lperms
        )
    }

    fn is_able_check(self) -> bool {
        matches!(self, FsType::IsR | FsType::IsW | FsType::IsX)
    }
}

/// The result of [`php_stat`].
#[derive(Debug, Clone, PartialEq)]
pub enum StatResult {
    /// A boolean.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A string (`filetype()`).
    Str(&'static str),
    /// The full stat buffer (`stat()`/`lstat()`).
    Stat(StatBuf),
}

impl StatResult {
    /// Converts to a [`Value`].
    pub fn to_value(&self) -> Value {
        match self {
            StatResult::Bool(b) => Value::Bool(*b),
            StatResult::Int(i) => Value::Int(*i),
            StatResult::Str(s) => Value::str(*s),
            StatResult::Stat(sb) => sb.to_value(),
        }
    }
}

fn getuid() -> i64 {
    i64::from(sys::uid())
}

/// `php_stat()`.
#[allow(clippy::too_many_lines)]
pub fn php_stat(filename: &[u8], ty: FsType) -> StatResult {
    if filename.is_empty() {
        return StatResult::Bool(false);
    }
    let mut located = None;
    if ty.is_able_check() || ty == FsType::Exists {
        located = wrapper::locate(filename, 0);
        if let Some(loc) = &located {
            if loc.wrapper.is_plain_files() {
                if !paths::check_open_basedir(&loc.path) {
                    return StatResult::Bool(false);
                }
                let mut check = loc.path.clone();
                if wrapper::scheme_len(&check) > 0 {
                    if let Some(p) = paths::expand_filepath(&loc.path) {
                        if crate::util::find_bytes(&loc.path, b"://").is_some() {
                            check = p;
                        }
                    }
                }
                let m = match ty {
                    FsType::Exists => sys::F_OK,
                    FsType::IsW => sys::W_OK,
                    FsType::IsR => sys::R_OK,
                    _ => sys::X_OK,
                };
                return StatResult::Bool(sys::access(&check, m).is_ok());
            }
        }
    }
    let link = ty.is_link_op();
    let mut flags = 0;
    if link {
        flags |= URL_STAT_LINK;
    }
    if ty.is_exists_check() {
        flags |= URL_STAT_QUIET;
    }
    let sb = if let Some(sb) = state::stat_cache_get(filename, link) {
        sb
    } else {
        if located.is_none() {
            located = wrapper::locate(filename, 0);
            if let Some(loc) = &located {
                if loc.wrapper.is_plain_files() && !paths::check_open_basedir(&loc.path) {
                    return StatResult::Bool(false);
                }
            }
        }
        let r = located.as_ref().and_then(|loc| {
            loc.wrapper
                .url_stat(&loc.path, flags | URL_STAT_IGNORE_OPEN_BASEDIR, None)
        });
        let Some(Ok(sb)) = r else {
            if !ty.is_exists_check() {
                diag::warning(diag::msg(&[
                    if link {
                        b"Lstat failed for "
                    } else {
                        b"stat failed for "
                    },
                    filename,
                ]));
            }
            return StatResult::Bool(false);
        };
        if link {
            state::stat_cache_put(filename, true, sb);
        }
        if !link || !sb.is_link() {
            state::stat_cache_put(filename, false, sb);
        }
        sb
    };
    let is_plain = located.as_ref().is_some_and(|l| l.wrapper.is_plain_files());
    let (mut rmask, mut wmask, mut xmask) = (0o004, 0o002, 0o001);
    if matches!(ty, FsType::IsW | FsType::IsR | FsType::IsX) {
        if sb.uid == getuid() {
            (rmask, wmask, xmask) = (0o400, 0o200, 0o100);
        } else if sb.gid == i64::from(sys::gid()) || sys::in_group(sb.gid) {
            (rmask, wmask, xmask) = (0o040, 0o020, 0o010);
        }
    }
    if ty.is_able_check() && getuid() == 0 && is_plain {
        if ty == FsType::IsX {
            xmask = 0o111;
        } else {
            return StatResult::Bool(true);
        }
    }
    match ty {
        FsType::Perms | FsType::Lperms => StatResult::Int(sb.mode),
        FsType::Inode => StatResult::Int(sb.ino),
        FsType::Size => StatResult::Int(sb.size),
        FsType::Owner => StatResult::Int(sb.uid),
        FsType::Group => StatResult::Int(sb.gid),
        FsType::Atime => StatResult::Int(sb.atime),
        FsType::Mtime => StatResult::Int(sb.mtime),
        FsType::Ctime => StatResult::Int(sb.ctime),
        FsType::Type => match sb.type_name() {
            Some(t) => StatResult::Str(t),
            None => {
                diag::notice(format!("Unknown file type ({})", sb.mode & mode::S_IFMT));
                StatResult::Str("unknown")
            }
        },
        FsType::IsW => StatResult::Bool(sb.mode & wmask != 0),
        FsType::IsR => StatResult::Bool(sb.mode & rmask != 0),
        FsType::IsX => StatResult::Bool(sb.mode & xmask != 0),
        FsType::IsFile => StatResult::Bool(sb.is_file()),
        FsType::IsDir => StatResult::Bool(sb.is_dir()),
        FsType::IsLink => StatResult::Bool(sb.is_link()),
        FsType::Exists => StatResult::Bool(true),
        FsType::Lstat | FsType::Stat => StatResult::Stat(sb),
    }
}

/// `clearstatcache()`. The realpath cache is not implemented (no cache is
/// kept), so `clear_realpath_cache` has nothing else to clear.
pub fn clearstatcache(_clear_realpath_cache: bool, _filename: Option<&[u8]>) {
    state::clear_stat_cache();
}

fn non_plain(filename: &[u8]) -> Option<std::rc::Rc<dyn wrapper::Wrapper>> {
    let loc = wrapper::locate(filename, 0);
    match loc {
        Some(l) if l.wrapper.is_plain_files() && !starts_with_ci(filename, b"file://") => None,
        Some(l) => Some(l.wrapper),
        // No wrapper at all: treated as non-standard without metadata.
        None => Some(std::rc::Rc::new(NoMeta) as std::rc::Rc<dyn wrapper::Wrapper>),
    }
}

#[derive(Debug)]
struct NoMeta;

impl wrapper::Wrapper for NoMeta {
    fn label(&self) -> &'static str {
        ":na"
    }
    fn open(
        &self,
        _path: &[u8],
        _mode: &str,
        _options: u32,
        _opened_path: &mut Option<Vec<u8>>,
        _context: Option<&crate::context::ContextRef>,
    ) -> Result<crate::stream::Stream, wrapper::OpenError> {
        Err(wrapper::OpenError::silent())
    }
}

fn via_wrapper(w: &dyn wrapper::Wrapper, filename: &[u8], op: &MetadataOp, fname: &str) -> bool {
    match w.metadata(filename, op, None) {
        Some(r) => r,
        None => {
            diag::warning(format!("Cannot call {fname}() for a non-standard stream"));
            false
        }
    }
}

/// `chmod()`.
pub fn chmod(filename: &[u8], mode: i64) -> bool {
    if let Some(w) = non_plain(filename) {
        return via_wrapper(&*w, filename, &MetadataOp::Access(mode), "chmod");
    }
    if !paths::check_open_basedir(filename) {
        return false;
    }
    use std::os::unix::fs::PermissionsExt;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let perms = std::fs::Permissions::from_mode((mode as u32) & 0o7777);
    match std::fs::set_permissions(os_path(filename), perms) {
        Ok(()) => {
            state::clear_stat_cache();
            true
        }
        Err(e) => {
            diag::warning(sys::error_text(&e));
            false
        }
    }
}

/// A user or group given by name or id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// By name.
    Name(Vec<u8>),
    /// By numeric id.
    Id(i64),
}

fn do_chown(filename: &[u8], who: &Principal, group: bool, follow: bool) -> bool {
    let fname = match (group, follow) {
        (true, _) => "chgrp",
        (false, _) => "chown",
    };
    if let Some(w) = non_plain(filename) {
        let op = match (group, who) {
            (true, Principal::Name(n)) => MetadataOp::GroupName(n.clone()),
            (true, Principal::Id(i)) => MetadataOp::Group(*i),
            (false, Principal::Name(n)) => MetadataOp::OwnerName(n.clone()),
            (false, Principal::Id(i)) => MetadataOp::Owner(*i),
        };
        return via_wrapper(&*w, filename, &op, fname);
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let id = match who {
        Principal::Name(n) => {
            let r = if group {
                sys::gid_by_name(n)
            } else {
                sys::uid_by_name(n)
            };
            match r {
                Some(i) => i,
                None => {
                    let what = if group { "gid" } else { "uid" };
                    diag::warning(diag::msg(&[
                        format!("Unable to find {what} for ").as_bytes(),
                        n,
                    ]));
                    return false;
                }
            }
        }
        Principal::Id(i) => *i as u32,
    };
    if !paths::check_open_basedir(filename) {
        return false;
    }
    let r = if group {
        sys::chown(filename, None, Some(id), follow)
    } else {
        sys::chown(filename, Some(id), None, follow)
    };
    match r {
        Ok(()) => {
            state::clear_stat_cache();
            true
        }
        Err(e) => {
            diag::warning(sys::error_text(&e));
            false
        }
    }
}

/// `chown()`.
pub fn chown(filename: &[u8], user: &Principal) -> bool {
    do_chown(filename, user, false, true)
}

/// `lchown()`.
pub fn lchown(filename: &[u8], user: &Principal) -> bool {
    do_chown(filename, user, false, false)
}

/// `chgrp()`.
pub fn chgrp(filename: &[u8], group: &Principal) -> bool {
    do_chown(filename, group, true, true)
}

/// `lchgrp()`.
pub fn lchgrp(filename: &[u8], group: &Principal) -> bool {
    do_chown(filename, group, true, false)
}

/// `touch($filename, $mtime, $atime)`. `Err` is the `ValueError` for a
/// null `$mtime` with an integer `$atime`.
pub fn touch(
    filename: &[u8],
    mtime: Option<i64>,
    atime: Option<i64>,
) -> Result<bool, crate::file::ArgError> {
    if filename.is_empty() {
        return Ok(false);
    }
    let times = match (mtime, atime) {
        (None, None) => None,
        (Some(m), None) => Some((m, m)),
        (None, Some(_)) => {
            return Err(crate::file::ArgError {
                arg: 2,
                message: "($mtime) cannot be null when argument #3 ($atime) is an integer".into(),
            })
        }
        (Some(m), Some(a)) => Some((m, a)),
    };
    if let Some(w) = non_plain(filename) {
        if let Some(r) = w.metadata(filename, &MetadataOp::Touch(times), None) {
            return Ok(r);
        }
        if times.is_some() {
            diag::warning("Cannot call touch() for a non-standard stream");
            return Ok(false);
        }
        let mut opened = None;
        return Ok(
            match crate::open::open_wrapper(
                filename,
                "c",
                wrapper::REPORT_ERRORS,
                &mut opened,
                None,
            ) {
                Some(mut s) => {
                    s.close();
                    true
                }
                None => false,
            },
        );
    }
    if !paths::check_open_basedir(filename) {
        return Ok(false);
    }
    if sys::access(filename, sys::F_OK).is_err() {
        if let Err(e) = std::fs::File::create(os_path(filename)) {
            diag::warning(diag::msg(&[
                b"Unable to create file ",
                filename,
                b" because ",
                sys::error_text(&e).as_bytes(),
            ]));
            return Ok(false);
        }
    }
    if let Err(e) = sys::utime(filename, times.map(|(m, a)| (a, m))) {
        diag::warning(format!("Utime failed: {}", sys::error_text(&e)));
        return Ok(false);
    }
    state::clear_stat_cache();
    Ok(true)
}

/// `disk_free_space()` / `disk_total_space()`.
pub fn disk_space(directory: &[u8], total: bool) -> Option<f64> {
    if !paths::check_open_basedir(directory) {
        return None;
    }
    match sys::disk_space(directory) {
        Ok((free, tot)) => Some(if total { tot } else { free }),
        Err(e) => {
            diag::warning(sys::error_text(&e));
            None
        }
    }
}

#[cfg(test)]
mod tests;
