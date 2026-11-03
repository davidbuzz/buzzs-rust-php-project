//! Tar-based archives (`tar.c`): ustar/old-style reading with pax header
//! skipping, `././@LongLink` names, magic `.phar/*` files, and PHP's own
//! ustar writer.

use std::rc::Rc;

use crate::archive::{Archive, Entry, FpType, Metadata};
use crate::consts::{
    ENT_PERM_DEF_FILE, ENT_PERM_MASK, HALT_TOKEN, STUB_END_SEQUENCE, TAR_BLOCK, TAR_CHAR, TAR_DIR,
    TAR_FIFO, TAR_FILE, TAR_FILE_HDR, TAR_GLOBAL_HDR, TAR_LINK, TAR_LONGLINK, TAR_LONGNAME,
    TAR_NEWSTUB, TAR_SYMLINK,
};
use crate::util::{cstr, find_ci, put32, s, to_u32, validate_alias};

/// `phar_tar_number()`: octal with leading spaces.
fn tar_number(buf: &[u8]) -> u32 {
    let mut i = 0;
    while i < buf.len() && buf[i] == b' ' {
        i += 1;
    }
    let mut num: u32 = 0;
    while i < buf.len() && (b'0'..=b'7').contains(&buf[i]) {
        num = num.wrapping_mul(8).wrapping_add(u32::from(buf[i] - b'0'));
        i += 1;
    }
    num
}

/// `phar_tar_size()`: a strict octal size field.
fn tar_size(buf: &[u8]) -> Option<u32> {
    let mut i = 0;
    while i < buf.len() && buf[i] == b' ' {
        i += 1;
    }
    if i < buf.len() && buf[i] & 0x80 != 0 {
        return None;
    }
    let mut num: u64 = 0;
    while i < buf.len() && (b'0'..=b'7').contains(&buf[i]) {
        num = num
            .saturating_mul(8)
            .saturating_add(u64::from(buf[i] - b'0'));
        i += 1;
    }
    while i < buf.len() && (buf[i] == b' ' || buf[i] == 0) {
        i += 1;
    }
    if i != buf.len() || num > u64::from(u32::MAX - 511) {
        return None;
    }
    u32::try_from(num).ok()
}

/// `phar_tar_octal()`: fixed-width octal; `false` (and all 7s) on overflow.
fn tar_octal(buf: &mut [u8], mut val: u32) -> bool {
    for b in buf.iter_mut().rev() {
        *b = b'0' + (val & 7) as u8;
        val >>= 3;
    }
    if val == 0 {
        return true;
    }
    buf.fill(b'7');
    false
}

/// `phar_tar_checksum()`.
fn tar_checksum(buf: &[u8]) -> u32 {
    buf.iter().map(|&b| u32::from(b)).sum()
}

fn type_has_data(t: u8) -> bool {
    !matches!(
        t,
        TAR_LINK | TAR_SYMLINK | TAR_CHAR | TAR_BLOCK | TAR_DIR | TAR_FIFO
    )
}

/// `phar_is_tar()`: checks the first header's checksum, or trusts a
/// `.tar` extension for corrupted files.
pub fn is_tar(buf: &[u8], fname: &[u8]) -> bool {
    if buf.len() < 512 || buf.starts_with(b"<?php") {
        return false;
    }
    let checksum = tar_number(&buf[148..156]);
    let mut hdr = buf[..512].to_vec();
    hdr[148..156].fill(b' ');
    let ok = checksum == tar_checksum(&hdr);
    let base = match fname.iter().rposition(|&c| c == b'/') {
        Some(p) => &fname[p..],
        None => fname,
    };
    if !ok {
        if let Some(p) = crate::util::find(base, b".tar") {
            let after = base.get(p + 4).copied().unwrap_or(0);
            if after == 0 || after == b'.' {
                return true;
            }
        }
    }
    ok
}

/// A parsed tar, before registration.
#[derive(Debug)]
pub struct ParsedTar {
    /// The archive.
    pub archive: Archive,
    /// The alias from `.phar/alias.txt`.
    pub actual_alias: Option<Vec<u8>>,
}

/// `phar_tar_process_metadata()`.
fn process_metadata(ar: &mut Archive, key: &[u8], data: &[u8]) -> bool {
    let Some(e) = ar.manifest.get_mut(key) else {
        return false;
    };
    e.metadata = Metadata::from_serialized(data);
    if key == b".phar/.metadata.bin" {
        if ar.metadata.has_data() {
            return false;
        }
        let m = std::mem::take(&mut e.metadata);
        ar.metadata = m;
    } else if key.len() >= 31 {
        let target = key[16..key.len() - 14].to_vec();
        if ar.manifest.contains_key(&target) {
            let m = ar
                .manifest
                .get_mut(key)
                .map(|e| std::mem::take(&mut e.metadata))
                .unwrap_or_default();
            if let Some(t) = ar.manifest.get_mut(&target) {
                if t.metadata.has_data() {
                    return false;
                }
                t.metadata = m;
            }
        }
    }
    true
}

/// `phar_parse_tarfile()`.
#[allow(clippy::too_many_lines)]
pub fn parse(
    image: Rc<Vec<u8>>,
    fname: &[u8],
    compression: u32,
    require_hash: bool,
) -> Result<ParsedTar, String> {
    let data: &[u8] = &image;
    let f = s(fname);
    let totalsize = data.len();
    let truncated = || format!("phar error: \"{f}\" is a corrupted tar file (truncated)");
    if totalsize < 512 {
        return Err(format!(
            "phar error: \"{f}\" is not a tar file or is truncated"
        ));
    }
    let mut buf = data[..512].to_vec();
    let mut cur = 512usize;
    let mut old = &buf[257..262] != b"ustar";
    let mut ar = Archive::new(fname.to_vec());
    ar.is_brandnew = false;
    ar.is_writeable = false;
    ar.is_tar = true;
    ar.version = String::new();
    ar.flags = compression;
    let mut actual_alias: Option<Vec<u8>> = None;
    let mut last_was_longlink = false;
    let mut long_name: Vec<u8> = Vec::new();

    loop {
        let pos = cur;
        let sum1 = tar_number(&buf[148..156]);
        if sum1 == 0 && tar_checksum(&buf) == 0 {
            break;
        }
        buf[148..156].fill(b' ');
        let mut sum2 = tar_checksum(&buf[..if old { 257 } else { 512 }]);
        if old && sum2 != sum1 {
            let sum3 = tar_checksum(&buf);
            if sum3 == sum1 {
                sum2 = sum3;
                old = false;
            }
        }
        let Some(mut size) = tar_size(&buf[124..136]) else {
            return Err(format!(
                "phar error: \"{f}\" is a corrupted tar file (invalid entry size)"
            ));
        };
        let typeflag = buf[156];
        if typeflag == TAR_LONGLINK {
            return Err(format!(
                "phar error: \"{f}\" is a tar file with an unsupported GNU long link entry"
            ));
        }
        let prefix0 = buf[345];
        let mut skip_to_next = false;
        if !old && (typeflag == TAR_GLOBAL_HDR || typeflag == TAR_FILE_HDR) {
            size = (size + 511) & !511;
            skip_to_next = true;
        }
        if !skip_to_next && (old || prefix0 == 0) && cstr(&buf[..100]) == b".phar/signature.bin" {
            if size > 511 {
                return Err(format!(
                    "phar error: tar-based phar \"{f}\" has signature that is larger than 511 bytes, cannot process"
                ));
            }
            let curloc = cur;
            let sz = size as usize;
            let Some(sig) = data.get(cur..cur + sz).filter(|b| b.len() > 8) else {
                return Err(format!(
                    "phar error: tar-based phar \"{f}\" signature cannot be read"
                ));
            };
            // `PHAR_GET_32` on little-endian reads a single (signed) char.
            let sig_flags = if sig[0] >= 0x80 {
                0xFFFF_FF00 | u32::from(sig[0])
            } else {
                u32::from(sig[0])
            };
            ar.sig_flags = sig_flags;
            let end = curloc - 512;
            let hex = crate::signature::verify(sig_flags, &data[..end], &sig[8..], fname).map_err(
                |e| format!("phar error: tar-based phar \"{f}\" signature cannot be verified: {e}"),
            )?;
            ar.signature = Some(hex);
            cur = curloc + 512;
            if (typeflag == 0 || typeflag == TAR_FILE) && size > 0 {
                cur += 512;
                if cur > totalsize {
                    return Err(truncated());
                }
            }
            let Some(next) = data.get(cur..cur + 512) else {
                return Err(truncated());
            };
            let sum1 = tar_number(&next[148..156]);
            if sum1 == 0 && tar_checksum(next) == 0 {
                break;
            }
            return Err(format!(
                "phar error: \"{f}\" has entries after signature, invalid phar"
            ));
        }
        if !skip_to_next {
            if !last_was_longlink && typeflag == TAR_LONGNAME {
                last_was_longlink = true;
                if size == 0 || size as usize > totalsize || size > u32::from(u16::MAX) {
                    return Err(format!(
                        "phar error: \"{f}\" is a corrupted tar file (invalid entry size)"
                    ));
                }
                let Some(name) = data.get(cur..cur + size as usize) else {
                    return Err(truncated());
                };
                long_name = name.to_vec();
                cur += size as usize;
                cur += (((size + 511) & !511) - size) as usize;
                if cur > totalsize {
                    return Err(truncated());
                }
                let Some(next) = data.get(cur..cur + 512) else {
                    return Err(truncated());
                };
                buf = next.to_vec();
                cur += 512;
                continue;
            }
            let filename = if last_was_longlink {
                std::mem::take(&mut long_name)
            } else if !old && prefix0 != 0 {
                let mut name = cstr(&buf[345..500]).to_vec();
                name.push(b'/');
                name.extend_from_slice(cstr(&buf[..100]));
                if name.last() == Some(&b'/') {
                    name.pop();
                }
                name
            } else {
                let mut name = cstr(&buf[..100]).to_vec();
                if name.last() == Some(&b'/') {
                    name.pop();
                }
                name
            };
            last_was_longlink = false;
            ar.add_virtual_dirs(&filename);
            if sum1 != sum2 {
                return Err(format!(
                    "phar error: \"{f}\" is a corrupted tar file (checksum mismatch of file \"{}\")",
                    s(cstr(&filename))
                ));
            }
            let mode = tar_number(&buf[100..108]);
            let mut e = Entry {
                is_tar: true,
                is_crc_checked: true,
                uncompressed_filesize: size,
                compressed_filesize: size,
                tar_type: if old && typeflag == 0 {
                    TAR_FILE
                } else {
                    typeflag
                },
                offset: pos as u64,
                offset_abs: pos as u64,
                fp_type: FpType::Fp,
                flags: mode & ENT_PERM_MASK,
                timestamp: tar_number(&buf[136..148]),
                ..Entry::default()
            };
            if old && e.tar_type == TAR_FILE && mode & 0o170_000 == 0o040_000 {
                e.tar_type = TAR_DIR;
            }
            e.is_dir = e.tar_type == TAR_DIR;
            let linkname = cstr(&buf[157..257]);
            if e.tar_type == TAR_LINK {
                if !ar.manifest.contains_key(linkname) {
                    return Err(format!(
                        "phar error: \"{f}\" is a corrupted tar file - hard link to non-existent file \"{}\"",
                        s(linkname)
                    ));
                }
                e.symlink = Some(linkname.to_vec());
            } else if e.tar_type == TAR_SYMLINK {
                e.symlink = Some(linkname.to_vec());
            }
            e.inode = crate::util::entry_inode(fname, &filename);
            e.filename.clone_from(&filename);
            ar.manifest.update(filename.clone(), e);
            if filename.starts_with(b".phar/.metadata") {
                let meta = data.get(cur..cur + size as usize);
                let ok = size as usize <= totalsize
                    && meta.is_some_and(|m| process_metadata(&mut ar, &filename, m));
                if !ok {
                    return Err(format!(
                        "phar error: tar-based phar \"{f}\" has invalid metadata in magic file \"{}\"",
                        s(&filename)
                    ));
                }
            }
            if actual_alias.is_none() && filename == b".phar/alias.txt" {
                if size > 511 {
                    return Err(format!(
                        "phar error: tar-based phar \"{f}\" has alias that is larger than 511 bytes, cannot process"
                    ));
                }
                let Some(a) = data.get(cur..cur + size as usize) else {
                    return Err(format!(
                        "phar error: Unable to read alias from tar-based phar \"{f}\""
                    ));
                };
                if !validate_alias(a) {
                    let mut shown = cstr(a).to_vec();
                    if a.len() > 50 {
                        shown = cstr(&a[..50]).to_vec();
                        if shown.len() == 50 {
                            shown.extend_from_slice(b"...");
                        }
                    }
                    return Err(format!(
                        "phar error: invalid alias \"{}\" in tar-based phar \"{f}\"",
                        s(&shown)
                    ));
                }
                actual_alias = Some(a.to_vec());
            }
            size = (size + 511) & !511;
        }
        if skip_to_next || (type_has_data(typeflag) && size > 0) {
            cur += size as usize;
            if cur > totalsize {
                return Err(truncated());
            }
        }
        if cur == totalsize {
            break;
        }
        let Some(next) = data.get(cur..cur + 512) else {
            return Err(truncated());
        };
        buf = next.to_vec();
        cur += 512;
    }

    ar.is_data = !ar.manifest.contains_key(b".phar/stub.php");
    if !ar.is_data && require_hash && ar.signature.is_none() {
        return Err(format!("tar-based phar \"{f}\" does not have a signature"));
    }
    ar.fp = Some(image);
    Ok(ParsedTar {
        archive: ar,
        actual_alias,
    })
}

/// A magic `.phar/...` entry with the given content.
fn magic_entry(name: &[u8], content: &[u8], flags: u32, timestamp: u32) -> Entry {
    let mut e = Entry {
        filename: name.to_vec(),
        flags,
        timestamp,
        is_tar: true,
        tar_type: TAR_FILE,
        is_crc_checked: true,
        ..Entry::default()
    };
    e.set_content(content.to_vec());
    e
}

/// `phar_tar_setmetadata()`.
fn set_metadata(e: &mut Entry, meta: &Metadata) {
    e.metadata = meta.clone();
    let bytes = meta.bytes().to_vec();
    e.set_content(bytes);
    e.offset_abs = 0;
}

/// Writes one ustar header (and content) — `phar_tar_writeheaders_int()`.
/// Returns the error message on failure.
fn write_header(
    out: &mut Vec<u8>,
    fname: &[u8],
    e: &mut Entry,
    content: &[u8],
) -> Result<(), String> {
    let f = s(fname);
    let name = &e.filename;
    let mut h = [0u8; 512];
    if name.len() > 100 {
        let too_long = || {
            format!(
                "tar-based phar \"{f}\" cannot be created, filename \"{}\" is too long for tar file format",
                s(name)
            )
        };
        if name.len() > 256 {
            return Err(too_long());
        }
        let mut b = name.len() - 101;
        while b < name.len() && name[b] != b'/' {
            b += 1;
        }
        if b >= name.len() || b > 155 {
            return Err(too_long());
        }
        h[345..345 + b].copy_from_slice(&name[..b]);
        let rest = &name[b + 1..];
        h[..rest.len()].copy_from_slice(rest);
    } else {
        h[..name.len()].copy_from_slice(name);
    }
    tar_octal(&mut h[100..107], e.flags & ENT_PERM_MASK);
    if !tar_octal(&mut h[124..135], e.uncompressed_filesize) {
        return Err(format!(
            "tar-based phar \"{f}\" cannot be created, filename \"{}\" is too large for tar file format",
            s(name)
        ));
    }
    if !tar_octal(&mut h[136..147], e.timestamp) {
        return Err(format!(
            "tar-based phar \"{f}\" cannot be created, file modification time of file \"{}\" is too large for tar file format",
            s(name)
        ));
    }
    h[156] = e.tar_type;
    if let Some(link) = &e.symlink {
        let l = cstr(link);
        if l.len() >= 100 {
            return Err(format!(
                "tar-based phar \"{f}\" cannot be created, link \"{}\" is too long for format",
                s(l)
            ));
        }
        h[157..157 + l.len()].copy_from_slice(l);
    }
    h[257..262].copy_from_slice(b"ustar");
    h[263..265].copy_from_slice(b"00");
    h[148..156].fill(b' ');
    e.crc32 = tar_checksum(&h);
    if !tar_octal(&mut h[148..155], e.crc32) {
        return Err(format!(
            "tar-based phar \"{f}\" cannot be created, checksum of file \"{}\" is too large for tar file format",
            s(name)
        ));
    }
    e.header_offset = out.len() as u64;
    out.extend_from_slice(&h);
    let start = out.len();
    let pos = start as u64;
    if e.uncompressed_filesize > 0 {
        let n = (e.uncompressed_filesize as usize).min(content.len());
        out.extend_from_slice(&content[..n]);
        let total = (e.uncompressed_filesize as usize + 511) & !511;
        out.resize(start + total, 0);
    }
    e.is_modified = false;
    e.fp_type = FpType::Fp;
    e.data = None;
    e.offset = pos;
    e.offset_abs = pos;
    Ok(())
}

/// `phar_tar_flush()`: rewrites the archive; returns the new image.
#[allow(clippy::too_many_lines)]
pub fn flush(
    ar: &mut Archive,
    user_stub: Option<&[u8]>,
    is_default_stub: bool,
    private_key: &[u8],
) -> Result<Vec<u8>, String> {
    let fname = ar.fname.clone();
    let f = s(&fname);
    let now = crate::sys::now32();
    if !ar.is_data {
        if !ar.is_temporary_alias && ar.alias_len() > 0 {
            let alias = ar.alias_bytes().to_vec();
            let mut e = magic_entry(b".phar/alias.txt", &alias, ENT_PERM_DEF_FILE, now);
            e.compressed_filesize = 0;
            ar.manifest.update(b".phar/alias.txt".to_vec(), e);
        } else {
            ar.manifest.remove(b".phar/alias.txt");
        }
        match user_stub {
            Some(stub) if !is_default_stub => {
                let Some(pos) = find_ci(stub, HALT_TOKEN) else {
                    return Err(format!("illegal stub for tar-based phar \"{f}\""));
                };
                let mut content = stub[..pos + HALT_TOKEN.len()].to_vec();
                content.extend_from_slice(STUB_END_SEQUENCE);
                let e = magic_entry(b".phar/stub.php", &content, ENT_PERM_DEF_FILE, now);
                ar.manifest.update(b".phar/stub.php".to_vec(), e);
            }
            _ => {
                let e = magic_entry(b".phar/stub.php", TAR_NEWSTUB, ENT_PERM_DEF_FILE, now);
                if is_default_stub {
                    ar.manifest.update(b".phar/stub.php".to_vec(), e);
                } else {
                    ar.manifest.add(b".phar/stub.php".to_vec(), e);
                }
            }
        }
    }

    if ar.metadata.has_data() {
        let meta = ar.metadata.clone();
        if let Some(e) = ar.manifest.get_mut(b".phar/.metadata.bin") {
            set_metadata(e, &meta);
        } else {
            let mut e = magic_entry(b".phar/.metadata.bin", b"", 0, 0);
            set_metadata(&mut e, &meta);
            ar.manifest.add(b".phar/.metadata.bin".to_vec(), e);
        }
    }

    // phar_tar_setupmetadata
    let mut i = 0;
    loop {
        let keys = ar.manifest.keys();
        let Some(key) = keys.get(i).cloned() else {
            break;
        };
        i += 1;
        if key.starts_with(b".phar/.metadata") {
            if key == b".phar/.metadata.bin" {
                let meta = ar.metadata.clone();
                if let Some(e) = ar.manifest.get_mut(&key) {
                    set_metadata(e, &meta);
                }
                continue;
            }
            if key.len() >= 31 && !ar.manifest.contains_key(&key[16..key.len() - 14]) {
                ar.manifest.remove(&key);
                i -= 1;
            }
            continue;
        }
        let (modified, meta) = match ar.manifest.get(&key) {
            Some(e) => (e.is_modified, e.metadata.clone()),
            None => continue,
        };
        if !modified {
            continue;
        }
        let mut lookfor = b".phar/.metadata/".to_vec();
        lookfor.extend_from_slice(&key);
        lookfor.extend_from_slice(b"/.metadata.bin");
        if !meta.has_data() {
            let before = ar.manifest.keys().iter().position(|k| *k == lookfor);
            if ar.manifest.remove(&lookfor).is_some() && before.is_some_and(|b| b < i) {
                i -= 1;
            }
            continue;
        }
        if let Some(m) = ar.manifest.get_mut(&lookfor) {
            set_metadata(m, &meta);
        } else {
            let mut m = magic_entry(&lookfor, b"", 0, 0);
            set_metadata(&mut m, &meta);
            ar.manifest.add(lookfor, m);
        }
    }

    // phar_tar_writeheaders
    let mut out = Vec::new();
    ar.virtual_dirs.clear();
    for key in ar.manifest.keys() {
        let Some((mounted, deleted, removable, size)) = ar.manifest.get(&key).map(|e| {
            (
                e.is_mounted,
                e.is_deleted,
                e.fp_refcount == 0 && e.fileinfo_lock_count == 0,
                e.uncompressed_filesize,
            )
        }) else {
            continue;
        };
        if mounted {
            continue;
        }
        if deleted {
            if removable {
                ar.manifest.remove(&key);
            }
            continue;
        }
        ar.add_virtual_dirs(&key);
        let content = if size > 0 {
            ar.open_entry_fp(&key, false)?;
            ar.read_entry_raw(&key)
        } else {
            Vec::new()
        };
        let e = ar.manifest.get_mut(&key).ok_or_else(String::new)?;
        write_header(&mut out, &fname, e, &content)?;
    }

    if !ar.is_data || ar.sig_flags != 0 {
        let mut sig_flags = ar.sig_flags;
        let sig = crate::signature::create(&mut sig_flags, &out, private_key, &fname)
            .map_err(|e| format!("phar error: unable to write signature to tar-based phar: {e}"))?;
        ar.sig_flags = sig_flags;
        ar.signature = Some(crate::util::hex_upper(&sig));
        let mut content = Vec::with_capacity(sig.len() + 8);
        put32(&mut content, sig_flags);
        put32(&mut content, to_u32(sig.len()));
        content.extend_from_slice(&sig);
        let mut e = magic_entry(b".phar/signature.bin", &content, ENT_PERM_DEF_FILE, now);
        write_header(&mut out, &fname, &mut e, &content)?;
    }
    out.extend_from_slice(&[0u8; 1024]);
    ar.is_brandnew = false;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(tar_number(b"  0000644\0"), 0o644);
        assert_eq!(tar_size(b"00000000012\0"), Some(10));
        assert_eq!(tar_size(b"0000000001x\0"), None);
        assert_eq!(tar_size(b"\x80\0\0\0"), None);
        let mut b = [0u8; 3];
        assert!(tar_octal(&mut b, 0o17));
        assert_eq!(&b, b"017");
        assert!(!tar_octal(&mut b, 0o7777));
        assert_eq!(&b, b"777");
    }
}
