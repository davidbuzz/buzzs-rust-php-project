//! Scenario oracle tests: `tests/scenarios/<name>.scn` scripts are run by
//! PHP (`tests/oracle/scenario.php`, output cached as `<name>.out` by the
//! ignored `regenerate_scenarios` test) and by [`run`] here; the outputs
//! (results, warnings, exceptions) must be identical.
//!
//! Script lines: `ini <name> <0|1>`, `phar|data <args>` (constructors),
//! `call <method> <args>`, `swap <method> <args>` (replace the object by the
//! returned one), `static <method> <args>`, `info <entry> <method> <args>`
//! (on a fresh `PharFileInfo`), `fn <function> <args>`, `release`,
//! `shell <file>` (does a file exist in the work dir). Argument tokens:
//! `s:str`, `x:hex`, `i:int`, `b:0|1`, `n` (null), `p:file` (path in the
//! work dir), `u:file/inner` (`phar://` URL in the work dir),
//! `a:tok,tok` (array).

#![allow(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    clippy::cast_possible_wrap,
    clippy::format_push_string
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use php_phar::error::PharError;
use php_phar::object::{ExtractFiles, MetadataGet, PharFileInfo, PharObject};
use php_phar::registry;
use php_streams::filestat::{php_stat, FsType, StatResult};

#[derive(Debug, Clone)]
enum Tok {
    Null,
    Str(Vec<u8>),
    Int(i64),
    Bool(bool),
    Arr(Vec<Tok>),
}

impl Tok {
    fn bytes(&self) -> Vec<u8> {
        match self {
            Tok::Str(s) => s.clone(),
            Tok::Int(i) => i.to_string().into_bytes(),
            Tok::Bool(true) => b"1".to_vec(),
            _ => Vec::new(),
        }
    }

    fn opt(&self) -> Option<Vec<u8>> {
        match self {
            Tok::Null => None,
            t => Some(t.bytes()),
        }
    }

    fn int(&self) -> i64 {
        match self {
            Tok::Int(i) => *i,
            Tok::Bool(b) => i64::from(*b),
            _ => 0,
        }
    }
}

fn hex(h: &str) -> Vec<u8> {
    (0..h.len() / 2)
        .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}

fn tok(t: &str, w: &str) -> Tok {
    if t == "n" {
        return Tok::Null;
    }
    let (k, v) = t.split_once(':').unwrap_or((t, ""));
    match k {
        "s" => Tok::Str(v.as_bytes().to_vec()),
        "x" => Tok::Str(hex(v)),
        "i" => Tok::Int(v.parse().unwrap_or(0)),
        "b" => Tok::Bool(v == "1"),
        "p" => Tok::Str(format!("{w}/{v}").into_bytes()),
        "u" => Tok::Str(format!("phar://{w}/{v}").into_bytes()),
        "a" => Tok::Arr(if v.is_empty() {
            Vec::new()
        } else {
            v.split(',').map(|x| tok(x, w)).collect()
        }),
        _ => Tok::Str(t.as_bytes().to_vec()),
    }
}

/// A PHP value as printed by `dump()` in scenario.php.
#[derive(Debug, Clone)]
enum V {
    Null,
    Bool(bool),
    Int(i64),
    Str(Vec<u8>),
    Arr(Vec<(V, V)>),
    Obj(String),
}

fn dump(v: &V, w: &str) -> String {
    match v {
        V::Null => "NULL".into(),
        V::Bool(b) => format!("bool({b})"),
        V::Int(i) => format!("int({i})"),
        V::Str(s) => {
            let s = String::from_utf8_lossy(s).replace(w, "%W%").into_bytes();
            if s.len() > 80 || s.iter().any(|&c| !(0x20..=0x7e).contains(&c)) {
                format!(
                    "string({}) md5:{}",
                    s.len(),
                    php_hash::bin2hex(&php_hash::md5(&s))
                )
            } else {
                format!("string({}) \"{}\"", s.len(), String::from_utf8_lossy(&s))
            }
        }
        V::Arr(items) => format!(
            "array({})",
            items
                .iter()
                .map(|(k, x)| format!("{}=>{}", dump(k, w), dump(x, w)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        V::Obj(o) => o.replace(w, "%W%"),
    }
}

fn runtime_to_v(v: &php_runtime::Value) -> V {
    use php_runtime::{ArrayKey, Value};
    match v {
        Value::Null => V::Null,
        Value::Bool(b) => V::Bool(*b),
        Value::Int(i) => V::Int(*i),
        Value::Str(s) => V::Str(s.as_bytes().to_vec()),
        Value::Array(a) => V::Arr(
            a.iter()
                .map(|(k, x)| {
                    (
                        match k {
                            ArrayKey::Int(i) => V::Int(*i),
                            ArrayKey::Str(s) => V::Str(s.as_bytes().to_vec()),
                        },
                        runtime_to_v(x),
                    )
                })
                .collect(),
        ),
        _ => V::Obj("object".into()),
    }
}

fn meta_v(m: MetadataGet) -> V {
    match m {
        MetadataGet::None => V::Null,
        MetadataGet::Value(v) => runtime_to_v(&v),
        MetadataGet::Serialized(s) => {
            let mut ctx = php_runtime::CollectingContext::default();
            let v = php_runtime::serialize::unserialize(
                &s,
                &php_runtime::serialize::UnserializeOptions::default(),
                &mut ctx,
            )
            .ok()
            .flatten()
            .unwrap_or(php_runtime::Value::Bool(false));
            runtime_to_v(&v)
        }
    }
}

fn tok_runtime(t: &Tok) -> php_runtime::Value {
    match t {
        Tok::Null => php_runtime::Value::Null,
        Tok::Str(s) => php_runtime::Value::string(s.as_slice()),
        Tok::Int(i) => php_runtime::Value::Int(*i),
        Tok::Bool(b) => php_runtime::Value::Bool(*b),
        Tok::Arr(items) => php_runtime::Value::Array(php_runtime::ZArray::from_values(
            items.iter().map(tok_runtime),
        )),
    }
}

fn serialize(v: &php_runtime::Value) -> Vec<u8> {
    let mut ctx = php_runtime::CollectingContext::default();
    php_runtime::serialize::serialize(v, &mut ctx).unwrap()
}

fn strs(v: &[&str]) -> V {
    V::Arr(
        v.iter()
            .enumerate()
            .map(|(i, s)| (V::Int(i as i64), V::Str(s.as_bytes().to_vec())))
            .collect(),
    )
}

fn obj_name(is_data: bool, path: &[u8]) -> V {
    V::Obj(format!(
        "{}({})",
        if is_data { "PharData" } else { "Phar" },
        String::from_utf8_lossy(path)
    ))
}

struct Ctx {
    w: String,
    p: Option<PharObject>,
    out: String,
    method: String,
}

/// Stands in for ext/openssl so `getSupportedSignatures()` matches a PHP
/// build with OpenSSL; signing and verifying always fail.
struct NoOpenSsl;

impl php_phar::signature::OpenSslHook for NoOpenSsl {
    fn sign(&self, _: &[u8], _: &[u8], _: i64) -> Option<Vec<u8>> {
        None
    }

    fn verify(&self, _: &[u8], _: &[u8], _: &[u8], _: i64) -> bool {
        false
    }
}

fn exc(e: &PharError) -> String {
    format!("EXC {}: {}", e.class.name(), e.message)
}

impl Ctx {
    fn obj(&mut self) -> Result<&mut PharObject, String> {
        let m = self.method.clone();
        self.p
            .as_mut()
            .ok_or_else(|| format!("EXC Error: Call to a member function {m}() on null"))
    }

    #[allow(clippy::too_many_lines)]
    fn call(&mut self, method: &str, a: &[Tok]) -> Result<V, String> {
        let s = |i: usize| a.get(i).map(Tok::bytes).unwrap_or_default();
        let o = |i: usize| a.get(i).and_then(Tok::opt);
        let n = |i: usize| a.get(i).map(Tok::int);
        let p = self.obj()?;
        let e = |e: PharError| exc(&e);
        Ok(match method {
            "getAlias" => p.get_alias().map_err(e)?.map_or(V::Null, V::Str),
            "setAlias" => V::Bool(p.set_alias(&s(0)).map_err(e)?),
            "getPath" => V::Str(p.get_path().map_err(e)?),
            "getVersion" => V::Str(p.get_version().map_err(e)?.into_bytes()),
            "count" => V::Int(p.count().map_err(e)?),
            "getStub" => V::Str(p.get_stub().map_err(e)?),
            "setStub" => V::Bool(p.set_stub(&s(0)).map_err(e)?),
            "setDefaultStub" => V::Bool(
                p.set_default_stub(o(0).as_deref(), o(1).as_deref())
                    .map_err(e)?,
            ),
            "getSignature" => match p.get_signature().map_err(e)? {
                Some(sig) => V::Arr(vec![
                    (V::Str(b"hash".to_vec()), V::Str(sig.hash)),
                    (
                        V::Str(b"hash_type".to_vec()),
                        V::Str(sig.hash_type.into_bytes()),
                    ),
                ]),
                None => V::Bool(false),
            },
            "setSignatureAlgorithm" => {
                p.set_signature_algorithm(n(0).unwrap_or(0), o(1).as_deref())
                    .map_err(e)?;
                V::Null
            }
            "getMetadata" => meta_v(p.get_metadata(false).map_err(e)?),
            "setMetadata" => {
                let v = tok_runtime(&a[0]);
                let ser = serialize(&v);
                p.set_metadata(v, ser).map_err(e)?;
                V::Null
            }
            "delMetadata" => V::Bool(p.del_metadata().map_err(e)?),
            "hasMetadata" => V::Bool(p.has_metadata().map_err(e)?),
            "offsetGet" => {
                let path = p.offset_get(&s(0)).map_err(e)?;
                // the VM instantiates the info class with that path
                let mut fi = PharFileInfo::new();
                let mut name = fi.construct(&path).map_err(e)?;
                fi.release();
                // SplFileInfo strips trailing slashes from the path name
                while name.len() > 1 && name.last() == Some(&b'/') {
                    name.pop();
                }
                V::Obj(format!("PharFileInfo({})", String::from_utf8_lossy(&name)))
            }
            "offsetSet" => {
                p.offset_set(&s(0), &s(1)).map_err(e)?;
                V::Null
            }
            "offsetExists" => V::Bool(p.offset_exists(&s(0)).map_err(e)?),
            "offsetUnset" => {
                p.offset_unset(&s(0)).map_err(e)?;
                V::Null
            }
            "addFromString" => {
                p.add_from_string(&s(0), &s(1)).map_err(e)?;
                V::Null
            }
            "addEmptyDir" => {
                p.add_empty_dir(&s(0)).map_err(e)?;
                V::Null
            }
            "addFile" => {
                let file = s(0);
                let content = std::fs::read(String::from_utf8_lossy(&file).as_ref()).ok();
                let mode = std::fs::metadata(String::from_utf8_lossy(&file).as_ref())
                    .ok()
                    .map(|m| std::os::unix::fs::PermissionsExt::mode(&m.permissions()));
                let opened = content.as_ref().map(|c| (c.as_slice(), mode));
                p.add_file(&file, o(1).as_deref(), opened).map_err(e)?;
                V::Null
            }
            "delete" => V::Bool(p.delete(&s(0)).map_err(e)?),
            "copy" => V::Bool(p.copy(&s(0), &s(1)).map_err(e)?),
            "compressFiles" => {
                p.compress_files(n(0).unwrap_or(0)).map_err(e)?;
                V::Null
            }
            "decompressFiles" => V::Bool(p.decompress_files().map_err(e)?),
            "isCompressed" => p.is_compressed().map_err(e)?.map_or(V::Bool(false), V::Int),
            "isFileFormat" => V::Bool(p.is_file_format(n(0).unwrap_or(0)).map_err(e)?),
            "isWritable" => V::Bool(p.is_writable().map_err(e)?),
            "isBuffering" => V::Bool(p.is_buffering().map_err(e)?),
            "startBuffering" => {
                p.start_buffering().map_err(e)?;
                V::Null
            }
            "stopBuffering" => {
                p.stop_buffering().map_err(e)?;
                V::Null
            }
            "getModified" => V::Bool(p.get_modified().map_err(e)?),
            "extractTo" => {
                let files = match a.get(1) {
                    None | Some(Tok::Null) => ExtractFiles::All,
                    Some(Tok::Arr(items)) => ExtractFiles::List(
                        items
                            .iter()
                            .map(|t| match t {
                                Tok::Str(s) => Some(s.clone()),
                                _ => None,
                            })
                            .collect(),
                    ),
                    Some(t) => ExtractFiles::One(t.bytes()),
                };
                let overwrite = a.get(2).is_some_and(|t| t.int() != 0);
                V::Bool(p.extract_to(&s(0), &files, overwrite).map_err(e)?)
            }
            other => panic!("unknown method {other}"),
        })
    }

    fn swap(&mut self, method: &str, a: &[Tok]) -> Result<V, String> {
        let o = |i: usize| a.get(i).and_then(Tok::opt);
        let n = |i: usize| match a.get(i) {
            None | Some(Tok::Null) => None,
            Some(t) => Some(t.int()),
        };
        let p = self.obj()?;
        let r = match method {
            "compress" => p.compress(n(0).unwrap_or(0), o(1).as_deref()),
            "decompress" => p.decompress(o(0).as_deref()),
            "convertToData" => p.convert_to_data(n(0), n(1), o(2).as_deref()),
            "convertToExecutable" => p.convert_to_executable(n(0), n(1), o(2).as_deref()),
            other => panic!("unknown swap {other}"),
        };
        let c = r.map_err(|e| exc(&e))?;
        let mut np = PharObject::new(c.is_data);
        np.construct(&c.fname, None, 0).map_err(|e| exc(&e))?;
        if let Some(mut old) = self.p.replace(np) {
            old.release();
        }
        Ok(obj_name(c.is_data, &c.fname))
    }

    fn convert_only(&mut self, method: &str, a: &[Tok]) -> Result<V, String> {
        let o = |i: usize| a.get(i).and_then(Tok::opt);
        let n = |i: usize| match a.get(i) {
            None | Some(Tok::Null) => None,
            Some(t) => Some(t.int()),
        };
        let p = self.obj()?;
        let r = match method {
            "compress" => p.compress(n(0).unwrap_or(0), o(1).as_deref()),
            "decompress" => p.decompress(o(0).as_deref()),
            "convertToData" => p.convert_to_data(n(0), n(1), o(2).as_deref()),
            _ => p.convert_to_executable(n(0), n(1), o(2).as_deref()),
        };
        let c = r.map_err(|e| exc(&e))?;
        let mut np = PharObject::new(c.is_data);
        np.construct(&c.fname, None, 0).map_err(|e| exc(&e))?;
        np.release();
        Ok(obj_name(c.is_data, &c.fname))
    }

    fn stat_fn(f: &str, path: &[u8]) -> V {
        let ty = match f {
            "is_dir" => FsType::IsDir,
            "is_file" => FsType::IsFile,
            "file_exists" => FsType::Exists,
            "filesize" => FsType::Size,
            "fileperms" => FsType::Perms,
            "filemtime" => FsType::Mtime,
            _ => FsType::IsLink,
        };
        match php_stat(path, ty) {
            StatResult::Bool(b) => V::Bool(b),
            StatResult::Int(i) => V::Int(i),
            _ => V::Null,
        }
    }

    fn func(f: &str, a: &[Tok]) -> V {
        let s = |i: usize| a.get(i).map(Tok::bytes).unwrap_or_default();
        match f {
            "file_get_contents" => {
                match php_streams::file::file_get_contents(&s(0), false, None, 0, None) {
                    Ok(Some(c)) => V::Str(c),
                    _ => V::Bool(false),
                }
            }
            "file_put_contents" => {
                let d = s(1);
                php_streams::file::file_put_contents(
                    &s(0),
                    php_streams::file::PutData::Bytes(&d),
                    0,
                    None,
                )
                .map_or(V::Bool(false), V::Int)
            }
            "unlink" => V::Bool(php_streams::file::unlink(&s(0), None)),
            "rename" => V::Bool(php_streams::file::rename(&s(0), &s(1), None)),
            "mkdir" => V::Bool(php_streams::file::mkdir(&s(0), 0o777, false, None)),
            "rmdir" => V::Bool(php_streams::file::rmdir(&s(0), None)),
            "scandir" => {
                if let Some(list) = php_streams::open::scandir(
                    &s(0),
                    php_streams::open::ScandirSort::Ascending,
                    None,
                ) {
                    V::Arr(
                        list.into_iter()
                            .enumerate()
                            .map(|(i, n)| (V::Int(i as i64), V::Str(n)))
                            .collect(),
                    )
                } else {
                    // ext/standard's scandir() warning
                    php_streams::diag::warning("(errno 2): No such file or directory");
                    V::Bool(false)
                }
            }
            other => Self::stat_fn(other, &s(0)),
        }
    }

    fn statik(f: &str, a: &[Tok]) -> Result<V, String> {
        use php_phar::object as ob;
        let s = |i: usize| a.get(i).map(Tok::bytes).unwrap_or_default();
        let o = |i: usize| a.get(i).and_then(Tok::opt);
        Ok(match f {
            "isValidPharFilename" => V::Bool(ob::is_valid_phar_filename(
                &s(0),
                a.get(1).is_none_or(|t| t.int() != 0),
            )),
            "canWrite" => V::Bool(ob::can_write()),
            "canCompress" => V::Bool(ob::can_compress(a.first().map_or(0, Tok::int))),
            "apiVersion" => V::Str(ob::api_version().as_bytes().to_vec()),
            "getSupportedSignatures" => strs(&ob::get_supported_signatures()),
            "getSupportedCompression" => strs(&ob::get_supported_compression()),
            "createDefaultStub" => V::Str(
                ob::create_default_stub(o(0).as_deref(), o(1).as_deref()).map_err(|e| exc(&e))?,
            ),
            "loadPhar" => V::Bool(ob::load_phar(&s(0), o(1).as_deref()).map_err(|e| exc(&e))?),
            "unlinkArchive" => V::Bool(ob::unlink_archive(&s(0), None).map_err(|e| exc(&e))?),
            "running" => V::Str(ob::running(None, true)),
            other => panic!("unknown static {other}"),
        })
    }

    fn info(&mut self, name: &[u8], method: &str, a: &[Tok]) -> Result<V, String> {
        let path = self.obj()?.get_path().map_err(|e| exc(&e))?;
        let mut url = b"phar://".to_vec();
        url.extend_from_slice(&path);
        url.push(b'/');
        url.extend_from_slice(name);
        let mut fi = PharFileInfo::new();
        fi.construct(&url).map_err(|e| exc(&e))?;
        let e = |e: PharError| exc(&e);
        let n = |i: usize| a.get(i).map(Tok::int);
        let r = (|| -> Result<V, String> {
            Ok(match method {
                "getCompressedSize" => V::Int(fi.get_compressed_size().map_err(e)?),
                "isCompressed" => V::Bool(fi.is_compressed(n(0)).map_err(e)?),
                "getCRC32" => V::Int(fi.get_crc32().map_err(e)?),
                "isCRCChecked" => V::Bool(fi.is_crc_checked().map_err(e)?),
                "getPharFlags" => V::Int(fi.get_phar_flags().map_err(e)?),
                "chmod" => {
                    fi.chmod(n(0).unwrap_or(0)).map_err(e)?;
                    V::Null
                }
                "getMetadata" => meta_v(fi.get_metadata(false).map_err(e)?),
                "setMetadata" => {
                    let v = tok_runtime(&a[0]);
                    let ser = serialize(&v);
                    fi.set_metadata(v, ser).map_err(e)?;
                    V::Null
                }
                "delMetadata" => V::Bool(fi.del_metadata().map_err(e)?),
                "hasMetadata" => V::Bool(fi.has_metadata().map_err(e)?),
                "getContent" => V::Str(fi.get_content().map_err(e)?),
                "compress" => V::Bool(fi.compress(n(0).unwrap_or(0)).map_err(e)?),
                "decompress" => V::Bool(fi.decompress().map_err(e)?),
                other => panic!("unknown info method {other}"),
            })
        })();
        fi.release();
        r
    }

    fn line(&mut self, line: &str) {
        let words: Vec<&str> = line.split(' ').collect();
        let w = self.w.clone();
        let args: Vec<Tok> = words.iter().skip(2).map(|t| tok(t, &w)).collect();
        self.method = words.get(1).map_or(String::new(), |m| (*m).to_string());
        if words[0] == "info" {
            self.method = "getPath".into();
        }
        let r: Result<String, String> = match words[0] {
            "ini" => {
                let old = if registry::readonly() && words[1] == "phar.readonly"
                    || registry::require_hash() && words[1] == "phar.require_hash"
                {
                    "1"
                } else {
                    "0"
                };
                let ok = registry::set_ini(words[1], words[2] == "1", false);
                Ok(if ok {
                    dump(&V::Str(old.as_bytes().to_vec()), &w)
                } else {
                    "bool(false)".into()
                })
            }
            "phar" | "data" => {
                let all: Vec<Tok> = words.iter().skip(1).map(|t| tok(t, &w)).collect();
                let mut p = PharObject::new(words[0] == "data");
                let alias = all.get(2).and_then(Tok::opt);
                let format = all.get(3).map_or(0, Tok::int);
                match p.construct(&all[0].bytes(), alias.as_deref(), format) {
                    Ok(_) => {
                        if let Some(mut old) = self.p.replace(p) {
                            old.release();
                        }
                        Ok("ok".into())
                    }
                    Err(e) => Err(exc(&e)),
                }
            }
            "call"
                if matches!(
                    words[1],
                    "compress" | "decompress" | "convertToData" | "convertToExecutable"
                ) =>
            {
                // Like `swap`, but the returned object is dropped again.
                self.convert_only(words[1], &args).map(|v| dump(&v, &w))
            }
            "call" => self.call(words[1], &args).map(|v| dump(&v, &w)),
            "swap" => self.swap(words[1], &args).map(|v| dump(&v, &w)),
            "static" => {
                let args: Vec<Tok> = words.iter().skip(2).map(|t| tok(t, &w)).collect();
                Self::statik(words[1], &args).map(|v| dump(&v, &w))
            }
            "info" => {
                let name = tok(words[1], &w).bytes();
                let args: Vec<Tok> = words.iter().skip(3).map(|t| tok(t, &w)).collect();
                self.info(&name, words[2], &args).map(|v| dump(&v, &w))
            }
            "fn" => Ok(dump(&Self::func(words[1], &args), &w)),
            "clearstatcache" => {
                php_streams::state::clear_stat_cache();
                Ok("ok".into())
            }
            "copyfile" => Ok(dump(
                &V::Bool(
                    std::fs::copy(format!("{w}/{}", words[1]), format!("{w}/{}", words[2])).is_ok(),
                ),
                &w,
            )),
            "release" => {
                if let Some(mut p) = self.p.take() {
                    p.release();
                }
                Ok("ok".into())
            }
            "mkfile" => {
                let path = format!("{w}/{}", words[1]);
                if let Some(parent) = Path::new(&path).parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                let content = words.get(2).map_or(Vec::new(), |t| tok(t, &w).bytes());
                std::fs::write(&path, content).unwrap();
                Ok("ok".into())
            }
            "build" => {
                let dir = format!("{w}/{}", words[1]);
                let suffix = words.get(2).map(|r| {
                    let r = tok(r, &w).bytes();
                    let r = String::from_utf8_lossy(&r).into_owned();
                    // only `/\.ext$/` style patterns are supported here
                    r.trim_start_matches("/\\")
                        .trim_end_matches("$/")
                        .to_string()
                });
                let filter = |p: &[u8]| suffix.as_ref().is_none_or(|s| p.ends_with(s.as_bytes()));
                let p = self.obj();
                match p {
                    Err(e) => Err(e),
                    Ok(p) => match p.build_from_directory(dir.as_bytes(), Some(&filter)) {
                        Ok(mut r) => {
                            r.sort();
                            Ok(dump(
                                &V::Arr(
                                    r.into_iter().map(|(k, v)| (V::Str(k), V::Str(v))).collect(),
                                ),
                                &w,
                            ))
                        }
                        Err(e) => Err(exc(&e)),
                    },
                }
            }
            "buildi" => {
                let base = tok(words[1], &w);
                let items: Vec<(Tok, Tok)> = words[2..]
                    .iter()
                    .map(|kv| {
                        let (k, v) = kv.split_once('=').unwrap();
                        (tok(k, &w), tok(v, &w))
                    })
                    .collect();
                let p = self.obj();
                match p {
                    Err(e) => Err(e),
                    Ok(p) => {
                        let r = (|| {
                            let mut st =
                                p.build_from_iterator_start(&base.opt().unwrap_or_default())?;
                            for (k, v) in items {
                                let key = match k {
                                    Tok::Str(s) => Some(s),
                                    _ => None,
                                };
                                let item = match v {
                                    Tok::Str(s) => {
                                        php_phar::object::BuildItem::Path { value: s, key }
                                    }
                                    _ => php_phar::object::BuildItem::Invalid,
                                };
                                st.add(item, "ArrayIterator")?;
                            }
                            st.finish()
                        })();
                        match r {
                            Ok(r) => Ok(dump(
                                &V::Arr(
                                    r.into_iter().map(|(k, v)| (V::Str(k), V::Str(v))).collect(),
                                ),
                                &w,
                            )),
                            Err(e) => Err(exc(&e)),
                        }
                    }
                }
            }
            "fopen" => {
                let url = tok(words[1], &w).bytes();
                let mut opened = None;
                match php_streams::open::open_wrapper(
                    &url,
                    words[2],
                    php_streams::wrapper::REPORT_ERRORS,
                    &mut opened,
                    None,
                ) {
                    Some(mut f) => {
                        if let Some(d) = words.get(3) {
                            let _ = f.write(&tok(d, &w).bytes());
                        }
                        f.close();
                        Ok("ok".into())
                    }
                    None => Ok("bool(false)".into()),
                }
            }
            "shell" => Ok(dump(
                &V::Bool(Path::new(&format!("{w}/{}", words[1])).exists()),
                &w,
            )),
            other => panic!("unknown op {other}"),
        };
        for d in php_streams::diag::take() {
            let tag = match d.severity {
                php_streams::diag::Severity::Warning => "W",
                php_streams::diag::Severity::Notice => "N",
                php_streams::diag::Severity::Deprecated => "D",
                php_streams::diag::Severity::Error => "E",
            };
            self.out += &format!("{tag}: {}\n", d.text().replace(&w, "%W%"));
        }
        match r {
            Ok(s) | Err(s) => {
                self.out += &s.replace(&w, "%W%");
                self.out.push('\n');
            }
        }
    }
}

fn scenarios_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scenarios")
}

fn scenarios() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(scenarios_dir())
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            (p.extension()? == "scn").then(|| p.file_stem().unwrap().to_string_lossy().into_owned())
        })
        .collect();
    v.sort();
    v
}

fn workdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("php-phar-scn-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Runs a scenario with this crate; returns its output.
fn run(name: &str) -> String {
    let w = workdir(name);
    let ws = w.to_string_lossy().into_owned();
    php_phar::stub::set_flavor(php_phar::stub::Flavor::Php83);
    registry::set_compression_support(true, false);
    registry::set_ini("phar.readonly", false, true);
    registry::set_ini("phar.require_hash", true, true);
    php_phar::signature::set_openssl_hook(Some(Rc::new(NoOpenSsl)));
    // Ubuntu's PHP 8.3 writes 0 for entry timestamps.
    php_phar::sys::set_entry_time_override(Some(0));
    let _ = php_streams::wrapper::register_global(b"phar", Rc::new(php_phar::wrapper::PharWrapper));
    let _ = php_streams::diag::take();
    let mut ctx = Ctx {
        w: ws.clone(),
        p: None,
        out: String::new(),
        method: String::new(),
    };
    let text = std::fs::read_to_string(scenarios_dir().join(format!("{name}.scn"))).unwrap();
    for line in text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        ctx.out += &format!("> {line}\n");
        ctx.line(line);
    }
    if let Some(mut p) = ctx.p.take() {
        p.release();
    }
    registry::reset_request();
    let _ = std::fs::remove_dir_all(&w);
    ctx.out
}

#[test]
#[ignore = "regenerates scenario outputs with /usr/bin/php"]
fn regenerate_scenarios() {
    for name in scenarios() {
        let w = workdir(&name);
        let o = Command::new("/usr/bin/php")
            .args([
                "-d",
                "memory_limit=1G",
                "-d",
                "display_errors=0",
                "-d",
                "phar.readonly=0",
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/oracle/scenario.php")
                    .to_str()
                    .unwrap(),
                scenarios_dir()
                    .join(format!("{name}.scn"))
                    .to_str()
                    .unwrap(),
                w.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        std::fs::write(scenarios_dir().join(format!("{name}.out")), &o.stdout).unwrap();
        let _ = std::fs::remove_dir_all(&w);
    }
}

/// Removes VM-level noise (errno texts of ext/standard warnings) and maps
/// PHP 8.3 messages that php-src master reworded.
fn normalize(out: &str) -> String {
    out.lines()
        .filter(|l| !l.starts_with("W: (errno "))
        .map(|l| {
            // master drops the path from the RecursiveDirectoryIterator error
            let l = match (
                l.find("RecursiveDirectoryIterator::__construct("),
                l.find("): Failed"),
            ) {
                (Some(a), Some(b)) if a < b => format!(
                    "{}RecursiveDirectoryIterator::__construct(){}",
                    &l[..a],
                    &l[b + 1..]
                ),
                _ => l.to_string(),
            };
            l.replace(
                "exists and is a not a directory",
                "exists and is not a directory",
            )
            .replace("\" cannot be empty", "\" must not be empty")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn scenarios_match_php() {
    let mut failures = Vec::new();
    for name in scenarios() {
        let Ok(expected) = std::fs::read_to_string(scenarios_dir().join(format!("{name}.out")))
        else {
            continue;
        };
        let ours = normalize(&run(&name));
        let expected = normalize(&expected);
        if ours != expected {
            let e: Vec<&str> = expected.lines().collect();
            let o: Vec<&str> = ours.lines().collect();
            let first = e
                .iter()
                .zip(&o)
                .position(|(a, b)| a != b)
                .unwrap_or(e.len().min(o.len()));
            let from = first.saturating_sub(3);
            failures.push(format!(
                "== {name} (line {first})\n--- php\n{}\n--- ours\n{}",
                e[from..(first + 4).min(e.len())].join("\n"),
                o[from..(first + 4).min(o.len())].join("\n")
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
