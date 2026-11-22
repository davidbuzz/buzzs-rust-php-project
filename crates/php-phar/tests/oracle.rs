// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Oracle tests against PHP's own phar implementation.
//!
//! Each `tests/fixtures/<case>.recipe` is a small script (one operation
//! per line) that `tests/oracle/build.php` executes with real PHP and
//! [`run_recipe`] executes with this crate:
//!
//! ```text
//! new phar|data <file> [alias=<a>] [format=zip|tar]
//! add <name> <content>        dir <name>          stub <content>
//! defaultstub [<index> <web>] meta <serialized>   filemeta <name> <serialized>
//! sig md5|sha1|sha256|sha512  compressfiles gz|bz2  decompressfiles
//! fcompress <name> gz|bz2     compress gz|bz2|none  delete <name>
//! unset <name>                chmod <name> <octal>  setalias <alias>
//! buffer start|stop           copy <from> <to>      convert data|exec phar|tar|zip [gz|bz2|none]
//! ```
//!
//! Arguments may be `hex:`-encoded. `regenerate_fixtures` (ignored; needs
//! `/usr/bin/php8.5`) stores PHP's archive, the build time and PHP's listing
//! under `tests/fixtures/<case>/`. The regular tests then check that
//! (a) our reader lists PHP's archive exactly like PHP does, and
//! (b) our writer, replaying the recipe at the same clock time, produces
//! the very same bytes.

#![allow(clippy::format_push_string, clippy::too_many_lines)]
use std::path::{Path, PathBuf};
use std::process::Command;

use php_phar::object::{MetadataGet, PharFileInfo, PharObject};
use php_phar::registry;
use php_streams::wrapper::Wrapper;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn oracle_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/oracle")
}

fn arg(s: &str) -> Vec<u8> {
    match s.strip_prefix("hex:") {
        Some(h) => (0..h.len() / 2)
            .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap())
            .collect(),
        None => s.as_bytes().to_vec(),
    }
}

fn skip_write(case: &str) -> bool {
    std::fs::read_to_string(fixtures().join(format!("{case}.recipe")))
        .unwrap_or_default()
        .lines()
        .any(|l| l.starts_with("# skip-write"))
}

fn recipes() -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(fixtures()).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "recipe") {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            let lines = std::fs::read_to_string(&p)
                .unwrap()
                .lines()
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_string)
                .collect();
            out.push((name, lines));
        }
    }
    out.sort();
    out
}

/// Whether the final archive of a recipe is a `PharData`.
fn final_is_data(lines: &[String]) -> bool {
    let mut data = false;
    for l in lines {
        let w: Vec<&str> = l.split(' ').collect();
        match w[0] {
            "new" | "convert" => data = w[1] == "data",
            _ => {}
        }
    }
    data
}

fn php(args: &[&str]) -> std::process::Output {
    Command::new("/usr/bin/php8.5")
        .args(["-d", "memory_limit=1G"])
        .args(args)
        .output()
        .expect("run php")
}

fn metadata_value(serialized: &[u8]) -> php_runtime::Value {
    let mut ctx = php_runtime::CollectingContext::default();
    php_runtime::serialize::unserialize(
        serialized,
        &php_runtime::serialize::UnserializeOptions::default(),
        &mut ctx,
    )
    .unwrap()
    .unwrap()
}

/// Replays a recipe with this crate inside `dir`; returns the final path.
#[allow(clippy::many_single_char_names)]
fn run_recipe(lines: &[String], dir: &Path) -> Vec<u8> {
    let mut obj = PharObject::new(false);
    let mut path = Vec::new();
    let comp = |s: &str| match s {
        "gz" => 0x1000,
        "bz2" => 0x2000,
        _ => 0,
    };
    let fmt = |s: &str| match s {
        "phar" => 1,
        "tar" => 2,
        _ => 3,
    };
    let reopen = |obj: &mut PharObject, c: php_phar::object::Converted| -> Vec<u8> {
        let mut n = PharObject::new(c.is_data);
        n.construct(&c.fname, None, 0).unwrap();
        obj.release();
        *obj = n;
        c.fname
    };
    for l in lines {
        let w: Vec<&str> = l.split(' ').collect();
        let a = |i: usize| arg(w.get(i).copied().unwrap_or(""));
        match w[0] {
            "new" => {
                let mut p = dir.to_string_lossy().as_bytes().to_vec();
                p.push(b'/');
                p.extend_from_slice(w[2].as_bytes());
                let mut alias = None;
                let mut format = 0;
                for o in &w[3..] {
                    let (k, v) = o.split_once('=').unwrap();
                    if k == "alias" {
                        alias = Some(arg(v));
                    }
                    if k == "format" {
                        format = fmt(v);
                    }
                }
                obj = PharObject::new(w[1] == "data");
                obj.construct(&p, alias.as_deref(), format).unwrap();
                path = p;
            }
            "add" => obj.offset_set(&a(1), &a(2)).unwrap(),
            "dir" => obj.add_empty_dir(&a(1)).unwrap(),
            "stub" => {
                obj.set_stub(&a(1)).unwrap();
            }
            "defaultstub" => {
                if w.len() > 1 {
                    obj.set_default_stub(Some(&a(1)), Some(&a(2))).unwrap();
                } else {
                    obj.set_default_stub(None, None).unwrap();
                }
            }
            "meta" => {
                let s = a(1);
                obj.set_metadata(metadata_value(&s), s).unwrap();
            }
            "filemeta" => {
                let s = a(2);
                let info_path = obj.offset_get(&a(1)).unwrap();
                let mut fi = PharFileInfo::new();
                fi.construct(&info_path).unwrap();
                fi.set_metadata(metadata_value(&s), s).unwrap();
                fi.release();
            }
            "delmeta" => {
                obj.del_metadata().unwrap();
            }
            "sig" => {
                let algo = match w[1] {
                    "md5" => 1,
                    "sha1" => 2,
                    "sha256" => 3,
                    _ => 4,
                };
                obj.set_signature_algorithm(algo, None).unwrap();
            }
            "compressfiles" => obj.compress_files(comp(w[1])).unwrap(),
            "decompressfiles" => {
                obj.decompress_files().unwrap();
            }
            "fcompress" | "fdecompress" | "chmod" => {
                let info_path = obj.offset_get(&a(1)).unwrap();
                let mut fi = PharFileInfo::new();
                fi.construct(&info_path).unwrap();
                match w[0] {
                    "fcompress" => {
                        fi.compress(comp(w[2])).unwrap();
                    }
                    "fdecompress" => {
                        fi.decompress().unwrap();
                    }
                    _ => fi.chmod(i64::from_str_radix(w[2], 8).unwrap()).unwrap(),
                }
                fi.release();
            }
            "compress" => {
                let c = obj.compress(comp(w[1]), None).unwrap();
                path = reopen(&mut obj, c);
            }
            "decompress" => {
                let c = obj.decompress(None).unwrap();
                path = reopen(&mut obj, c);
            }
            "delete" => {
                obj.delete(&a(1)).unwrap();
            }
            "unset" => obj.offset_unset(&a(1)).unwrap(),
            "setalias" => {
                obj.set_alias(&a(1)).unwrap();
            }
            "buffer" => {
                if w[1] == "start" {
                    obj.start_buffering().unwrap();
                } else {
                    obj.stop_buffering().unwrap();
                }
            }
            "copy" => {
                obj.copy(&a(1), &a(2)).unwrap();
            }
            "convert" => {
                let m = w.get(3).map(|m| comp(m));
                let c = if w[1] == "data" {
                    obj.convert_to_data(Some(fmt(w[2])), m, None).unwrap()
                } else {
                    obj.convert_to_executable(Some(fmt(w[2])), m, None).unwrap()
                };
                path = reopen(&mut obj, c);
            }
            other => panic!("unknown op {other}"),
        }
    }
    obj.release();
    path
}

fn var_export_str(s: &[u8]) -> String {
    let mut out = String::from("'");
    for &c in s {
        match c {
            b'\'' => out.push_str("\\'"),
            b'\\' => out.push_str("\\\\"),
            _ => out.push(char::from(c)),
        }
    }
    out.push('\'');
    out
}

fn md5hex(b: &[u8]) -> String {
    php_hash::bin2hex(&php_hash::md5(b))
}

fn meta_string(m: &MetadataGet) -> String {
    match m {
        MetadataGet::None => "NONE".into(),
        MetadataGet::Serialized(s) => {
            // PHP prints serialize(getMetadata()): unserialize, then serialize
            let mut ctx = php_runtime::CollectingContext::default();
            let v = php_runtime::serialize::unserialize(
                s,
                &php_runtime::serialize::UnserializeOptions::default(),
                &mut ctx,
            )
            .ok()
            .flatten()
            .unwrap_or(php_runtime::Value::Bool(false));
            String::from_utf8_lossy(&php_runtime::serialize::serialize(&v, &mut ctx).unwrap())
                .into_owned()
        }
        MetadataGet::Value(v) => {
            let mut ctx = php_runtime::CollectingContext::default();
            String::from_utf8_lossy(&php_runtime::serialize::serialize(v, &mut ctx).unwrap())
                .into_owned()
        }
    }
}

/// The Rust counterpart of `tests/oracle/list.php`.
#[allow(
    clippy::format_push_string,
    clippy::items_after_statements,
    clippy::too_many_lines
)]
fn listing(path: &[u8], is_data: bool) -> String {
    let mut out = String::new();
    let mut p = PharObject::new(is_data);
    if let Err(e) = p.construct(path, None, 0) {
        return format!("EXCEPTION {}: {}\n", e.class.name(), e.message);
    }
    out += &format!("version: {}\n", p.get_version().unwrap());
    out += &format!(
        "alias: {}\n",
        p.get_alias()
            .unwrap()
            .map_or("NULL".to_string(), |a| var_export_str(&a))
    );
    out += &format!("count: {}\n", p.count().unwrap());
    out += &format!(
        "signature: {}\n",
        p.get_signature()
            .unwrap()
            .map_or("none".to_string(), |s| format!(
                "{} {}",
                s.hash_type,
                String::from_utf8_lossy(&s.hash)
            ))
    );
    out += &format!(
        "compressed: {}\n",
        p.is_compressed()
            .unwrap()
            .map_or("false".to_string(), |c| c.to_string())
    );
    out += &format!(
        "format: {}\n",
        if p.is_file_format(3).unwrap() {
            "zip"
        } else if p.is_file_format(2).unwrap() {
            "tar"
        } else {
            "phar"
        }
    );
    out += &format!(
        "metadata: {}\n",
        meta_string(&p.get_metadata(false).unwrap())
    );
    let stub = p.get_stub().unwrap();
    out += &format!("stub: {} {}\n", md5hex(&stub), stub.len());
    let mut prefix = b"phar://".to_vec();
    prefix.extend_from_slice(&p.get_path().unwrap());
    let w = php_phar::wrapper::PharWrapper;
    fn walk(
        w: php_phar::wrapper::PharWrapper,
        dir: &[u8],
        prefix_len: usize,
        out: &mut String,
    ) -> Result<(), php_phar::error::PharError> {
        let Ok(mut d) = w.opendir(dir, 0, None) else {
            return Ok(());
        };
        while let Some(name) = d.readdir() {
            if name.is_empty() {
                // RecursiveDirectoryIterator::valid() stops at an empty d_name
                break;
            }
            if name == b"." || name == b".." {
                continue;
            }
            let mut full = dir.to_vec();
            full.push(b'/');
            full.extend_from_slice(&name);
            let sb = w.url_stat(&full, 0, None).unwrap().unwrap_or_default();
            let is_dir = sb.is_dir();
            let mut fi = PharFileInfo::new();
            fi.construct(&full)?;
            let rel = String::from_utf8_lossy(&full[prefix_len + 1..]).into_owned();
            let mut line = format!(
                "entry {rel} dir={} size={} perms={:o} mtime={} csize={} gz={} bz2={} flags={}",
                i32::from(is_dir),
                sb.size,
                sb.mode,
                sb.mtime,
                fi.get_compressed_size()?,
                i32::from(fi.is_compressed(Some(0x1000))?),
                i32::from(fi.is_compressed(Some(0x2000))?),
                fi.get_phar_flags()?,
            );
            let crc = if !is_dir && fi.is_crc_checked()? {
                (fi.get_crc32()? & 0xFFFF_FFFF).to_string()
            } else {
                "-".into()
            };
            line += &format!(" crc={crc} meta={}", meta_string(&fi.get_metadata(false)?));
            let content = if is_dir {
                "-".to_string()
            } else {
                md5hex(&fi.get_content()?)
            };
            out.push_str(&format!("{line} md5={content}\n"));
            fi.release();
            if is_dir {
                walk(w, &full, prefix_len, out)?;
            }
        }
        Ok(())
    }
    if let Err(e) = walk(w, &prefix, prefix.len(), &mut out) {
        out += &format!("ITER EXCEPTION {}: {}\n", e.class.name(), e.message);
    }
    p.release();
    out
}

fn case_dir(case: &str) -> PathBuf {
    fixtures().join(case)
}

fn fixture_archive(case: &str) -> Option<(PathBuf, i64, String)> {
    let dir = case_dir(case);
    let name = std::fs::read_to_string(dir.join("name")).ok()?;
    let time = std::fs::read_to_string(dir.join("time"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let list = std::fs::read_to_string(dir.join("list.txt")).ok()?;
    Some((dir.join(name.trim()), time, list))
}

fn scratch(case: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("php-phar-oracle-{}-{case}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
#[ignore = "regenerates fixtures with /usr/bin/php8.5"]
fn regenerate_fixtures() {
    for (case, lines) in recipes() {
        let dir = case_dir(&case);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut done = false;
        for _attempt in 0..10 {
            let work = scratch(&case);
            let t0 = php_phar::sys::now();
            let recipe = fixtures().join(format!("{case}.recipe"));
            let o = php(&[
                "-d",
                "phar.readonly=0",
                oracle_dir().join("build.php").to_str().unwrap(),
                recipe.to_str().unwrap(),
                work.to_str().unwrap(),
            ]);
            let t1 = php_phar::sys::now();
            assert!(
                o.status.success(),
                "{case}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            if t0 != t1 {
                continue;
            }
            let built = String::from_utf8(o.stdout).unwrap().trim().to_string();
            let name = Path::new(&built).file_name().unwrap().to_owned();
            std::fs::copy(&built, dir.join(&name)).unwrap();
            std::fs::write(dir.join("name"), name.to_string_lossy().as_bytes()).unwrap();
            std::fs::write(dir.join("time"), t0.to_string()).unwrap();
            let kind = if final_is_data(&lines) {
                "data"
            } else {
                "phar"
            };
            let l = php(&[
                oracle_dir().join("list.php").to_str().unwrap(),
                dir.join(&name).to_str().unwrap(),
                kind,
            ]);
            let list = String::from_utf8_lossy(&l.stdout).replace(dir.to_str().unwrap(), "%DIR%");
            std::fs::write(dir.join("list.txt"), list).unwrap();
            let _ = std::fs::remove_dir_all(&work);
            done = true;
            break;
        }
        assert!(done, "{case}: could not build within one second");
    }
}

#[test]
fn reads_php_archives_like_php() {
    php_phar::stub::set_flavor(php_phar::stub::Flavor::Release);
    let mut failures = Vec::new();
    for (case, lines) in recipes() {
        let Some((archive, _, expected)) = fixture_archive(&case) else {
            continue;
        };
        let ours = listing(archive.to_str().unwrap().as_bytes(), final_is_data(&lines))
            .replace(case_dir(&case).to_str().unwrap(), "%DIR%");
        if ours != expected {
            failures.push(format!("== {case}\n--- php\n{expected}--- ours\n{ours}"));
        }
        registry::reset_request();
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn writes_archives_byte_identical_to_php() {
    php_phar::stub::set_flavor(php_phar::stub::Flavor::Release);
    registry::set_ini("phar.readonly", false, true);
    let mut failures = Vec::new();
    for (case, lines) in recipes() {
        let Some((archive, time, _)) = fixture_archive(&case) else {
            continue;
        };
        if skip_write(&case) {
            continue;
        }
        let work = scratch(&case);
        // Ubuntu's PHP builds (8.5 too) write 0 for entry timestamps (a
        // reproducible-build patch) but the real time for magic tar/zip
        // entries.
        php_phar::sys::set_time_override(Some(time));
        php_phar::sys::set_entry_time_override(Some(0));
        let built = run_recipe(&lines, &work);
        php_phar::sys::set_time_override(None);
        php_phar::sys::set_entry_time_override(None);
        registry::reset_request();
        let ours = std::fs::read(String::from_utf8(built).unwrap()).unwrap();
        let theirs = std::fs::read(&archive).unwrap();
        if ours != theirs {
            if std::env::var_os("PHAR_ORACLE_KEEP").is_some() {
                std::fs::write(case_dir(&case).join("ours.bin"), &ours).unwrap();
            }
            let first = ours.iter().zip(&theirs).position(|(a, b)| a != b);
            failures.push(format!(
                "{case}: {} vs {} bytes, first difference at {first:?}",
                ours.len(),
                theirs.len()
            ));
        }
        let _ = std::fs::remove_dir_all(&work);
    }
    registry::set_ini("phar.readonly", true, true);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[ignore = "runs /usr/bin/php8.5 on archives written by this crate"]
fn php_reads_our_archives() {
    php_phar::stub::set_flavor(php_phar::stub::Flavor::Release);
    php_phar::sys::set_entry_time_override(Some(0));
    registry::set_ini("phar.readonly", false, true);
    let mut failures = Vec::new();
    for (case, lines) in recipes() {
        let work = scratch(&case);
        let built = run_recipe(&lines, &work);
        registry::reset_request();
        let path = String::from_utf8(built).unwrap();
        let kind = if final_is_data(&lines) {
            "data"
        } else {
            "phar"
        };
        let l = php(&[oracle_dir().join("list.php").to_str().unwrap(), &path, kind]);
        registry::set_ini("phar.readonly", true, true);
        let ours = listing(path.as_bytes(), kind == "data");
        registry::set_ini("phar.readonly", false, true);
        registry::reset_request();
        let theirs = String::from_utf8_lossy(&l.stdout).into_owned();
        if ours != theirs {
            failures.push(format!("== {case}\n--- php\n{theirs}--- ours\n{ours}"));
        }
        let _ = std::fs::remove_dir_all(&work);
    }
    registry::set_ini("phar.readonly", true, true);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn php_src_tests() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src/ext/phar/tests")
}

fn php_src_listing_dir() -> PathBuf {
    fixtures().join("php-src")
}

/// The archives shipped with php-src's phar tests (tracked files only).
fn php_src_archives() -> Vec<String> {
    let list = php_src_listing_dir().join("files.txt");
    std::fs::read_to_string(list)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
#[ignore = "regenerates php-src archive listings with /usr/bin/php8.5"]
fn regenerate_php_src_listings() {
    let o = Command::new("git")
        .args(["-C", php_src_tests().to_str().unwrap(), "ls-files", "."])
        .output()
        .unwrap();
    let files: Vec<String> = String::from_utf8(o.stdout)
        .unwrap()
        .lines()
        .filter(|f| {
            [".phar", ".zip", ".tar", ".tgz", ".gz", ".bz2"]
                .iter()
                .any(|e| f.ends_with(e))
        })
        .map(str::to_string)
        .collect();
    let dir = php_src_listing_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("files.txt"), files.join("\n")).unwrap();
    let base = std::fs::canonicalize(php_src_tests()).unwrap();
    for f in &files {
        let path = base.join(f);
        for kind in ["phar", "data"] {
            let l = php(&[
                "-d",
                "display_errors=0",
                "-d",
                "log_errors=0",
                oracle_dir().join("list.php").to_str().unwrap(),
                path.to_str().unwrap(),
                kind,
            ]);
            let text = String::from_utf8_lossy(&l.stdout).replace(base.to_str().unwrap(), "%DIR%");
            std::fs::write(
                dir.join(format!("{}.{kind}.txt", f.replace('/', "__"))),
                text,
            )
            .unwrap();
        }
    }
}

#[test]
fn reads_php_src_archives_like_php() {
    php_phar::stub::set_flavor(php_phar::stub::Flavor::Release);
    registry::set_compression_support(true, true);
    let Ok(base) = std::fs::canonicalize(php_src_tests()) else {
        return;
    };
    let mut failures = Vec::new();
    let mut checked = 0;
    for f in php_src_archives() {
        if f.contains("openssl") {
            continue;
        }
        let path = base.join(&f);
        if !path.exists() {
            continue;
        }
        for kind in ["phar", "data"] {
            let Ok(expected) = std::fs::read_to_string(
                php_src_listing_dir().join(format!("{}.{kind}.txt", f.replace('/', "__"))),
            ) else {
                continue;
            };
            // OpenSSL-signed archives need an ext/openssl hook.
            if expected.contains("signature: OpenSSL") {
                continue;
            }
            let ours = listing(path.to_str().unwrap().as_bytes(), kind == "data")
                .replace(base.to_str().unwrap(), "%DIR%");
            registry::reset_request();
            checked += 1;
            if ours != expected {
                failures.push(format!(
                    "== {f} ({kind})\n--- php\n{expected}--- ours\n{ours}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {checked} differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ------------------------------------------------------------ corruption

fn corrupt_dir() -> PathBuf {
    fixtures().join("corrupt")
}

/// Builds corrupted variants of a few fixture archives.
fn corrupt_variants() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let read = |case: &str, name: &str| std::fs::read(case_dir(case).join(name)).unwrap();
    let find = |h: &[u8], n: &[u8]| h.windows(n.len()).position(|w| w == n).unwrap();
    // phar format: an unsigned copy (signature flag cleared) so that later
    // checks are reachable with phar.require_hash=0
    for (case, name) in [
        ("simple_phar", "simple.phar"),
        ("phar_gz", "gz.phar"),
        ("phar_meta", "meta.phar"),
        ("phar_dirs", "dirs.phar"),
    ] {
        let mut base = read(case, name);
        let halt = find(&base, b"__HALT_COMPILER(); ?>") + 21;
        let flags = halt + 4 + 4 + 2;
        base[flags + 2] &= !1;
        let stem = case.replace('_', "");
        out.push((format!("{stem}-unsigned.phar"), base.clone()));
        let mlen = u32::from_le_bytes(base[halt..halt + 4].try_into().unwrap()) as usize;
        let end = (halt + 4 + mlen + 40).min(base.len());
        for cut in (halt - 3..end).step_by(3) {
            out.push((format!("{stem}-cut{cut}.phar"), base[..cut].to_vec()));
        }
        for pos in halt..(halt + 4 + mlen).min(base.len()) {
            for val in [0x00u8, 0xFF, 0x7F] {
                if base[pos] == val {
                    continue;
                }
                let mut v = base.clone();
                v[pos] = val;
                out.push((format!("{stem}-set{pos}x{val:02x}.phar"), v));
            }
        }
    }
    // tar: truncations and header field damage
    for (case, name) in [("tar_data", "data.tar"), ("tar_exec", "exec.phar.tar")] {
        let base = read(case, name);
        let stem = case.replace('_', "");
        for cut in [
            0,
            100,
            511,
            512,
            700,
            1024,
            1536,
            2048,
            base.len() - 1024,
            base.len() - 512,
        ] {
            out.push((
                format!("{stem}-cut{cut}.{}", name.split_once('.').unwrap().1),
                base[..cut.min(base.len())].to_vec(),
            ));
        }
        for (pos, val) in [
            (148usize, b'9'),
            (124, b'7'),
            (124, 0x80),
            (156, b'K'),
            (156, b'x'),
            (0, 0),
            (257, b'X'),
            (512 + 156, b'1'),
            (512 + 124, b'7'),
        ] {
            let mut v = base.clone();
            v[pos] = val;
            out.push((
                format!(
                    "{stem}-set{pos}x{val:02x}.{}",
                    name.split_once('.').unwrap().1
                ),
                v,
            ));
        }
    }
    // zip: EOCD / central directory damage
    for (case, name) in [
        ("zip_data", "data.zip"),
        ("zip_gz", "gz.zip"),
        ("zip_exec", "exec.phar.zip"),
    ] {
        let base = read(case, name);
        let stem = case.replace('_', "");
        let ext = name.split_once('.').unwrap().1;
        let eocd = base.len() - 22;
        let cdir = u32::from_le_bytes(base[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
        for cut in [10, cdir, cdir + 20, eocd, eocd + 10, base.len() - 1] {
            out.push((format!("{stem}-cut{cut}.{ext}"), base[..cut].to_vec()));
        }
        let fields = [
            eocd + 4,
            eocd + 6,
            eocd + 8,
            eocd + 10,
            eocd + 16,
            eocd + 20,
            cdir,
            cdir + 8,
            cdir + 10,
            cdir + 28,
            cdir + 30,
            cdir + 32,
            cdir + 42,
            30 + 26,
        ];
        for pos in fields {
            for val in [0x00u8, 0x01, 0x09, 0xFF] {
                if base[pos] == val {
                    continue;
                }
                let mut v = base.clone();
                v[pos] = val;
                out.push((format!("{stem}-set{pos}x{val:02x}.{ext}"), v));
            }
        }
    }
    out
}

fn corrupt_kind(name: &str) -> &'static str {
    if name.contains(".phar")
        && !name.starts_with("tardata")
        && !name.starts_with("zipdata")
        && !name.starts_with("zipgz")
    {
        "phar"
    } else {
        "data"
    }
}

#[test]
#[ignore = "regenerates corruption listings with /usr/bin/php8.5"]
fn regenerate_corruptions() {
    let dir = corrupt_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut expected = String::new();
    for (name, bytes) in corrupt_variants() {
        let path = dir.join(&name);
        std::fs::write(&path, &bytes).unwrap();
        let l = php(&[
            "-d",
            "phar.require_hash=0",
            "-d",
            "display_errors=0",
            "-d",
            "log_errors=0",
            oracle_dir().join("list.php").to_str().unwrap(),
            path.to_str().unwrap(),
            corrupt_kind(&name),
        ]);
        let text = String::from_utf8_lossy(&l.stdout).replace(dir.to_str().unwrap(), "%DIR%");
        expected += &format!("### {name}\n{text}");
        std::fs::remove_file(&path).unwrap();
    }
    std::fs::write(dir.join("expected.txt"), expected).unwrap();
}

/// Maps PHP 8.5's `(cannot create unknown filter while decompressing file
/// "x")` to master's `(file "x" is compressed with an unknown compression
/// algorithm)`.
fn reword_unknown_filter(text: &str) -> String {
    const OLD: &str = "(cannot create unknown filter while decompressing file \"";
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(OLD) {
        out.push_str(&rest[..i]);
        let after = &rest[i + OLD.len()..];
        let end = after.find("\")").unwrap_or(after.len());
        out.push_str(&format!(
            "(file \"{}\" is compressed with an unknown compression algorithm)",
            &after[..end]
        ));
        rest = &after[(end + 2).min(after.len())..];
    }
    out.push_str(rest);
    out
}

/// Tar archives truncated exactly at a block boundary: master stops at the
/// end of the file (and detects 512-byte tars), PHP 8.5 reports truncation
/// (of these, only for `tarexec-cut1024`).
fn master_differs_corrupt(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("tardata-cut")
        .or_else(|| name.strip_prefix("tarexec-cut"))
    else {
        return false;
    };
    let n: usize = rest.split('.').next().unwrap_or("").parse().unwrap_or(1);
    n > 0 && n % 512 == 0
}

#[test]
fn corrupted_archives_fail_like_php() {
    php_phar::stub::set_flavor(php_phar::stub::Flavor::Release);
    registry::set_compression_support(true, true);
    let Ok(expected) = std::fs::read_to_string(corrupt_dir().join("expected.txt")) else {
        return;
    };
    let mut want = std::collections::HashMap::new();
    for chunk in expected.split("### ").skip(1) {
        let (name, body) = chunk.split_once('\n').unwrap();
        want.insert(name.to_string(), body.to_string());
    }
    let work = scratch("corrupt");
    let mut failures = Vec::new();
    let mut checked = 0;
    for (name, bytes) in corrupt_variants() {
        let Some(exp) = want.get(&name) else { continue };
        if master_differs_corrupt(&name) {
            continue;
        }
        // reworded in master (e65a4b0f53b "Improve various error messages")
        let exp = &reword_unknown_filter(exp);
        let path = work.join(&name);
        std::fs::write(&path, &bytes).unwrap();
        registry::reset_request();
        registry::set_ini("phar.require_hash", false, true);
        let ours = listing(
            path.to_str().unwrap().as_bytes(),
            corrupt_kind(&name) == "data",
        )
        .replace(work.to_str().unwrap(), "%DIR%");
        checked += 1;
        if &ours != exp {
            failures.push(format!("== {name}\n--- php\n{exp}--- ours\n{ours}"));
        }
    }
    registry::set_ini("phar.require_hash", true, true);
    registry::reset_request();
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        failures.is_empty(),
        "{} of {checked} differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
