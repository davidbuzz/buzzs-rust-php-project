//! The native phar format: `__HALT_COMPILER();` detection
//! (`phar_open_from_fp()`), manifest parsing (`phar_parse_pharfile()`) and
//! writing (`phar_flush_ex()`).

use std::rc::Rc;

use crate::archive::{Archive, Entry, FpType, Metadata};
use crate::consts::{
    API_MIN_DIR, API_MIN_READ, API_VERSION, API_VERSION_NODIR, API_VER_MASK, ENT_COMPRESSED_BZ2,
    ENT_COMPRESSED_GZ, ENT_COMPRESSION_MASK, ENT_PERM_DEF_DIR, FILE_COMPRESSION_MASK,
    HALT_TOKEN, HDR_SIGNATURE, SIG_MD5, SIG_OPENSSL, SIG_OPENSSL_SHA256, SIG_OPENSSL_SHA512,
    SIG_SHA1, SIG_SHA256, SIG_SHA512,
};
use crate::util::{find_ci, get32, put32, s, to_u32};

/// What `phar_open_from_fp()` found at the start of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detected {
    /// Whole file gzip-compressed.
    Gzip,
    /// Whole file bzip2-compressed.
    Bzip2,
    /// A zip archive.
    Zip,
    /// A tar archive.
    Tar,
    /// A phar with the manifest after this offset.
    Phar(u64),
}

/// The window scan of `phar_open_from_fp()`. `first` enables the magic
/// checks (they only run on the first chunk and only while decompression
/// recursion is allowed).
pub fn detect(data: &[u8], fname: &[u8], magic: bool) -> Result<Detected, String> {
    const WINDOW: usize = 1024;
    let tokenlen = HALT_TOKEN.len();
    let mut buffer = vec![b' '; tokenlen];
    let mut pos = 0usize;
    let mut eof = false;
    let mut first = true;
    let mut halt_offset = 0usize;
    while !eof {
        let got = WINDOW.min(data.len() - pos);
        eof = got < WINDOW;
        if got < tokenlen {
            return Err(format!(
                "internal corruption of phar \"{}\" (truncated entry)",
                s(fname)
            ));
        }
        let chunk = &data[pos..pos + got];
        pos += got;
        if first && magic {
            first = false;
            if chunk.starts_with(b"\x1f\x8b\x08") {
                return Ok(Detected::Gzip);
            }
            if chunk.starts_with(b"BZh") {
                return Ok(Detected::Bzip2);
            }
            if chunk.starts_with(b"PK\x03\x04") {
                return Ok(Detected::Zip);
            }
            if got >= 512 && crate::tar_format::is_tar(chunk, fname) {
                return Ok(Detected::Tar);
            }
        }
        buffer.truncate(tokenlen);
        buffer.extend_from_slice(chunk);
        if let Some(p) = find_ci(&buffer, HALT_TOKEN) {
            halt_offset += p;
            return Ok(Detected::Phar(halt_offset as u64));
        }
        halt_offset += got;
        let tail = buffer[buffer.len() - tokenlen..].to_vec();
        buffer = tail;
    }
    Err(format!(
        "internal corruption of phar \"{}\" (__HALT_COMPILER(); not found)",
        s(fname)
    ))
}

/// A parsed phar, before registration.
#[derive(Debug)]
pub struct ParsedPhar {
    /// The archive.
    pub archive: Archive,
    /// Whether the alias must be registered.
    pub register_alias: bool,
}

/// `phar_parse_pharfile()`.
#[allow(clippy::too_many_lines)]
pub fn parse(
    image: Rc<Vec<u8>>,
    fname: &[u8],
    alias: Option<&[u8]>,
    halt_offset: u64,
    compression: u32,
    require_hash: bool,
) -> Result<ParsedPhar, String> {
    let data: &[u8] = &image;
    let fail = |msg: &str| Err(msg.replacen("%s", &s(fname), 1));
    let mut halt = usize::try_from(halt_offset).unwrap_or(usize::MAX);
    if halt > data.len() {
        return fail("cannot seek to __HALT_COMPILER(); location in phar \"%s\"");
    }
    let Some(b) = data.get(halt..halt + 3) else {
        return fail("internal corruption of phar \"%s\" (truncated manifest at stub end)");
    };
    if (b[0] == b' ' || b[0] == b'\n') && b[1] == b'?' && b[2] == b'>' {
        halt += 3;
        let Some(&next) = data.get(halt) else {
            return fail("internal corruption of phar \"%s\" (truncated manifest at stub end)");
        };
        if next == b'\r' {
            if data.get(halt + 1) != Some(&b'\n') {
                return fail(
                    "internal corruption of phar \"%s\" (truncated manifest at stub end)",
                );
            }
            halt += 1;
            halt += 1;
        } else if next == b'\n' {
            halt += 1;
        }
    }
    let Some(lenb) = data.get(halt..halt + 4) else {
        return fail("internal corruption of phar \"%s\" (truncated manifest at manifest length)");
    };
    let manifest_len = get32(lenb, 0) as usize;
    if manifest_len > 1_048_576 * 100 {
        return fail("manifest cannot be larger than 100 MB in phar \"%s\"");
    }
    let mstart = halt + 4;
    if manifest_len < 18 || data.len() < mstart + manifest_len {
        return fail("internal corruption of phar \"%s\" (truncated manifest header)");
    }
    let m = &data[mstart..mstart + manifest_len];
    let mut p = 0usize;
    let manifest_count = get32(m, p) as usize;
    p += 4;
    if manifest_count == 0 {
        return fail(
            "in phar \"%s\", manifest claims to have zero entries.  Phars must have at least 1 entry",
        );
    }
    let manifest_ver = u16::from_be_bytes([m[p], m[p + 1]]);
    p += 2;
    if manifest_ver & API_VER_MASK < API_MIN_READ {
        return Err(format!(
            "phar \"{}\" is API version {}.{}.{}, and cannot be processed",
            s(fname),
            manifest_ver >> 12,
            (manifest_ver >> 8) & 0xF,
            (manifest_ver >> 4) & 0xF
        ));
    }
    let mut manifest_flags = get32(m, p);
    p += 4;
    manifest_flags &= !FILE_COMPRESSION_MASK;
    manifest_flags |= compression;

    let mut sig_flags = 0;
    let mut signature = None;
    if manifest_flags & HDR_SIGNATURE != 0 {
        let broken = || Err(format!("phar \"{}\" has a broken signature", s(fname)));
        if data.len() < 20 + 8 || &data[data.len() - 4..] != b"GBMB" {
            return broken();
        }
        let flags = get32(data, data.len() - 8);
        sig_flags = flags;
        let end = data.len() - 8;
        let hashed = |len: usize, label: &str| -> Result<Vec<u8>, String> {
            let Some(start) = end.checked_sub(len) else {
                return Err(format!("phar \"{}\" has a broken signature", s(fname)));
            };
            crate::signature::verify(flags, &data[..start], &data[start..end], fname).map_err(
                |e| {
                    format!(
                        "phar \"{}\" {label} signature could not be verified: {e}",
                        s(fname)
                    )
                },
            )
        };
        signature = Some(match flags {
            SIG_OPENSSL | SIG_OPENSSL_SHA256 | SIG_OPENSSL_SHA512 => {
                let Some(lstart) = end.checked_sub(4) else {
                    return Err(format!(
                        "phar \"{}\" openssl signature length could not be read",
                        s(fname)
                    ));
                };
                let siglen = get32(data, lstart) as usize;
                let Some(sstart) = lstart.checked_sub(siglen).filter(|&x| x > 0) else {
                    return Err(format!(
                        "phar \"{}\" openssl signature could not be read",
                        s(fname)
                    ));
                };
                crate::signature::verify(flags, &data[..sstart], &data[sstart..lstart], fname)
                    .map_err(|e| {
                        format!(
                            "phar \"{}\" openssl signature could not be verified: {e}",
                            s(fname)
                        )
                    })?
            }
            SIG_SHA512 => hashed(64, "SHA512")?,
            SIG_SHA256 => hashed(32, "SHA256")?,
            SIG_SHA1 => hashed(20, "SHA1")?,
            SIG_MD5 => hashed(16, "MD5")?,
            _ => {
                return Err(format!(
                    "phar \"{}\" has a broken or unsupported signature",
                    s(fname)
                ))
            }
        });
    } else if require_hash {
        return Err(format!("phar \"{}\" does not have a signature", s(fname)));
    }

    // alias
    if p + 4 > m.len() {
        return fail("internal corruption of phar \"%s\" (truncated manifest header)");
    }
    let tmp_len = get32(m, p) as usize;
    p += 4;
    if p + tmp_len > m.len() {
        return fail("internal corruption of phar \"%s\" (buffer overrun)");
    }
    if manifest_len < 18 + tmp_len {
        return fail("internal corruption of phar \"%s\" (truncated manifest header)");
    }
    let mut register_alias = false;
    let mut temp_alias = false;
    let mut the_alias: Option<Vec<u8>> = None;
    if tmp_len > 0 {
        let stored = &m[p..p + tmp_len];
        if let Some(a) = alias.filter(|a| !a.is_empty()) {
            if a != stored {
                return Err(format!(
                    "cannot load phar \"{}\" with implicit alias \"{}\" under different alias \"{}\"",
                    s(fname),
                    s(stored),
                    s(crate::util::cstr(a))
                ));
            }
        }
        the_alias = Some(stored.to_vec());
        p += tmp_len;
        register_alias = true;
    } else if let Some(a) = alias.filter(|a| !a.is_empty()) {
        the_alias = Some(a.to_vec());
        register_alias = true;
        temp_alias = true;
    }

    if manifest_count > (manifest_len - 18 - tmp_len) / (5 * 4 + 1) {
        return fail(
            "internal corruption of phar \"%s\" (too many manifest entries for size of manifest)",
        );
    }
    if p + 4 > m.len() {
        return fail("internal corruption of phar \"%s\" (truncated manifest header)");
    }
    let mlen = get32(m, p) as usize;
    p += 4;
    if mlen > m.len() - p {
        return fail("internal corruption of phar \"%s\" (trying to read past buffer end)");
    }
    let mut ar = Archive::new(fname.to_vec());
    ar.is_brandnew = false;
    ar.is_writeable = false;
    ar.metadata = Metadata::from_serialized(&m[p..p + mlen]);
    p += mlen;

    let first_offset = (halt + manifest_len + 4) as u64;
    let mut offset = first_offset;
    for _ in 0..manifest_count {
        if p + 28 > m.len() {
            return fail("internal corruption of phar \"%s\" (truncated manifest entry)");
        }
        let mut filename_len = get32(m, p) as usize;
        p += 4;
        if filename_len == 0 {
            return fail("zero-length filename encountered in phar \"%s\"");
        }
        if filename_len > m.len() - p - 24 {
            return fail("internal corruption of phar \"%s\" (truncated manifest entry)");
        }
        let mut e = Entry::default();
        e.is_dir = manifest_ver & API_VER_MASK >= API_MIN_DIR && m[p + filename_len - 1] == b'/';
        ar.add_virtual_dirs(&m[p..p + filename_len]);
        let raw = p;
        p += filename_len;
        e.uncompressed_filesize = get32(m, p);
        e.timestamp = get32(m, p + 4);
        if offset == first_offset || ar.max_timestamp < e.timestamp {
            ar.max_timestamp = e.timestamp;
        }
        e.compressed_filesize = get32(m, p + 8);
        e.crc32 = get32(m, p + 12);
        e.flags = get32(m, p + 16);
        p += 20;
        if e.is_dir {
            filename_len -= 1;
            e.flags |= ENT_PERM_DEF_DIR;
        }
        e.filename = m[raw..raw + filename_len].to_vec();
        let elen = get32(m, p) as usize;
        p += 4;
        if elen > m.len() - p {
            return fail("internal corruption of phar \"%s\" (truncated manifest entry)");
        }
        e.metadata = Metadata::from_serialized(&m[p..p + elen]);
        p += elen;
        e.offset = offset;
        e.offset_abs = offset;
        offset += u64::from(e.compressed_filesize);
        match e.flags & ENT_COMPRESSION_MASK {
            ENT_COMPRESSED_GZ | ENT_COMPRESSED_BZ2 => {}
            _ => {
                if e.uncompressed_filesize != e.compressed_filesize {
                    return fail("internal corruption of phar \"%s\" (compressed and uncompressed size does not match for uncompressed entry)");
                }
            }
        }
        manifest_flags |= e.flags & ENT_COMPRESSION_MASK;
        e.is_crc_checked = manifest_flags & HDR_SIGNATURE != 0;
        e.fp_type = FpType::Fp;
        e.inode = crate::util::entry_inode(fname, &e.filename);
        let key = e.filename.clone();
        ar.manifest.add(key, e);
    }
    ar.version = format!(
        "{}.{}.{}",
        manifest_ver >> 12,
        (manifest_ver >> 8) & 0xF,
        (manifest_ver >> 4) & 0xF
    );
    ar.halt_offset = halt as u64;
    ar.flags = manifest_flags;
    ar.alias = Some(the_alias.clone().unwrap_or_else(|| fname.to_vec()));
    ar.is_temporary_alias = if register_alias { temp_alias } else { true };
    ar.sig_flags = sig_flags;
    ar.signature = signature;
    ar.fp = Some(image);
    Ok(ParsedPhar {
        archive: ar,
        register_alias,
    })
}

/// The output of a flush: the new (uncompressed) archive image.
#[derive(Debug)]
pub struct Flushed {
    /// The new archive bytes, before whole-archive compression.
    pub image: Vec<u8>,
}

/// Content of an entry for writing: the uncompressed bytes, or `None` when
/// they cannot be obtained (the entry is then skipped like `phar_open_jit`
/// failures).
pub(crate) fn uncompressed_for_write(ar: &mut Archive, key: &[u8]) -> Option<Vec<u8>> {
    ar.read_entry(key).ok()
}

/// `phar_flush_ex()` for the phar format. `old` is the previous file
/// image (`oldfile`), `private_key` the `setSignatureAlgorithm()` key.
#[allow(clippy::too_many_lines)]
pub fn flush(
    ar: &mut Archive,
    user_stub: Option<&[u8]>,
    old: Option<&Rc<Vec<u8>>>,
    private_key: &[u8],
) -> Result<Flushed, String> {
    let fname = s(&ar.fname);
    let mut newfile: Vec<u8> = Vec::new();
    if let Some(stub) = user_stub {
        let Some(normalized) = crate::stub::normalize_user_stub(stub) else {
            return Err(format!(
                "illegal stub for phar \"{fname}\" (__HALT_COMPILER(); is missing)"
            ));
        };
        newfile.extend_from_slice(&normalized);
        ar.halt_offset = newfile.len() as u64;
    } else if ar.halt_offset > 0 && old.is_some() && !ar.is_brandnew {
        let old = old.map(|o| o.as_slice()).unwrap_or_default();
        let n = usize::try_from(ar.halt_offset).unwrap_or(usize::MAX).min(old.len());
        newfile.extend_from_slice(&old[..n]);
        if n as u64 != ar.halt_offset {
            return Err(format!(
                "unable to copy stub of old phar to new phar \"{fname}\""
            ));
        }
    } else {
        let stub = crate::stub::create_default_stub(None, None).unwrap_or_default();
        ar.halt_offset = stub.len() as u64;
        newfile.extend_from_slice(&stub);
    }
    let halt_offset = newfile.len() as u64;

    // drop deleted entries nobody references any more
    ar.manifest
        .retain(|_, e| !(e.is_deleted && e.fp_refcount == 0 && e.fileinfo_lock_count == 0));

    let main_meta = ar.metadata.bytes().to_vec();
    let mut global_flags = 0u32;
    let mut new_count = 0u32;
    let mut has_dirs = false;
    let mut offset = 0usize;
    ar.virtual_dirs.clear();
    let keys = ar.manifest.keys();
    // compressed payloads of re-compressed entries
    let mut cfp: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for key in &keys {
        let (deleted, mounted, is_dir, modified, fname_len, meta_len) = {
            let e = ar.manifest.get(key).ok_or_else(String::new)?;
            (
                e.is_deleted,
                e.is_mounted,
                e.is_dir,
                e.is_modified,
                e.filename.len(),
                e.metadata.bytes().len(),
            )
        };
        if deleted || mounted {
            continue;
        }
        new_count += 1;
        ar.add_virtual_dirs(key);
        has_dirs |= is_dir;
        offset += 4 + fname_len + 24 + meta_len + usize::from(is_dir);
        if (old.is_some() && !modified) || is_dir {
            if let Some(e) = ar.manifest.get_mut(key) {
                if e.fp_type == FpType::Ufp {
                    e.fp_type = FpType::Fp;
                    e.offset = e.offset_abs;
                    e.data = None;
                }
            }
            continue;
        }
        let Some(content) = uncompressed_for_write(ar, key) else {
            continue;
        };
        let e = ar.manifest.get_mut(key).ok_or_else(String::new)?;
        e.crc32 = php_hash::crc32(&content);
        e.is_crc_checked = true;
        if e.flags & ENT_COMPRESSION_MASK == 0 {
            e.compressed_filesize = e.uncompressed_filesize;
            continue;
        }
        let compressed = match e.flags & ENT_COMPRESSION_MASK {
            ENT_COMPRESSED_GZ => crate::compress::deflate_raw(&content),
            _ => crate::compress::bzip2(&content),
        };
        let Some(compressed) = compressed else {
            return Err(format!(
                "unable to {} compress file \"{}\" to new phar \"{fname}\"",
                if e.flags & ENT_COMPRESSED_GZ != 0 { "gzip" } else { "bzip2" },
                s(&e.filename)
            ));
        };
        e.compressed_filesize = to_u32(compressed.len());
        e.old_flags = e.flags;
        e.is_modified = true;
        global_flags |= e.flags & ENT_COMPRESSION_MASK;
        cfp.push((key.clone(), compressed));
    }
    global_flags |= HDR_SIGNATURE;

    let alias_len = if ar.is_temporary_alias { 0 } else { ar.alias_len() };
    let mut manifest_len = to_u32(offset + alias_len + 18 + main_meta.len());
    let mut manifest_hack = false;
    if matches!(manifest_len & 0xFF, 0x0D | 0x0A) {
        manifest_len += 1;
        manifest_hack = true;
    }
    put32(&mut newfile, manifest_len);
    put32(&mut newfile, new_count);
    let ver = if has_dirs { API_VERSION } else { API_VERSION_NODIR };
    newfile.extend_from_slice(&ver.to_be_bytes());
    put32(&mut newfile, global_flags);
    put32(&mut newfile, to_u32(alias_len));
    newfile.extend_from_slice(&ar.alias_bytes()[..alias_len]);
    put32(&mut newfile, to_u32(main_meta.len()));
    newfile.extend_from_slice(&main_meta);

    let mytime = crate::sys::now32();
    for key in &keys {
        let Some(e) = ar.manifest.get(key) else { continue };
        if e.is_deleted || e.is_mounted {
            continue;
        }
        put32(&mut newfile, to_u32(e.filename.len() + usize::from(e.is_dir)));
        newfile.extend_from_slice(&e.filename);
        if e.is_dir {
            newfile.push(b'/');
        }
        put32(&mut newfile, e.uncompressed_filesize);
        put32(&mut newfile, mytime);
        put32(&mut newfile, e.compressed_filesize);
        put32(&mut newfile, e.crc32);
        put32(&mut newfile, e.flags);
        let meta = e.metadata.bytes();
        put32(&mut newfile, to_u32(meta.len()));
        newfile.extend_from_slice(meta);
    }
    if manifest_hack {
        newfile.push(to_u32(main_meta.len()).to_le_bytes()[0]);
    }

    for key in &keys {
        let (skip, csize) = match ar.manifest.get(key) {
            Some(e) => (e.is_deleted || e.is_dir || e.is_mounted, e.compressed_filesize),
            None => continue,
        };
        if skip {
            continue;
        }
        let payload = if let Some(pos) = cfp.iter().position(|(k, _)| k == key) {
            cfp.swap_remove(pos).1
        } else {
            let e = ar.manifest.get(key).ok_or_else(String::new)?;
            match e.fp_type {
                FpType::Fp => ar.stored_bytes(key).unwrap_or_default(),
                _ => {
                    let mut c = ar.read_entry(key).unwrap_or_default();
                    c.truncate(csize as usize);
                    c
                }
            }
        };
        let pos = newfile.len() as u64;
        newfile.extend_from_slice(&payload);
        if let Some(e) = ar.manifest.get_mut(key) {
            e.offset = pos;
            e.offset_abs = pos;
            e.is_modified = false;
            if matches!(e.fp_type, FpType::Mod | FpType::Ufp) {
                e.fp_type = FpType::Fp;
                e.data = None;
            }
        }
    }

    // signature
    let mut sig_flags = ar.sig_flags;
    let sig = crate::signature::create(&mut sig_flags, &newfile, private_key, &ar.fname)
        .map_err(|e| format!("phar error: unable to write signature: {e}"))?;
    ar.sig_flags = sig_flags;
    ar.signature = Some(crate::util::hex_upper(&sig));
    newfile.extend_from_slice(&sig);
    if crate::signature::is_openssl(sig_flags) {
        put32(&mut newfile, to_u32(sig.len()));
    }
    put32(&mut newfile, sig_flags);
    newfile.extend_from_slice(b"GBMB");
    ar.halt_offset = halt_offset;
    ar.is_brandnew = false;
    Ok(Flushed { image: newfile })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_halt() {
        let mut data = b"<?php echo 1; __HALT_COMPILER(); ?>\r\n".to_vec();
        data.extend_from_slice(&[0u8; 30]);
        assert_eq!(
            detect(&data, b"x.phar", true),
            Ok(Detected::Phar(32))
        );
        assert!(detect(b"blah", b"/x.phar", true)
            .unwrap_err()
            .contains("(truncated entry)"));
        assert!(detect(&[b'a'; 100], b"/x.phar", true)
            .unwrap_err()
            .contains("not found"));
        // token straddling the 1024 boundary
        let mut big = vec![b'a'; 1015];
        big.extend_from_slice(b"__halt_compiler();");
        big.extend_from_slice(&[b'b'; 40]);
        assert_eq!(detect(&big, b"x", true), Ok(Detected::Phar(1033)));
        assert_eq!(detect(b"\x1f\x8b\x08aaaaaaaaaaaaaaaaaaaaaa", b"x", true), Ok(Detected::Gzip));
    }
}
