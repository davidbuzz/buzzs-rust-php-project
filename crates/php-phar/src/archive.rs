//! The in-memory archive model (`phar_archive_data` / `phar_entry_info`)
//! and entry content access (`phar_open_entry_fp()`, `phar_get_efp()`,
//! `phar_postprocess_file()`).
//!
//! The archive file itself is held as an immutable byte image
//! ([`Archive::fp`], decompressed for whole-archive gz/bz2 compression);
//! entries either point into it ([`FpType::Fp`]) or carry their own
//! uncompressed bytes ([`FpType::Ufp`] for a decompressed cache,
//! [`FpType::Mod`] for modified content) or refer to an external file
//! ([`FpType::Tmp`], mounted paths).

use std::cell::RefCell;
use std::rc::Rc;

use php_runtime::Value;

use crate::consts::{
    ENT_COMPRESSED_BZ2, ENT_COMPRESSED_GZ, ENT_COMPRESSION_MASK, ENT_PERM_DEF_FILE,
};
use crate::omap::OrderedMap;
use crate::util::{get16, get32, parent_dirs, s};

/// A shared archive handle.
pub type ArchiveRef = Rc<RefCell<Archive>>;

/// Shared mutable entry content.
pub type Buf = Rc<RefCell<Vec<u8>>>;

/// The metadata of an archive or entry (`phar_metadata_tracker`): the
/// serialized string and/or the value it was set from.
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    /// The serialized form (`tracker->str`).
    pub serialized: Option<Vec<u8>>,
    /// The PHP value (`tracker->val`), when set from PHP code.
    pub value: Option<Value>,
}

impl Metadata {
    /// Metadata read from a file.
    pub fn from_serialized(data: &[u8]) -> Self {
        Metadata {
            serialized: (!data.is_empty()).then(|| data.to_vec()),
            value: None,
        }
    }

    /// `phar_metadata_tracker_has_data()`.
    pub fn has_data(&self) -> bool {
        self.serialized.is_some() || self.value.is_some()
    }

    /// `phar_metadata_tracker_free()`.
    pub fn clear(&mut self) {
        self.serialized = None;
        self.value = None;
    }

    /// The serialized bytes (empty when there is none).
    pub fn bytes(&self) -> &[u8] {
        self.serialized.as_deref().unwrap_or_default()
    }
}

/// `enum phar_fp_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FpType {
    /// In the archive image at `offset`.
    #[default]
    Fp,
    /// Decompressed copy in `data`.
    Ufp,
    /// Modified / new content in `data`.
    Mod,
    /// External file `tmp` (mounted).
    Tmp,
}

/// One manifest entry (`phar_entry_info`).
#[derive(Debug, Clone, Default)]
#[allow(clippy::struct_excessive_bools)] // mirrors the C bit-fields
pub struct Entry {
    /// The path inside the archive (no leading or trailing `/`).
    pub filename: Vec<u8>,
    /// Uncompressed size.
    pub uncompressed_filesize: u32,
    /// Modification time.
    pub timestamp: u32,
    /// Stored size.
    pub compressed_filesize: u32,
    /// CRC32 of the uncompressed data (the tar header checksum for tars).
    pub crc32: u32,
    /// Permissions and compression bits.
    pub flags: u32,
    /// Flags before a compression change.
    pub old_flags: u32,
    /// Per-file metadata.
    pub metadata: Metadata,
    /// Offset of the content within its fp.
    pub offset: u64,
    /// Offset of the content within the archive file.
    pub offset_abs: u64,
    /// Offset of the zip local header.
    pub header_offset: u64,
    /// Where the content lives.
    pub fp_type: FpType,
    /// Content for [`FpType::Ufp`] / [`FpType::Mod`].
    pub data: Option<Buf>,
    /// The external path of a mounted entry.
    pub tmp: Option<Vec<u8>>,
    /// Link target (tar links).
    pub symlink: Option<Vec<u8>>,
    /// Tar type flag.
    pub tar_type: u8,
    /// Fake inode number.
    pub inode: u16,
    /// Open stream count.
    pub fp_refcount: i32,
    /// Live `PharFileInfo` objects.
    pub fileinfo_lock_count: u32,
    /// CRC has been verified.
    pub is_crc_checked: bool,
    /// Content changed since the last flush.
    pub is_modified: bool,
    /// Deleted but still referenced.
    pub is_deleted: bool,
    /// Directory entry.
    pub is_dir: bool,
    /// Mounted external path.
    pub is_mounted: bool,
    /// Synthesised directory (not in the manifest).
    pub is_temp_dir: bool,
    /// Belongs to a tar archive.
    pub is_tar: bool,
    /// Belongs to a zip archive.
    pub is_zip: bool,
    /// From `phar.cache_list`.
    pub is_persistent: bool,
}

impl Entry {
    /// A fresh modified file entry with empty content.
    pub fn new_file(filename: Vec<u8>, timestamp: u32) -> Self {
        Entry {
            filename,
            timestamp,
            flags: ENT_PERM_DEF_FILE,
            old_flags: ENT_PERM_DEF_FILE,
            fp_type: FpType::Mod,
            data: Some(Rc::new(RefCell::new(Vec::new()))),
            is_modified: true,
            is_crc_checked: true,
            ..Entry::default()
        }
    }

    /// The compression bits.
    pub fn compression(&self) -> u32 {
        self.flags & ENT_COMPRESSION_MASK
    }

    /// Replaces the content with `bytes` (as `PHAR_MOD`).
    pub fn set_content(&mut self, bytes: Vec<u8>) {
        let len = crate::util::to_u32(bytes.len());
        self.data = Some(Rc::new(RefCell::new(bytes)));
        self.fp_type = FpType::Mod;
        self.offset = 0;
        self.uncompressed_filesize = len;
        self.compressed_filesize = len;
        self.is_modified = true;
    }
}

/// An open archive (`phar_archive_data`).
#[derive(Debug, Default, Clone)]
#[allow(clippy::struct_excessive_bools)] // mirrors the C bit-fields
pub struct Archive {
    /// Absolute path of the archive file.
    pub fname: Vec<u8>,
    /// Length of the file extension at the end of `fname`.
    pub ext_len: usize,
    /// The alias (`None` for `PharData`; the file name when an executable
    /// archive has no explicit alias).
    pub alias: Option<Vec<u8>>,
    /// Whether the alias is temporary (not stored in the archive).
    pub is_temporary_alias: bool,
    /// Manifest API version ("1.1.1").
    pub version: String,
    /// Length of the stub (offset of the manifest).
    pub halt_offset: u64,
    /// The entries, in archive order.
    pub manifest: OrderedMap<Entry>,
    /// Implicit directories.
    pub virtual_dirs: OrderedMap<()>,
    /// Mounted directories.
    pub mounted_dirs: OrderedMap<()>,
    /// Global flags (signature, compression).
    pub flags: u32,
    /// Newest entry timestamp.
    pub max_timestamp: u32,
    /// References from objects and streams.
    pub refcount: i32,
    /// The archive image (decompressed for gz/bz2 archives).
    pub fp: Option<Rc<Vec<u8>>>,
    /// Signature type.
    pub sig_flags: u32,
    /// Upper-case hex signature.
    pub signature: Option<Vec<u8>>,
    /// Archive metadata.
    pub metadata: Metadata,
    /// Has unsaved changes.
    pub is_modified: bool,
    /// Writable.
    pub is_writeable: bool,
    /// Not yet written to disk.
    pub is_brandnew: bool,
    /// Buffering (`startBuffering()`).
    pub donotflush: bool,
    /// Zip-based.
    pub is_zip: bool,
    /// Tar-based.
    pub is_tar: bool,
    /// A `PharData` (non-executable) archive.
    pub is_data: bool,
    /// From `phar.cache_list`.
    pub is_persistent: bool,
}

impl Archive {
    /// A brand new archive.
    pub fn new(fname: Vec<u8>) -> Self {
        let ext_len = crate::util::archive_ext(&fname).map_or(0, <[u8]>::len);
        Archive {
            alias: Some(fname.clone()),
            fname,
            ext_len,
            version: crate::consts::API_VERSION_STR.to_string(),
            is_temporary_alias: true,
            is_writeable: true,
            is_brandnew: true,
            ..Archive::default()
        }
    }

    /// Wraps the archive in a shared handle.
    pub fn into_ref(self) -> ArchiveRef {
        Rc::new(RefCell::new(self))
    }

    /// Recomputes `ext_len` from `fname`.
    pub fn set_ext_from_fname(&mut self) {
        self.ext_len = crate::util::archive_ext(&self.fname).map_or(0, <[u8]>::len);
    }

    /// `alias_len`.
    pub fn alias_len(&self) -> usize {
        self.alias.as_ref().map_or(0, Vec::len)
    }

    /// The alias bytes (empty when unset).
    pub fn alias_bytes(&self) -> &[u8] {
        self.alias.as_deref().unwrap_or_default()
    }

    /// The extension (`phar->ext`).
    pub fn ext(&self) -> &[u8] {
        &self.fname[self.fname.len() - self.ext_len..]
    }

    /// `phar_add_virtual_dirs()`.
    pub fn add_virtual_dirs(&mut self, filename: &[u8]) {
        for d in parent_dirs(filename) {
            if !self.virtual_dirs.add(d.to_vec(), ()) {
                break;
            }
        }
    }

    /// Rebuilds the virtual directory list from the live entries.
    pub fn rebuild_virtual_dirs(&mut self) {
        self.virtual_dirs.clear();
        let names: Vec<Vec<u8>> = self
            .manifest
            .iter()
            .filter(|(_, e)| !e.is_deleted)
            .map(|(k, _)| k.clone())
            .collect();
        for n in names {
            self.add_virtual_dirs(&n);
        }
    }

    /// Number of live manifest entries (`count()`).
    pub fn count(&self) -> usize {
        self.manifest.len()
    }

    /// Loads the image from disk when it is not open (`phar_open_archive_fp`).
    pub fn open_archive_fp(&mut self) -> Option<Rc<Vec<u8>>> {
        if let Some(fp) = &self.fp {
            return Some(Rc::clone(fp));
        }
        if !php_streams::paths::check_open_basedir(&self.fname) {
            return None;
        }
        let data = Rc::new(crate::sys::read_file(&self.fname).ok()?);
        self.fp = Some(Rc::clone(&data));
        Some(data)
    }

    /// Follows tar links (`phar_get_link_source()`); `None` for cycles or
    /// dangling links.
    pub fn link_source(&self, key: &[u8]) -> Option<Vec<u8>> {
        let follow = |k: &[u8]| -> Option<Vec<u8>> {
            let e = self.manifest.get(k)?;
            let link = e.symlink.as_ref()?;
            let location = if let Some(rest) = link.strip_prefix(b"/") {
                rest.to_vec()
            } else if let Some(p) = e.filename.iter().rposition(|&c| c == b'/') {
                let mut l = e.filename[..p].to_vec();
                l.push(b'/');
                l.extend_from_slice(link);
                l
            } else {
                link.clone()
            };
            if self.manifest.contains_key(link) {
                Some(link.clone())
            } else if self.manifest.contains_key(&location) {
                Some(location)
            } else {
                None
            }
        };
        let has_link = |k: &[u8]| self.manifest.get(k).is_some_and(|e| e.symlink.is_some());
        if !has_link(key) {
            return Some(key.to_vec());
        }
        let mut slow = key.to_vec();
        let mut fast = key.to_vec();
        loop {
            fast = follow(&fast)?;
            if !has_link(&fast) {
                return Some(fast);
            }
            fast = follow(&fast)?;
            if !has_link(&fast) {
                return Some(fast);
            }
            slow = follow(&slow)?;
            if slow == fast {
                return None;
            }
        }
    }

    /// `phar_postprocess_file()` zip part: verifies the local header against
    /// the central directory and fixes the content offset.
    fn verify_zip_local(&mut self, key: &[u8]) -> Result<(), String> {
        let fname = s(&self.fname);
        let Some(img) = self.open_archive_fp() else {
            let name = self
                .manifest
                .get(key)
                .map(|e| s(&e.filename))
                .unwrap_or_default();
            return Err(format!(
                "phar error: unable to open zip-based phar archive \"{fname}\" to verify local file header for file \"{name}\""
            ));
        };
        let Some(e) = self.manifest.get_mut(key) else {
            return Ok(());
        };
        let name = s(&e.filename);
        let ho = usize::try_from(e.header_offset).unwrap_or(usize::MAX);
        let Some(local) = img.get(ho..ho.saturating_add(30)) else {
            return Err(format!(
                "phar error: internal corruption of zip-based phar \"{fname}\" (cannot read local file header for file \"{name}\")"
            ));
        };
        let mut crc = get32(local, 14);
        let mut comp = get32(local, 18);
        let mut uncomp = get32(local, 22);
        let fnlen = usize::from(get16(local, 26));
        let extra = usize::from(get16(local, 28));
        if get16(local, 6) & 0x8 == 0x8 {
            let at = ho + 30 + fnlen + extra + e.compressed_filesize as usize;
            let Some(desc) = img.get(at..at + 16) else {
                return Err(format!(
                    "phar error: internal corruption of zip-based phar \"{fname}\" (cannot read local data descriptor for file \"{name}\")"
                ));
            };
            let base = if &desc[..2] == b"PK" { 4 } else { 0 };
            crc = get32(desc, base);
            comp = get32(desc, base + 4);
            uncomp = get32(desc, base + 8);
        }
        if e.filename.len() != fnlen
            || e.crc32 != crc
            || e.uncompressed_filesize != uncomp
            || e.compressed_filesize != comp
        {
            return Err(format!(
                "phar error: internal corruption of zip-based phar \"{fname}\" (local header of file \"{name}\" does not match central directory)"
            ));
        }
        let off = (30 + ho + fnlen + extra) as u64;
        e.offset = off;
        e.offset_abs = off;
        Ok(())
    }

    /// The uncompressed bytes of `key` as currently available, without
    /// decompressing (`phar_get_efp` + read of `uncompressed_filesize`).
    fn raw_content(&mut self, key: &[u8]) -> Option<Vec<u8>> {
        let (fp_type, offset, len, data, tmp) = {
            let e = self.manifest.get(key)?;
            (
                e.fp_type,
                e.offset,
                e.uncompressed_filesize,
                e.data.clone(),
                e.tmp.clone(),
            )
        };
        match fp_type {
            FpType::Ufp | FpType::Mod => {
                let d = data?;
                let b = d.borrow();
                let start = usize::try_from(offset).unwrap_or(0).min(b.len());
                let end = start.saturating_add(len as usize).min(b.len());
                Some(b[start..end].to_vec())
            }
            FpType::Tmp => crate::sys::read_file(&tmp?).ok(),
            FpType::Fp => {
                let img = self.open_archive_fp()?;
                let start = usize::try_from(offset).ok()?;
                let end = start.checked_add(len as usize)?;
                Some(img.get(start..end.min(img.len()))?.to_vec())
            }
        }
    }

    /// `phar_open_entry_fp()`: makes the uncompressed content of `key`
    /// available (decompressing into a cache when needed).
    pub fn open_entry_fp(&mut self, key: &[u8], follow_links: bool) -> Result<(), String> {
        let key = if follow_links {
            self.link_source(key).unwrap_or_else(|| key.to_vec())
        } else {
            key.to_vec()
        };
        let fname = s(&self.fname);
        let (is_modified, fp_type, flags, old_flags, is_zip, crc, name) = {
            let Some(e) = self.manifest.get(&key) else {
                return Ok(());
            };
            (
                e.is_modified,
                e.fp_type,
                e.flags,
                e.old_flags,
                e.is_zip,
                e.crc32,
                s(&e.filename),
            )
        };
        if is_modified || fp_type != FpType::Fp {
            return Ok(());
        }
        if self.open_archive_fp().is_none() {
            return Err(format!(
                "phar error: Cannot open phar archive \"{fname}\" for reading"
            ));
        }
        let uncompressed = (old_flags != 0 && old_flags & ENT_COMPRESSION_MASK == 0)
            || flags & ENT_COMPRESSION_MASK == 0;
        if is_zip {
            self.verify_zip_local(&key)?;
        }
        if uncompressed {
            return Ok(());
        }
        let (offset, csize, usize_) = {
            let e = self.manifest.get(&key).ok_or_else(String::new)?;
            (e.offset, e.compressed_filesize, e.uncompressed_filesize)
        };
        let img = self.open_archive_fp().ok_or_else(String::new)?;
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(img.len());
        let end = start.saturating_add(csize as usize).min(img.len());
        let raw = &img[start..end];
        let mismatch = || {
            format!(
                "phar error: internal corruption of phar \"{fname}\" (actual filesize mismatch on file \"{name}\")"
            )
        };
        let method = if is_modified { old_flags } else { flags } & ENT_COMPRESSION_MASK;
        let out = if usize_ == 0 {
            Vec::new()
        } else {
            match method {
                ENT_COMPRESSED_GZ => crate::compress::inflate_raw(raw).unwrap_or_default(),
                ENT_COMPRESSED_BZ2 => crate::bz2::decompress(raw).0,
                _ => {
                    return Err(format!(
                        "phar error: unable to read phar \"{fname}\" (file \"{name}\" is compressed with an unknown compression algorithm)"
                    ))
                }
            }
        };
        if out.len() != usize_ as usize {
            return Err(mismatch());
        }
        let ok = php_hash::crc32(&out) == crc;
        let e = self.manifest.get_mut(&key).ok_or_else(String::new)?;
        e.old_flags = e.flags;
        e.fp_type = FpType::Ufp;
        e.offset = 0;
        e.data = Some(Rc::new(RefCell::new(out)));
        if !ok {
            return Err(format!(
                "phar error: internal corruption of phar \"{fname}\" (crc32 mismatch on file \"{name}\")"
            ));
        }
        e.is_crc_checked = true;
        Ok(())
    }

    /// Verifies the CRC of an uncompressed entry (`phar_postprocess_file`
    /// with `process_zip = 2`, done when a stream is opened).
    pub fn check_crc(&mut self, key: &[u8]) -> Result<(), String> {
        let fname = s(&self.fname);
        let (is_zip, crc, name) = match self.manifest.get(key) {
            Some(e) => (e.is_zip, e.crc32, s(&e.filename)),
            None => return Ok(()),
        };
        if is_zip {
            self.verify_zip_local(key)?;
        }
        let data = self.raw_content(key).unwrap_or_default();
        if php_hash::crc32(&data) == crc {
            if let Some(e) = self.manifest.get_mut(key) {
                e.is_crc_checked = true;
            }
            Ok(())
        } else {
            Err(format!(
                "phar error: internal corruption of phar \"{fname}\" (crc32 mismatch on file \"{name}\")"
            ))
        }
    }

    /// The content of `key` from its current fp without following links
    /// or decompressing (`phar_get_efp(entry, false)`); call
    /// [`Archive::open_entry_fp`] first.
    pub fn read_entry_raw(&mut self, key: &[u8]) -> Vec<u8> {
        self.raw_content(key).unwrap_or_default()
    }

    /// The uncompressed content of an entry (following links), opening and
    /// decompressing as needed.
    pub fn read_entry(&mut self, key: &[u8]) -> Result<Vec<u8>, String> {
        self.open_entry_fp(key, true)?;
        let src = self.link_source(key).unwrap_or_else(|| key.to_vec());
        Ok(self.raw_content(&src).unwrap_or_default())
    }

    /// The bytes as stored in the archive (compressed per `flags`).
    pub fn stored_bytes(&mut self, key: &[u8]) -> Option<Vec<u8>> {
        let (offset, csize) = {
            let e = self.manifest.get(key)?;
            (e.offset_abs, e.compressed_filesize)
        };
        let img = self.open_archive_fp()?;
        let start = usize::try_from(offset).ok()?;
        img.get(start..start.checked_add(csize as usize)?)
            .map(<[u8]>::to_vec)
    }

    /// `phar_separate_entry_fp()` / `phar_copy_entry_fp()`: gives the entry
    /// its own modifiable copy of its content.
    pub fn separate_entry(&mut self, key: &[u8]) -> Result<(), String> {
        if self
            .manifest
            .get(key)
            .is_some_and(|e| e.fp_type == FpType::Mod)
        {
            return Ok(());
        }
        let data = self.read_entry(key)?;
        let is_tar = self.is_tar;
        if let Some(e) = self.manifest.get_mut(key) {
            if e.symlink.take().is_some() {
                e.tar_type = if e.is_tar || is_tar {
                    crate::consts::TAR_FILE
                } else {
                    0
                };
            }
            e.data = Some(Rc::new(RefCell::new(data)));
            e.offset = 0;
            e.fp_type = FpType::Mod;
            e.is_modified = true;
        }
        Ok(())
    }

    /// `phar_create_writeable_entry()`: truncates the entry for writing.
    pub fn truncate_entry(&mut self, key: &[u8]) {
        if let Some(e) = self.manifest.get_mut(key) {
            if e.fp_type != FpType::Mod && e.symlink.take().is_some() {
                e.tar_type = if e.is_tar { crate::consts::TAR_FILE } else { 0 };
            }
            match (&e.data, e.fp_type) {
                (Some(d), FpType::Mod) => d.borrow_mut().clear(),
                _ => e.data = Some(Rc::new(RefCell::new(Vec::new()))),
            }
            e.old_flags = e.flags;
            e.is_modified = true;
            e.uncompressed_filesize = 0;
            e.compressed_filesize = 0;
            e.crc32 = 0;
            e.flags = ENT_PERM_DEF_FILE;
            e.fp_type = FpType::Mod;
            e.offset = 0;
        }
        self.is_modified = true;
    }

    /// `phar_get_entry_info_dir()` without the mounted-directory part:
    /// looks up an entry by path (see [`crate::registry`] for the full
    /// version). Returns the manifest key.
    pub fn find_entry(&self, path: &[u8]) -> Option<&Entry> {
        self.manifest.get(path).filter(|e| !e.is_deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_dirs() {
        let mut a = Archive::new(b"/tmp/x.tar".to_vec());
        assert_eq!(a.ext(), b".tar");
        let mut e = Entry::new_file(b"dir/a".to_vec(), 0);
        e.set_content(b"data".to_vec());
        a.manifest.add(b"dir/a".to_vec(), e);
        let mut l = Entry::new_file(b"dir/l".to_vec(), 0);
        l.symlink = Some(b"a".to_vec());
        a.manifest.add(b"dir/l".to_vec(), l);
        assert_eq!(a.link_source(b"dir/l"), Some(b"dir/a".to_vec()));
        assert_eq!(a.read_entry(b"dir/l").unwrap(), b"data");
        let mut c1 = Entry::new_file(b"c1".to_vec(), 0);
        c1.symlink = Some(b"c2".to_vec());
        let mut c2 = Entry::new_file(b"c2".to_vec(), 0);
        c2.symlink = Some(b"c1".to_vec());
        a.manifest.add(b"c1".to_vec(), c1);
        a.manifest.add(b"c2".to_vec(), c2);
        assert_eq!(a.link_source(b"c1"), None);
        a.add_virtual_dirs(b"x/y/z");
        assert!(a.virtual_dirs.contains_key(b"x/y"));
        assert!(a.virtual_dirs.contains_key(b"x"));
    }
}
