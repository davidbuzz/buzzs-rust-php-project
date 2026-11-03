//! Zip-based archives (`zip.c`): PHP's own zip reader (end of central
//! directory search, central directory, local header checks, extra fields,
//! the `.phar/*` magic files) and writer.

use std::rc::Rc;

use crate::archive::{Archive, Entry, FpType, Metadata};
use crate::consts::{
    ENT_COMPRESSED_BZ2, ENT_COMPRESSED_GZ, ENT_COMPRESSION_MASK, ENT_PERM_DEF_DIR,
    ENT_PERM_DEF_FILE, ENT_PERM_MASK, HALT_TOKEN, SIG_SHA256, STUB_END_SEQUENCE, ZIP_NEWSTUB,
};
use crate::util::{find_ci, get16, get32, put16, put32, s, to_u32};

/// `phar_zip_d2u_time()`: DOS date/time to a Unix timestamp (local time).
pub fn d2u_time(dtime: u16, ddate: u16) -> u32 {
    let (dtime, ddate) = (i32::from(dtime), i32::from(ddate));
    let tm = crate::sys::Tm {
        year: ((ddate >> 9) & 127) + 1980 - 1900,
        mon: ((ddate >> 5) & 15) - 1,
        mday: ddate & 31,
        hour: (dtime >> 11) & 31,
        min: (dtime >> 5) & 63,
        sec: (dtime << 1) & 62,
    };
    // Stored in a uint32_t by the caller.
    (crate::sys::mktime_from_now(tm) & 0xFFFF_FFFF) as u32
}

/// `phar_zip_u2d_time()`: a Unix timestamp to DOS (time, date).
pub fn u2d_time(t: u32) -> (u16, u16) {
    let tm = crate::sys::localtime(i64::from(t));
    if tm.year >= 80 {
        let cdate = ((tm.year + 1900 - 1980) << 9) + ((tm.mon + 1) << 5) + tm.mday;
        let ctime = (tm.hour << 11) + (tm.min << 5) + (tm.sec >> 1);
        // Truncation to 16 bits as in the C code.
        ((ctime & 0xFFFF) as u16, (cdate & 0xFFFF) as u16)
    } else {
        (0, (1 << 5) + 1)
    }
}

/// `phar_find_eocd()`.
fn find_eocd(buf: &[u8]) -> Option<usize> {
    if buf.len() < 22 {
        return None;
    }
    let mut end = buf.len() + 4 - 22;
    loop {
        let start = (0..end.saturating_sub(3))
            .rev()
            .find(|&i| &buf[i..i + 4] == b"PK\x05\x06")?;
        let comment_len = usize::from(get16(buf, start + 20));
        if start + 22 + comment_len == buf.len() {
            return Some(start);
        }
        end = start;
        if end == 0 {
            return None;
        }
    }
}

/// `phar_zip_process_extra()`: reads the `nu` (unix3) permissions and `UT`
/// timestamp extra fields.
fn process_extra(extra: &[u8], e: &mut Entry) -> bool {
    let mut p = 0usize;
    let mut len = extra.len();
    while len > 0 {
        if len < 4 {
            return false;
        }
        let tag = [extra[p], extra[p + 1]];
        let header_size = usize::from(get16(extra, p + 2));
        p += 4;
        len -= 4;
        if header_size > len {
            return false;
        }
        if &tag == b"UT" && header_size >= 5 {
            let flags = extra[p];
            if flags & 1 != 0 {
                e.timestamp = get32(extra, p + 1);
            }
            p += header_size;
            len -= header_size;
            continue;
        }
        if &tag != b"nu" {
            p += header_size;
            len -= header_size;
            continue;
        }
        let unix3_size = 14;
        let mut field_size = header_size;
        if field_size == unix3_size - 4 {
            field_size = unix3_size;
        }
        if field_size < unix3_size || field_size > len {
            return false;
        }
        if p + unix3_size > extra.len() {
            return false;
        }
        let perms = u32::from(get16(extra, p + 4));
        p += field_size;
        len -= field_size;
        e.flags &= ENT_COMPRESSION_MASK;
        e.flags |= perms & ENT_PERM_MASK;
    }
    true
}

/// A parsed zip, before registration.
#[derive(Debug)]
pub struct ParsedZip {
    /// The archive.
    pub archive: Archive,
    /// The alias from `.phar/alias.txt`.
    pub actual_alias: Option<Vec<u8>>,
}

/// `phar_parse_zipfile()`.
#[allow(clippy::too_many_lines)]
pub fn parse(image: Rc<Vec<u8>>, fname: &[u8], require_hash: bool) -> Result<ParsedZip, String> {
    let data: &[u8] = &image;
    let f = s(fname);
    let fail = |msg: &str| Err(format!("phar error: {msg} in zip-based phar \"{f}\""));
    let window = 22 + 65536;
    let base = data.len().saturating_sub(window);
    let buf = &data[base..];
    if buf.is_empty() {
        return Err(format!(
            "phar error: unable to read in data to search for end of central directory in zip-based phar \"{f}\""
        ));
    }
    let Some(eocd) = find_eocd(buf) else {
        return Err(format!(
            "phar error: end of central directory not found in zip-based phar \"{f}\""
        ));
    };
    let loc = &buf[eocd..eocd + 22];
    if get16(loc, 6) != 0 || get16(loc, 4) != 0 {
        return Err(format!(
            "phar error: split archives spanning multiple zips cannot be processed in zip-based phar \"{f}\""
        ));
    }
    if get16(loc, 8) != get16(loc, 10) {
        return Err(format!(
            "phar error: corrupt zip archive, conflicting file count in end of central directory record in zip-based phar \"{f}\""
        ));
    }
    let count = get16(loc, 10);
    let comment_len = usize::from(get16(loc, 20));
    let comment = &buf[eocd + 22..eocd + 22 + comment_len];
    let mut ar = Archive::new(fname.to_vec());
    ar.is_brandnew = false;
    ar.is_writeable = false;
    ar.is_zip = true;
    ar.version = String::new();
    ar.metadata = Metadata::from_serialized(comment);
    let cdir_offset = get32(loc, 16) as usize;
    let mut pos = cdir_offset;
    let mut actual_alias: Option<Vec<u8>> = None;
    for i in 0..count {
        let beforeus = pos;
        let Some(c) = data.get(pos..pos + 46) else {
            return fail("unable to read central directory entry, truncated");
        };
        pos += 46;
        if &c[..4] != b"PK\x01\x02" {
            return fail("corrupted central directory entry, no magic signature");
        }
        let mut e = Entry {
            is_zip: true,
            fp_type: FpType::Fp,
            compressed_filesize: get32(c, 20),
            uncompressed_filesize: get32(c, 24),
            crc32: get32(c, 16),
            timestamp: d2u_time(get16(c, 12), get16(c, 14)),
            flags: ENT_PERM_DEF_FILE,
            header_offset: u64::from(get32(c, 42)),
            ..Entry::default()
        };
        if get16(c, 8) & 1 != 0 {
            return fail("Cannot process encrypted zip files");
        }
        let fnlen = usize::from(get16(c, 28));
        if fnlen == 0 {
            return fail("Cannot process zips created from stdin (zero-length filename)");
        }
        let Some(name) = data.get(pos..pos + fnlen) else {
            return fail("unable to read in filename from central directory, truncated");
        };
        pos += fnlen;
        let mut name = name.to_vec();
        if name.last() == Some(&b'/') {
            e.is_dir = true;
            if name.len() > 1 {
                name.pop();
            }
            e.flags |= ENT_PERM_DEF_DIR;
        }
        let ho = usize::try_from(e.header_offset).unwrap_or(usize::MAX);
        let mut local_fields = None;
        if e.is_dir {
            e.offset = (ho + 30 + name.len() + usize::from(get16(c, 30))) as u64;
        } else {
            let Some(local) = data.get(ho..ho.saturating_add(30)) else {
                return fail("phar error: internal corruption (cannot read local file header)");
            };
            if name.len() != usize::from(get16(local, 26)) {
                return fail(
                    "phar error: internal corruption (local file header does not match central directory)",
                );
            }
            local_fields = Some((get32(local, 14), get32(local, 18), get32(local, 22)));
            e.offset = (ho + 30 + name.len() + usize::from(get16(local, 28))) as u64;
        }
        e.offset_abs = e.offset;
        if name == b".phar/signature.bin" {
            if e.uncompressed_filesize > 0x10000 {
                return fail("signatures larger than 64 KiB are not supported");
            }
            let mut hashed = data[..ho.min(data.len())].to_vec();
            hashed.extend_from_slice(&data[cdir_offset.min(beforeus)..beforeus]);
            hashed.extend_from_slice(comment);
            let off = usize::try_from(e.offset).unwrap_or(usize::MAX);
            let sig = data.get(off..off.saturating_add(e.uncompressed_filesize as usize));
            let Some(sig) = sig.filter(|s| s.len() > 8) else {
                return fail("signature cannot be read");
            };
            let sig_flags = get32(sig, 0);
            ar.sig_flags = sig_flags;
            match crate::signature::verify(sig_flags, &hashed, &sig[8..], fname) {
                Ok(hex) => ar.signature = Some(hex),
                Err(err) => {
                    let mut m = format!("signature cannot be verified: {err}");
                    // snprintf into a 128-byte buffer
                    if m.len() > 127 {
                        let mut cut = 127;
                        while !m.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        m.truncate(cut);
                    }
                    return fail(&m);
                }
            }
            if i != count - 1 {
                return fail("entries exist after signature, invalid phar");
            }
            continue;
        }
        ar.add_virtual_dirs(&name);
        let extra_len = usize::from(get16(c, 30));
        if extra_len > 0 {
            let extra = data.get(pos..pos + extra_len).unwrap_or_default();
            if extra.len() != extra_len || !process_extra(extra, &mut e) {
                return fail("Unable to process extra field header for file in central directory");
            }
            pos += extra_len;
        }
        match get16(c, 10) {
            0 => {}
            8 => e.flags |= ENT_COMPRESSED_GZ,
            12 => e.flags |= ENT_COMPRESSED_BZ2,
            1 => return fail("unsupported compression method (Shrunk) used in this zip"),
            2..=5 => return fail("unsupported compression method (Reduce) used in this zip"),
            6 => return fail("unsupported compression method (Implode) used in this zip"),
            7 => return fail("unsupported compression method (Tokenize) used in this zip"),
            9 => return fail("unsupported compression method (Deflate64) used in this zip"),
            10 => {
                return fail(
                    "unsupported compression method (PKWare Implode/old IBM TERSE) used in this zip",
                )
            }
            14 => return fail("unsupported compression method (LZMA) used in this zip"),
            18 => return fail("unsupported compression method (IBM TERSE) used in this zip"),
            19 => return fail("unsupported compression method (IBM LZ77) used in this zip"),
            97 => return fail("unsupported compression method (WavPack) used in this zip"),
            98 => return fail("unsupported compression method (PPMd) used in this zip"),
            _ => return fail("unsupported compression method (unknown) used in this zip"),
        }
        let clen = usize::from(get16(c, 32));
        if clen > 0 {
            let Some(cm) = data.get(pos..pos + clen) else {
                return fail("unable to read in file comment, truncated");
            };
            e.metadata = Metadata::from_serialized(cm);
            pos += clen;
        }
        if actual_alias.is_none() && name == b".phar/alias.txt" {
            let (lcrc, lcomp, luncomp) = local_fields.unwrap_or_default();
            if e.crc32 != lcrc
                || e.uncompressed_filesize != luncomp
                || e.compressed_filesize != lcomp
            {
                return fail("phar error: internal corruption of zip-based phar (local header of alias does not match central directory)");
            }
            let off = usize::try_from(e.offset).unwrap_or(usize::MAX).min(data.len());
            let want = e.uncompressed_filesize as usize;
            let raw = if e.flags & ENT_COMPRESSED_GZ != 0 {
                let end = off.saturating_add(e.compressed_filesize as usize).min(data.len());
                crate::compress::inflate_raw(&data[off..end]).unwrap_or_default()
            } else if e.flags & ENT_COMPRESSED_BZ2 != 0 {
                let end = off.saturating_add(e.compressed_filesize as usize).min(data.len());
                crate::bz2::decompress(&data[off..end]).0
            } else {
                data[off..off.saturating_add(want).min(data.len())].to_vec()
            };
            let a: Vec<u8> = raw.into_iter().take(want).collect();
            if a.is_empty() {
                return fail("unable to read in alias, truncated");
            }
            e.uncompressed_filesize = to_u32(a.len());
            actual_alias = Some(a);
        }
        e.inode = crate::util::entry_inode(fname, &name);
        e.filename.clone_from(&name);
        ar.manifest.add(name, e);
    }
    ar.is_data = !ar.manifest.contains_key(b".phar/stub.php");
    if !ar.is_data && require_hash && ar.signature.is_none() {
        return fail("signature is missing");
    }
    ar.fp = Some(image);
    Ok(ParsedZip {
        archive: ar,
        actual_alias,
    })
}

/// The two halves of a zip being written.
struct Pass {
    filefp: Vec<u8>,
    centralfp: Vec<u8>,
}

/// `phar_zip_changed_apply_int()` for one entry.
#[allow(clippy::too_many_lines)]
fn write_entry(
    pass: &mut Pass,
    ar: &mut Archive,
    key: Option<&[u8]>,
    standalone: Option<&mut Entry>,
) -> Result<(), String> {
    let f = s(&ar.fname);
    // Fetch the entry and its content source.
    let mut owned;
    let e: &mut Entry = if let Some(e) = standalone {
        e
    } else {
        let key = key.unwrap_or_default();
        let Some(e) = ar.manifest.get(key).cloned() else {
            return Ok(());
        };
        if e.is_mounted {
            return Ok(());
        }
        if e.is_deleted {
            if e.fp_refcount == 0 && e.fileinfo_lock_count == 0 {
                ar.manifest.remove(key);
            }
            return Ok(());
        }
        ar.add_virtual_dirs(key);
        owned = e;
        &mut owned
    };
    let name = s(&e.filename);
    let mut local = [0u8; 30];
    let mut central = [0u8; 46];
    local[..4].copy_from_slice(b"PK\x03\x04");
    central[..4].copy_from_slice(b"PK\x01\x02");
    central[30..32].copy_from_slice(&18u16.to_le_bytes());
    local[28..30].copy_from_slice(&18u16.to_le_bytes());
    let mut perms = Vec::with_capacity(18);
    perms.extend_from_slice(b"nu");
    put16(&mut perms, 14);
    let pbytes = ((e.flags & ENT_PERM_MASK) as u16).to_le_bytes();
    put32(&mut perms, php_hash::crc32(&pbytes));
    perms.extend_from_slice(&pbytes);
    perms.extend_from_slice(&[0u8; 8]);
    let method: u16 = if e.flags & ENT_COMPRESSED_BZ2 != 0 {
        12
    } else if e.flags & ENT_COMPRESSED_GZ != 0 {
        8
    } else {
        0
    };
    central[10..12].copy_from_slice(&method.to_le_bytes());
    local[8..10].copy_from_slice(&method.to_le_bytes());
    let (t, d) = u2d_time(e.timestamp);
    for h in [&mut local[10..14], &mut central[12..16]] {
        h[..2].copy_from_slice(&t.to_le_bytes());
        h[2..].copy_from_slice(&d.to_le_bytes());
    }
    let fnlen = to_u32(e.filename.len() + usize::from(e.is_dir)) as u16;
    central[28..30].copy_from_slice(&fnlen.to_le_bytes());
    local[26..28].copy_from_slice(&fnlen.to_le_bytes());
    central[8..10].copy_from_slice(&0x0800u16.to_le_bytes());
    local[6..8].copy_from_slice(&0x0800u16.to_le_bytes());
    central[42..46].copy_from_slice(&to_u32(pass.filefp.len()).to_le_bytes());

    let set_sizes = |local: &mut [u8; 30], central: &mut [u8; 46], comp: u32, uncomp: u32| {
        central[20..24].copy_from_slice(&comp.to_le_bytes());
        local[18..22].copy_from_slice(&comp.to_le_bytes());
        central[24..28].copy_from_slice(&uncomp.to_le_bytes());
        local[22..26].copy_from_slice(&uncomp.to_le_bytes());
    };
    let mut not_really_modified = false;
    let mut payload: Option<Vec<u8>> = None;
    let mut is_dir_continue = false;
    if e.is_modified {
        if e.is_dir {
            e.is_modified = false;
            if e.fp_type == FpType::Mod {
                e.fp_type = FpType::Fp;
                e.data = None;
            }
            is_dir_continue = true;
        } else {
            let content = if let Some(k) = key {
                ar.open_entry_fp(k, false).map_err(|_| {
                    format!("unable to open file contents of file \"{name}\" in zip-based phar \"{f}\"")
                })?;
                ar.read_entry_raw(k)
            } else {
                e.data.as_ref().map(|d| d.borrow().clone()).unwrap_or_default()
            };
            if e.flags & ENT_COMPRESSION_MASK != 0 && (e.old_flags == e.flags || e.old_flags == 0) {
                not_really_modified = true;
                set_sizes(&mut local, &mut central, e.compressed_filesize, e.uncompressed_filesize);
            } else {
                let content = &content[..(e.uncompressed_filesize as usize).min(content.len())];
                e.crc32 = php_hash::crc32(content);
                if e.flags & ENT_COMPRESSION_MASK == 0 {
                    e.compressed_filesize = e.uncompressed_filesize;
                    set_sizes(&mut local, &mut central, e.uncompressed_filesize, e.uncompressed_filesize);
                    payload = Some(content.to_vec());
                } else {
                    let c = if e.flags & ENT_COMPRESSED_GZ != 0 {
                        crate::compress::deflate_raw(content)
                    } else {
                        crate::compress::bzip2(content)
                    };
                    let Some(c) = c else {
                        return Err(format!(
                            "unable to {} compress file \"{name}\" to zip-based phar \"{f}\"",
                            if e.flags & ENT_COMPRESSED_GZ != 0 { "gzip" } else { "bzip2" }
                        ));
                    };
                    e.compressed_filesize = to_u32(c.len());
                    set_sizes(&mut local, &mut central, e.compressed_filesize, e.uncompressed_filesize);
                    e.old_flags = e.flags;
                    e.is_modified = true;
                    payload = Some(c);
                }
            }
        }
    } else {
        set_sizes(&mut local, &mut central, e.compressed_filesize, e.uncompressed_filesize);
    }
    if !is_dir_continue {
        central[16..20].copy_from_slice(&e.crc32.to_le_bytes());
        local[14..18].copy_from_slice(&e.crc32.to_le_bytes());
    }
    if e.metadata.has_data() {
        central[32..34].copy_from_slice(&(e.metadata.bytes().len() as u16).to_le_bytes());
    }
    e.header_offset = pass.filefp.len() as u64;
    let offset = e.header_offset + 30 + e.filename.len() as u64 + u64::from(e.is_dir) + 18;
    pass.filefp.extend_from_slice(&local);
    pass.centralfp.extend_from_slice(&central);
    pass.filefp.extend_from_slice(&e.filename);
    pass.centralfp.extend_from_slice(&e.filename);
    if e.is_dir {
        pass.filefp.push(b'/');
        pass.centralfp.push(b'/');
    }
    pass.filefp.extend_from_slice(&perms);
    pass.centralfp.extend_from_slice(&perms);
    if !not_really_modified && e.is_modified {
        if let Some(p) = payload {
            pass.filefp.extend_from_slice(&p);
        }
        e.is_modified = false;
    } else {
        e.is_modified = false;
        if !e.is_dir && e.compressed_filesize > 0 {
            let old = ar.open_archive_fp();
            let start = usize::try_from(e.offset_abs).unwrap_or(usize::MAX);
            let bytes = old.as_ref().and_then(|o| {
                o.get(start..start.checked_add(e.compressed_filesize as usize)?)
                    .map(<[u8]>::to_vec)
            });
            let Some(bytes) = bytes else {
                return Err(format!(
                    "unable to copy contents of file \"{name}\" while creating zip-based phar \"{f}\""
                ));
            };
            pass.filefp.extend_from_slice(&bytes);
        }
    }
    e.data = None;
    e.offset = offset;
    e.offset_abs = offset;
    e.fp_type = FpType::Fp;
    if e.metadata.has_data() {
        pass.centralfp.extend_from_slice(e.metadata.bytes());
    }
    if let Some(k) = key {
        let updated = e.clone();
        if let Some(slot) = ar.manifest.get_mut(k) {
            *slot = updated;
        }
    }
    Ok(())
}

/// A magic `.phar/...` entry for a zip.
fn magic_entry(name: &[u8], content: &[u8], timestamp: u32) -> Entry {
    let mut e = Entry {
        filename: name.to_vec(),
        flags: ENT_PERM_DEF_FILE,
        timestamp,
        is_zip: true,
        is_crc_checked: true,
        ..Entry::default()
    };
    e.set_content(content.to_vec());
    e
}

/// The stub/alias handling at the start of `phar_zip_flush()`. Split out
/// because the registry re-registers the alias in between.
pub fn prepare_alias(ar: &mut Archive) {
    if ar.is_data {
        return;
    }
    if !ar.is_temporary_alias && ar.alias_len() > 0 {
        let alias = ar.alias_bytes().to_vec();
        let e = magic_entry(b".phar/alias.txt", &alias, crate::sys::now32());
        ar.manifest.update(b".phar/alias.txt".to_vec(), e);
    } else {
        ar.manifest.remove(b".phar/alias.txt");
    }
}

/// `phar_zip_flush()` after [`prepare_alias`]: returns the new image.
#[allow(clippy::too_many_lines)]
pub fn flush(
    ar: &mut Archive,
    user_stub: Option<&[u8]>,
    is_default_stub: bool,
    private_key: &[u8],
) -> Result<Vec<u8>, String> {
    let f = s(&ar.fname);
    let now = crate::sys::now32();
    if !ar.is_data {
        match user_stub {
            Some(stub) if !is_default_stub => {
                let Some(pos) = find_ci(stub, HALT_TOKEN) else {
                    return Err(format!("illegal stub for zip-based phar \"{f}\""));
                };
                let mut content = stub[..pos + HALT_TOKEN.len()].to_vec();
                content.extend_from_slice(STUB_END_SEQUENCE);
                let mut e = magic_entry(b".phar/stub.php", &content, now);
                e.compressed_filesize = 0;
                ar.manifest.update(b".phar/stub.php".to_vec(), e);
            }
            _ => {
                let e = magic_entry(b".phar/stub.php", ZIP_NEWSTUB, now);
                if is_default_stub {
                    ar.manifest.update(b".phar/stub.php".to_vec(), e);
                } else {
                    ar.manifest.add(b".phar/stub.php".to_vec(), e);
                }
            }
        }
    }
    let mut pass = Pass {
        filefp: Vec::new(),
        centralfp: Vec::new(),
    };
    if !ar.is_data && ar.sig_flags == 0 {
        ar.sig_flags = SIG_SHA256;
    }
    let n = ar.manifest.len() + usize::from(ar.sig_flags != 0);
    let count = u16::try_from(n).unwrap_or(u16::MAX);
    ar.virtual_dirs.clear();
    for key in ar.manifest.keys() {
        write_entry(&mut pass, ar, Some(&key), None)
            .map_err(|e| format!("phar zip flush of \"{f}\" failed: {e}"))?;
    }
    let meta = ar.metadata.bytes().to_vec();
    if !ar.is_data || ar.sig_flags != 0 {
        let mut hashed = pass.filefp.clone();
        hashed.extend_from_slice(&pass.centralfp);
        hashed.extend_from_slice(&meta);
        let mut sig_flags = ar.sig_flags;
        let sig = crate::signature::create(&mut sig_flags, &hashed, private_key, &ar.fname)
            .map_err(|e| {
                format!(
                    "phar zip flush of \"{f}\" failed: phar error: unable to write signature to zip-based phar: {e}"
                )
            })?;
        ar.sig_flags = sig_flags;
        ar.signature = Some(crate::util::hex_upper(&sig));
        let mut content = Vec::with_capacity(sig.len() + 8);
        put32(&mut content, sig_flags);
        put32(&mut content, to_u32(sig.len()));
        content.extend_from_slice(&sig);
        let mut e = Entry {
            filename: b".phar/signature.bin".to_vec(),
            ..Entry::default()
        };
        e.set_content(content);
        write_entry(&mut pass, ar, None, Some(&mut e))
            .map_err(|e| format!("phar zip flush of \"{f}\" failed: {e}"))?;
    }
    let cdir_size = to_u32(pass.centralfp.len());
    let cdir_offset = to_u32(pass.filefp.len());
    let mut out = pass.filefp;
    out.extend_from_slice(&pass.centralfp);
    out.extend_from_slice(b"PK\x05\x06");
    put16(&mut out, 0);
    put16(&mut out, 0);
    put16(&mut out, count);
    put16(&mut out, count);
    put32(&mut out, cdir_size);
    put32(&mut out, cdir_offset);
    put16(&mut out, meta.len() as u16);
    out.extend_from_slice(&meta);
    ar.is_brandnew = false;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dos_time() {
        let (t, d) = u2d_time(0);
        assert_eq!((t, d), (0, 33));
        let ts = 1_300_000_000u32;
        let (t, d) = u2d_time(ts);
        let back = d2u_time(t, d);
        assert!(back.abs_diff(ts) <= 3601);
    }

    #[test]
    fn eocd() {
        let mut b = vec![0u8; 10];
        b.extend_from_slice(b"PK\x05\x06");
        b.extend_from_slice(&[0u8; 16]);
        b.extend_from_slice(&3u16.to_le_bytes());
        b.extend_from_slice(b"abc");
        assert_eq!(find_eocd(&b), Some(10));
        b.push(0);
        assert_eq!(find_eocd(&b), None);
    }
}
