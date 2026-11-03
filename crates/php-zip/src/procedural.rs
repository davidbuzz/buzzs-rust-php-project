//! The deprecated procedural API: `zip_open()`, `zip_read()`,
//! `zip_close()` and `zip_entry_*()` (resources "Zip Directory" and
//! "Zip Entry"). The VM reports the `#[\Deprecated]` notices and the
//! resource type errors ("supplied resource is not a valid Zip Directory
//! resource").

use std::cell::Cell;
use std::rc::{Rc, Weak};

use php_runtime::RtResult;

use crate::archive::{copy_cstr, cstr};
use crate::error::{must_not_be_empty, warning};
use crate::ffi::{self, zip_t};
use crate::stream::ZipFile;

/// The `le_zip_dir` resource type name.
pub const DIR_RESOURCE_NAME: &str = "Zip Directory";
/// The `le_zip_entry` resource type name.
pub const ENTRY_RESOURCE_NAME: &str = "Zip Entry";

/// `zip_rsrc`: an archive opened with `zip_open()`.
#[derive(Debug)]
pub struct ZipDir {
    inner: Rc<DirInner>,
}

#[derive(Debug)]
struct DirInner {
    za: Cell<*mut zip_t>,
    index_current: Cell<u64>,
    num_files: i64,
}

impl Drop for DirInner {
    fn drop(&mut self) {
        close_dir(&self.za);
    }
}

/// `php_zip_free_dir()`.
fn close_dir(za: &Cell<*mut zip_t>) {
    let p = za.replace(std::ptr::null_mut());
    if !p.is_null() {
        // SAFETY: the archive is open and closed exactly once.
        if unsafe { ffi::zip_close(p) } != 0 {
            warning("Cannot destroy the zip context");
            // SAFETY: a failed zip_close() leaves the archive to discard.
            unsafe { ffi::zip_discard(p) };
        }
    }
}

/// The result of `zip_open()`.
#[derive(Debug)]
pub enum ZipOpenResult {
    /// The resource.
    Dir(ZipDir),
    /// `false`.
    False,
    /// The libzip error code (`int`).
    Error(i64),
}

/// `zip_open(string $filename)`.
pub fn zip_open(filename: &[u8]) -> RtResult<ZipOpenResult> {
    if filename.is_empty() {
        return Err(must_not_be_empty("zip_open", 1, "filename"));
    }
    if !php_streams::paths::check_open_basedir(filename) {
        return Ok(ZipOpenResult::False);
    }
    let Some(resolved) = php_streams::paths::expand_filepath(filename) else {
        warning("No such file or directory");
        return Ok(ZipOpenResult::False);
    };
    let c = cstr(&resolved);
    let mut err = 0;
    // SAFETY: C string path and a valid error output.
    let za = unsafe { ffi::zip_open(c.as_ptr(), 0, &raw mut err) };
    if za.is_null() {
        return Ok(ZipOpenResult::Error(i64::from(err)));
    }
    // SAFETY: open archive.
    let num_files = unsafe { ffi::zip_get_num_entries(za, 0) };
    Ok(ZipOpenResult::Dir(ZipDir {
        inner: Rc::new(DirInner {
            za: Cell::new(za),
            index_current: Cell::new(0),
            num_files,
        }),
    }))
}

impl ZipDir {
    /// `zip_close()`: closes the archive (the resource becomes invalid).
    pub fn close(&self) {
        close_dir(&self.inner.za);
    }

    /// Whether `zip_close()` was called.
    pub fn is_closed(&self) -> bool {
        self.inner.za.get().is_null()
    }

    /// `zip_read()`: the next entry, `None` for `false`.
    #[allow(clippy::cast_sign_loss)]
    pub fn read(&self) -> Option<ZipEntry> {
        let za = self.inner.za.get();
        if za.is_null() {
            return None;
        }
        let cur = self.inner.index_current.get();
        if cur as i64 >= self.inner.num_files {
            return None;
        }
        let mut sb = std::mem::MaybeUninit::<ffi::zip_stat_t>::zeroed();
        // SAFETY: open archive and zeroed output struct.
        if unsafe { ffi::zip_stat_index(za, cur, 0, sb.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: zip_stat_index() succeeded.
        let sb = unsafe { sb.assume_init() };
        // SAFETY: open archive.
        let zf = unsafe { ffi::zip_fopen_index(za, cur, 0) };
        if zf.is_null() {
            return None;
        }
        self.inner.index_current.set(cur + 1);
        Some(ZipEntry {
            zf: Some(ZipFile(zf)),
            // SAFETY: libzip's name is a valid C string (or NULL).
            name: unsafe { copy_cstr(sb.name) }.unwrap_or_default(),
            comp_size: sb.comp_size,
            size: sb.size,
            comp_method: sb.comp_method,
            dir: Rc::downgrade(&self.inner),
        })
    }
}

/// `zip_read_rsrc`: an entry returned by `zip_read()`.
#[derive(Debug)]
pub struct ZipEntry {
    zf: Option<ZipFile>,
    name: Vec<u8>,
    comp_size: u64,
    size: u64,
    comp_method: u16,
    dir: Weak<DirInner>,
}

impl ZipEntry {
    /// `zip_entry_open()`: whether the entry is readable.
    pub fn open(&self) -> bool {
        self.zf.is_some()
    }

    /// `zip_entry_close()`: frees the entry (the resource becomes invalid).
    pub fn close(&mut self) {
        self.zf.take();
    }

    /// `zip_entry_read(int $len = 1024)`: `None` for `false`.
    pub fn read(&self, len: i64) -> Option<Vec<u8>> {
        let len = if len <= 0 { 1024 } else { len };
        let zf = self.zf.as_ref()?;
        let mut buf = vec![0u8; usize::try_from(len).unwrap_or(usize::MAX).min(1 << 30)];
        match zf.read(&mut buf) {
            Some(n) if n > 0 => {
                buf.truncate(n);
                Some(buf)
            }
            _ => Some(Vec::new()),
        }
    }

    fn info_ok(&self) -> bool {
        self.zf.is_some() && self.dir.strong_count() > 0
    }

    /// `zip_entry_name()`.
    pub fn name(&self) -> Option<Vec<u8>> {
        self.info_ok().then(|| self.name.clone())
    }

    /// `zip_entry_compressedsize()`.
    #[allow(clippy::cast_possible_wrap)]
    pub fn compressed_size(&self) -> Option<i64> {
        self.info_ok().then_some(self.comp_size as i64)
    }

    /// `zip_entry_filesize()`.
    #[allow(clippy::cast_possible_wrap)]
    pub fn filesize(&self) -> Option<i64> {
        self.info_ok().then_some(self.size as i64)
    }

    /// `zip_entry_compressionmethod()`.
    pub fn compression_method(&self) -> Option<&'static str> {
        if !self.info_ok() {
            return None;
        }
        Some(match self.comp_method {
            0 => "stored",
            1 => "shrunk",
            2..=5 => "reduced",
            6 => "imploded",
            7 => "tokenized",
            8 => "deflated",
            9 => "deflatedX",
            10 => "implodedX",
            _ => return None,
        })
    }
}
