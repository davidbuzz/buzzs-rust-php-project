//! The Rust side of the scenario interpreter (mirror of
//! `tests/oracle/runner.php`).

#![allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names,
    clippy::float_cmp,
    clippy::format_push_string,
    dead_code
)]

use std::cell::RefCell;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::rc::Rc;

use php_runtime::{ErrorLevel, RtResult, RuntimeContext, RuntimeError, Value, ZArray};
use php_streams::diag::{self, Severity};
use php_streams::stream::Stream;
use php_zip::procedural::{self, ZipDir, ZipEntry, ZipOpenResult};
use php_zip::{EntryStat, ExtractFiles, OpenResult, RegexMatcher, ZipArchive, ZipHost};
use serde_json::Value as J;

/// A printable PHP value (`ex()` of runner.php).
#[derive(Debug, Clone)]
pub enum V {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Vec<u8>),
    Arr(Vec<(V, V)>),
    Raw(String),
}

impl V {
    fn opt_str(v: Option<Vec<u8>>) -> V {
        v.map_or(V::Bool(false), V::Str)
    }
    fn opt_int(v: Option<i64>) -> V {
        v.map_or(V::Bool(false), V::Int)
    }
    fn s(s: &str) -> V {
        V::Str(s.as_bytes().to_vec())
    }
}

fn from_value(v: &Value) -> V {
    match v {
        Value::Null => V::Null,
        Value::Bool(b) => V::Bool(*b),
        Value::Int(i) => V::Int(*i),
        Value::Float(f) => V::Float(*f),
        Value::Str(s) => V::Str(s.as_bytes().to_vec()),
        Value::Array(a) => V::Arr(
            a.iter()
                .map(|(k, x)| {
                    let kv = match k.to_value() {
                        Value::Int(i) => V::Int(i),
                        other => V::Str(other.to_string_lossy_bytes()),
                    };
                    (kv, from_value(x))
                })
                .collect(),
        ),
        Value::Reference(r) => from_value(&r.get()),
        other => V::Raw(other.type_name()),
    }
}

trait LossyBytes {
    fn to_string_lossy_bytes(&self) -> Vec<u8>;
}

impl LossyBytes for Value {
    fn to_string_lossy_bytes(&self) -> Vec<u8> {
        match self {
            Value::Str(s) => s.as_bytes().to_vec(),
            Value::Int(i) => i.to_string().into_bytes(),
            _ => Vec::new(),
        }
    }
}

fn stat_v(s: &EntryStat) -> V {
    V::Arr(vec![
        (V::s("name"), V::Str(s.name.clone())),
        (V::s("index"), V::Int(s.index)),
        (V::s("crc"), V::Int(s.crc)),
        (V::s("size"), V::Int(s.size)),
        (V::s("mtime"), V::Int(s.mtime)),
        (V::s("comp_size"), V::Int(s.comp_size)),
        (V::s("comp_method"), V::Int(s.comp_method)),
        (V::s("encryption_method"), V::Int(s.encryption_method)),
    ])
}

fn ex(v: &V) -> String {
    match v {
        V::Null => "null".into(),
        V::Bool(b) => if *b { "true" } else { "false" }.into(),
        V::Int(i) => i.to_string(),
        V::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{f:.1}")
            } else {
                format!("{f}")
            }
        }
        V::Str(s) => {
            if s.iter().all(|&c| (0x20..=0x7e).contains(&c)) {
                format!("\"{}\"", String::from_utf8_lossy(s))
            } else {
                let mut h = String::from("hex:");
                for c in s {
                    let _ = write!(h, "{c:02x}");
                }
                h
            }
        }
        V::Arr(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|(k, x)| {
                    let is_mtime = matches!(k, V::Str(s) if s == b"mtime");
                    if is_mtime && matches!(x, V::Int(t) if *t > 1_600_000_000) {
                        format!("{}=>NOW", ex(k))
                    } else {
                        format!("{}=>{}", ex(k), ex(x))
                    }
                })
                .collect();
            format!("[{}]", parts.join(","))
        }
        V::Raw(s) => s.clone(),
    }
}

/// A variable of the scenario.
enum Var {
    Zip(Rc<ZipArchive>),
    Dir(ZipDir),
    Entry(ZipEntry),
    Gone,
}

struct State {
    out: String,
    dir: String,
    fix: String,
    func: String,
    vars: Vec<(String, Var)>,
    streams: Vec<Option<Stream>>,
}

type Shared = Rc<RefCell<State>>;

fn emit(st: &Shared, line: &str) {
    let mut s = st.borrow_mut();
    s.out.push_str(line);
    s.out.push('\n');
}

/// The VM stand-in.
struct Host {
    st: Shared,
}

impl RuntimeContext for Host {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        let lvl = match level {
            ErrorLevel::Warning => "Warning",
            ErrorLevel::Deprecated => "Deprecated",
            ErrorLevel::Notice => "Notice",
            _ => "Other",
        };
        emit(&self.st, &format!("[{lvl}] {message}"));
        Ok(())
    }
}

impl ZipHost for Host {
    fn call(&mut self, callable: &Value, args: &[Value]) -> RtResult<Value> {
        let Value::Str(spec) = callable else {
            return Ok(Value::Null);
        };
        let spec = String::from_utf8_lossy(spec.as_bytes()).to_string();
        let spec = spec.strip_prefix("cb:").unwrap_or(&spec).to_string();
        let parts: Vec<&str> = spec.split(':').collect();
        let r = match args.first() {
            Some(Value::Float(f)) => Some(*f),
            _ => None,
        };
        match parts[0] {
            "progress" => {
                if let Some(r) = r {
                    if r == 0.0 {
                        emit(&self.st, "progress start");
                    }
                    if r == 1.0 {
                        emit(&self.st, "progress end");
                    }
                }
                Ok(Value::Null)
            }
            "progress_all" => {
                let r = r.unwrap_or(0.0);
                let v = if r > 0.0 && r < 1.0 {
                    V::s("mid")
                } else {
                    V::Float(r)
                };
                emit(&self.st, &format!("progress {}", ex(&v)));
                Ok(Value::Null)
            }
            "cancel" => {
                emit(&self.st, "cancel?");
                Ok(Value::Int(parts[1].parse().unwrap_or(0)))
            }
            "cancel_array" => Ok(Value::Array(ZArray::new())),
            "cancel_string" => Ok(Value::from("123abc")),
            "throw" => Err(RuntimeError::Custom {
                class: "Exception".into(),
                message: "boom".into(),
            }),
            "reenter" => {
                thread_local! { static DONE: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) }; }
                if r.is_some_and(|r| r <= 0.0) {
                    return Ok(Value::Int(0));
                }
                let key = format!("{spec}#{:p}", Rc::as_ptr(&self.st));
                if DONE.with(|d| d.borrow().contains(&key)) {
                    return Ok(Value::Int(0));
                }
                DONE.with(|d| d.borrow_mut().push(key));
                let var = parts[1].to_string();
                let method = parts[2].to_string();
                let args: Vec<J> = serde_json::from_str(&parts[3..].join(":")).unwrap_or_default();
                let zip = {
                    let s = self.st.borrow();
                    s.vars
                        .iter()
                        .find(|(n, _)| *n == var)
                        .and_then(|(_, v)| match v {
                            Var::Zip(z) => Some(Rc::clone(z)),
                            _ => None,
                        })
                };
                let saved = std::mem::replace(
                    &mut self.st.borrow_mut().func,
                    format!("ZipArchive::{method}"),
                );
                let res = zip.map(|z| dispatch(self, &z, &method, &args));
                self.st.borrow_mut().func = saved;
                match res {
                    Some(Ok(v)) => emit(&self.st, &format!("reenter {method} = {}", ex(&v))),
                    Some(Err(e)) => emit(
                        &self.st,
                        &format!(
                            "reenter {method} ! {}: {}",
                            e.exception_class().unwrap_or_default(),
                            e.message().unwrap_or("")
                        ),
                    ),
                    None => {}
                }
                Ok(Value::Int(0))
            }
            _ => Ok(Value::Null),
        }
    }

    fn compile_regex(&mut self, regex: &[u8]) -> Option<RegexMatcher> {
        match php_pcre::Regex::compile(regex) {
            Ok(re) => {
                let cfg = php_pcre::Pcre::new().config();
                Some(Box::new(
                    move |s: &[u8]| matches!(php_pcre::match_one(&re, &cfg, s, false, 0, 0).result, Ok(n) if n > 0),
                ))
            }
            Err(e) => {
                let func = self.st.borrow().func.clone();
                emit(
                    &self.st,
                    &format!(
                        "[Warning] {func}(): {}",
                        String::from_utf8_lossy(&e.message())
                    ),
                );
                None
            }
        }
    }
}

fn subst(st: &Shared, s: &str) -> Vec<u8> {
    if let Some(h) = s.strip_prefix("hex:") {
        return (0..h.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
            .collect();
    }
    if let Some(f) = s.strip_prefix("file:") {
        let path = String::from_utf8(subst(st, f)).unwrap();
        return fs::read(path).unwrap();
    }
    if let Some(r) = s.strip_prefix("rep:") {
        let (n, body) = r.split_once(':').unwrap();
        return body.repeat(n.parse().unwrap()).into_bytes();
    }
    let st = st.borrow();
    s.replace("{DIR}", &st.dir)
        .replace("{FIX}", &st.fix)
        .into_bytes()
}

fn constant(name: &str) -> i64 {
    use php_streams::glob as g;
    i64::from(match name {
        "GLOB_BRACE" => g::GLOB_BRACE,
        "GLOB_MARK" => g::GLOB_MARK,
        "GLOB_ONLYDIR" => g::GLOB_ONLYDIR,
        "GLOB_NOSORT" => g::GLOB_NOSORT,
        "GLOB_NOCHECK" => g::GLOB_NOCHECK,
        "GLOB_NOESCAPE" => g::GLOB_NOESCAPE,
        "GLOB_ERR" => g::GLOB_ERR,
        other => panic!("unknown constant {other}"),
    })
}

fn to_value(st: &Shared, j: &J) -> Value {
    match j {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => n
            .as_i64()
            .map_or_else(|| Value::Float(n.as_f64().unwrap()), Value::Int),
        J::String(s) => {
            if let Some(c) = s.strip_prefix("const:") {
                Value::Int(constant(c))
            } else if s.starts_with("cb:") {
                Value::from(s.as_str())
            } else {
                Value::from(&subst(st, s)[..])
            }
        }
        J::Array(items) => {
            let mut a = ZArray::new();
            let h = a.make_mut();
            for it in items {
                let _ = h.push(to_value(st, it));
            }
            Value::Array(a)
        }
        J::Object(map) => {
            let mut a = ZArray::new();
            let h = a.make_mut();
            for (k, v) in map {
                h.insert(k.as_str(), to_value(st, v));
            }
            Value::Array(a)
        }
    }
}

struct Args<'a> {
    st: &'a Shared,
    a: &'a [J],
}

impl Args<'_> {
    fn s(&self, i: usize) -> Vec<u8> {
        self.so(i, "")
    }
    fn so(&self, i: usize, d: &str) -> Vec<u8> {
        match self.a.get(i) {
            Some(J::String(s)) => subst(self.st, s),
            Some(J::Null) => Vec::new(),
            Some(other) => panic!("expected string, got {other}"),
            None => d.as_bytes().to_vec(),
        }
    }
    fn i(&self, i: usize, d: i64) -> i64 {
        match self.a.get(i) {
            Some(J::Number(n)) => n.as_i64().unwrap(),
            Some(J::String(s)) if s.starts_with("const:") => constant(&s[6..]),
            Some(J::Bool(b)) => i64::from(*b),
            Some(other) => panic!("expected int, got {other}"),
            None => d,
        }
    }
    fn f(&self, i: usize) -> f64 {
        self.a[i].as_f64().unwrap()
    }
    fn b(&self, i: usize, d: bool) -> bool {
        self.a.get(i).and_then(J::as_bool).unwrap_or(d)
    }
    fn v(&self, i: usize) -> Value {
        to_value(self.st, &self.a[i])
    }
    fn arr(&self, i: usize) -> Option<ZArray> {
        match self.a.get(i).map(|j| to_value(self.st, j)) {
            Some(Value::Array(a)) => Some(a),
            _ => None,
        }
    }
    fn opt_s(&self, i: usize) -> Option<Vec<u8>> {
        match self.a.get(i) {
            None | Some(J::Null) => None,
            Some(_) => Some(self.s(i)),
        }
    }
}

fn open_v(r: OpenResult) -> V {
    match r {
        OpenResult::Opened => V::Bool(true),
        OpenResult::False => V::Bool(false),
        OpenResult::Error(n) => V::Int(n),
    }
}

fn files_v(r: Option<Vec<Vec<u8>>>) -> V {
    r.map_or(V::Bool(false), |files| {
        V::Arr(
            files
                .into_iter()
                .enumerate()
                .map(|(i, f)| (V::Int(i as i64), V::Str(f)))
                .collect(),
        )
    })
}

/// Calls `method` on `zip` with JSON arguments.
fn dispatch(host: &mut Host, zip: &ZipArchive, method: &str, a: &[J]) -> RtResult<V> {
    let st = Rc::clone(&host.st);
    let a = Args { st: &st, a };
    Ok(match method {
        "open" => open_v(zip.open(host, &a.s(0), a.i(1, 0))?),
        "openString" => open_v(zip.open_string(host, &a.so(0, ""), a.i(1, 0))?),
        "setPassword" => V::Bool(zip.set_password(&a.s(0))?),
        "close" => V::Bool(zip.close(host)?),
        "closeString" => match zip.close_string(host)? {
            // Archives are compared by length: entry mtimes vary.
            Some(s) if a.a.first().is_some_and(|j| j == "len") => V::Int(s.len() as i64),
            Some(s) if a.a.first().is_some_and(|j| j == "same") => {
                V::Bool(s == subst(&st, a.a[1].as_str().unwrap()))
            }
            Some(s) => V::Str(s),
            None => V::Bool(false),
        },
        "count" => V::Int(zip.count()?),
        "getStatusString" => V::s(&zip.get_status_string()),
        "clearError" => {
            zip.clear_error();
            V::Null
        }
        "addEmptyDir" => V::Bool(zip.add_empty_dir(&a.s(0), a.i(1, 0))?),
        "addFromString" => V::Bool(zip.add_from_string(&a.s(0), &a.s(1), a.i(2, 8192))?),
        "addFile" => V::Bool(zip.add_file(
            &a.s(0),
            &a.so(1, ""),
            a.i(2, 0),
            a.i(3, php_zip::consts::LENGTH_TO_END),
            a.i(4, 8192),
        )?),
        "replaceFile" => V::Bool(zip.replace_file(
            &a.s(0),
            a.i(1, 0),
            a.i(2, 0),
            a.i(3, php_zip::consts::LENGTH_TO_END),
            a.i(4, 0),
        )?),
        "addGlob" => {
            let opts = a.arr(2);
            files_v(zip.add_glob(host, &a.s(0), a.i(1, 0), opts.as_ref())?)
        }
        "addPattern" => {
            let opts = a.arr(2);
            files_v(zip.add_pattern(host, &a.s(0), &a.so(1, "."), opts.as_ref())?)
        }
        "renameIndex" => V::Bool(zip.rename_index(a.i(0, 0), &a.s(1))?),
        "renameName" => V::Bool(zip.rename_name(&a.s(0), &a.s(1))?),
        "setArchiveComment" => V::Bool(zip.set_archive_comment(&a.s(0))?),
        "getArchiveComment" => V::opt_str(zip.get_archive_comment(a.i(0, 0))?),
        "setArchiveFlag" => V::Bool(zip.set_archive_flag(a.i(0, 0), a.i(1, 0))?),
        "getArchiveFlag" => V::Int(zip.get_archive_flag(a.i(0, 0), a.i(1, 0))?),
        "setCommentIndex" => V::Bool(zip.set_comment_index(a.i(0, 0), &a.s(1))?),
        "setCommentName" => V::Bool(zip.set_comment_name(&a.s(0), &a.s(1))?),
        "setMtimeIndex" => V::Bool(zip.set_mtime_index(a.i(0, 0), a.i(1, 0), a.i(2, 0))?),
        "setMtimeName" => V::Bool(zip.set_mtime_name(&a.s(0), a.i(1, 0), a.i(2, 0))?),
        "getCommentIndex" => V::opt_str(zip.get_comment_index(a.i(0, 0), a.i(1, 0))?),
        "getCommentName" => V::opt_str(zip.get_comment_name(&a.s(0), a.i(1, 0))?),
        "deleteIndex" => V::Bool(zip.delete_index(a.i(0, 0))?),
        "deleteName" => V::Bool(zip.delete_name(&a.s(0))?),
        "statName" => zip
            .stat_name(&a.s(0), a.i(1, 0))?
            .map_or(V::Bool(false), |s| stat_v(&s)),
        "statIndex" => zip
            .stat_index(a.i(0, 0), a.i(1, 0))?
            .map_or(V::Bool(false), |s| stat_v(&s)),
        "locateName" => V::opt_int(zip.locate_name(&a.s(0), a.i(1, 0))?),
        "getNameIndex" => V::opt_str(zip.get_name_index(a.i(0, 0), a.i(1, 0))?),
        "unchangeArchive" => V::Bool(zip.unchange_archive()?),
        "unchangeAll" => V::Bool(zip.unchange_all()?),
        "unchangeIndex" => V::Bool(zip.unchange_index(a.i(0, 0))?),
        "unchangeName" => V::Bool(zip.unchange_name(&a.s(0))?),
        "extractTo" => {
            let to = a.s(0);
            let files = match a.a.get(1) {
                None | Some(J::Null) => None,
                Some(J::String(_)) => Some(Err(a.s(1))),
                Some(_) => Some(Ok(a.arr(1).unwrap())),
            };
            let sel = match &files {
                None => ExtractFiles::All,
                Some(Err(s)) => ExtractFiles::One(s),
                Some(Ok(arr)) => ExtractFiles::Many(arr),
            };
            V::Bool(zip.extract_to(&to, sel)?)
        }
        "getFromName" => V::opt_str(zip.get_from_name(&a.s(0), a.i(1, 0), a.i(2, 0))?),
        "getFromIndex" => V::opt_str(zip.get_from_index(a.i(0, 0), a.i(1, 0), a.i(2, 0))?),
        "setExternalAttributesName" => {
            V::Bool(zip.set_external_attributes_name(&a.s(0), a.i(1, 0), a.i(2, 0), a.i(3, 0))?)
        }
        "setExternalAttributesIndex" => V::Bool(zip.set_external_attributes_index(
            a.i(0, 0),
            a.i(1, 0),
            a.i(2, 0),
            a.i(3, 0),
        )?),
        "setCompressionName" => V::Bool(zip.set_compression_name(&a.s(0), a.i(1, 0), a.i(2, 0))?),
        "setCompressionIndex" => {
            V::Bool(zip.set_compression_index(a.i(0, 0), a.i(1, 0), a.i(2, 0))?)
        }
        "setEncryptionName" => {
            V::Bool(zip.set_encryption_name(&a.s(0), a.i(1, 0), a.opt_s(2).as_deref())?)
        }
        "setEncryptionIndex" => {
            V::Bool(zip.set_encryption_index(a.i(0, 0), a.i(1, 0), a.opt_s(2).as_deref())?)
        }
        "registerProgressCallback" => V::Bool(zip.register_progress_callback(a.f(0), a.v(1))?),
        "registerCancelCallback" => V::Bool(zip.register_cancel_callback(a.v(0))?),
        other => panic!("unknown method {other}"),
    })
}

fn err_line(e: &RuntimeError) -> String {
    format!(
        "! {}: {}",
        e.exception_class().unwrap_or_else(|| "Fatal".into()),
        e.message().unwrap_or("")
    )
}

fn zip_var(st: &Shared, name: &str) -> Rc<ZipArchive> {
    let s = st.borrow();
    match s.vars.iter().find(|(n, _)| n == name) {
        Some((_, Var::Zip(z))) => Rc::clone(z),
        _ => panic!("no ZipArchive variable {name}"),
    }
}

fn ls(base: &Path, prefix: &str, out: &mut Vec<String>) {
    let d = if prefix.is_empty() {
        base.to_path_buf()
    } else {
        base.join(prefix)
    };
    let mut items: Vec<String> = fs::read_dir(&d)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    items.sort();
    for it in items {
        let p = if prefix.is_empty() {
            it.clone()
        } else {
            format!("{prefix}/{it}")
        };
        let full = base.join(&p);
        if full.is_dir() {
            out.push(format!("{p}/"));
            ls(base, &p, out);
        } else {
            let c = fs::read(&full).unwrap();
            let m = fs::metadata(&full).unwrap();
            let mtime = std::os::unix::fs::MetadataExt::mtime(&m);
            let md5 = md5_hex(&c);
            let mt = if mtime < 1_600_000_000 {
                format!(" mtime={mtime}")
            } else {
                String::new()
            };
            out.push(format!("{p} {} {}{mt}", c.len(), &md5[..8]));
        }
    }
}

fn md5_hex(data: &[u8]) -> String {
    let out = std::process::Command::new("md5sum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.take().unwrap().write_all(data)?;
            c.wait_with_output()
        })
        .unwrap();
    String::from_utf8_lossy(&out.stdout)[..32].to_string()
}

fn deprecation(st: &Shared, func: &str) {
    if let Some(f) = php_zip::class::FUNCTIONS.iter().find(|f| f.name == func) {
        let (since, msg) = f.deprecated.unwrap();
        let line = match msg {
            Some(m) => format!("[Deprecated] Function {func}() is deprecated since {since}, {m}"),
            None => format!("[Deprecated] Function {func}() is deprecated since {since}"),
        };
        emit(st, &line);
    }
}

/// Runs a scenario script and returns its transcript.
pub fn run(script: &str, dir: &Path, fix: &Path) -> String {
    php_streams::wrapper::register_global(b"zip", Rc::new(php_zip::ZipWrapper));
    let st: Shared = Rc::new(RefCell::new(State {
        out: String::new(),
        dir: dir.display().to_string(),
        fix: fix.display().to_string(),
        func: String::new(),
        vars: Vec::new(),
        streams: Vec::new(),
    }));
    let st2 = Rc::clone(&st);
    let prev = diag::set_handler(Some(Box::new(move |d: diag::Diagnostic| {
        let lvl = match d.severity {
            Severity::Warning => "Warning",
            Severity::Notice => "Notice",
            Severity::Deprecated => "Deprecated",
            Severity::Error => "Error",
        };
        let func = st2.borrow().func.clone();
        let params: Vec<String> = d
            .params
            .iter()
            .map(|p| String::from_utf8_lossy(p).to_string())
            .collect();
        emit(
            &st2,
            &format!("[{lvl}] {func}({}): {}", params.join(","), d.text()),
        );
    })));
    let mut host = Host { st: Rc::clone(&st) };
    for line in script.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut cmd: Vec<J> =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("bad line {line}: {e}"));
        if matches!(cmd[0].as_str(), Some("sread" | "sstat" | "sclose"))
            && cmd[1].as_i64() == Some(-1)
        {
            let last = st.borrow().streams.len().saturating_sub(1);
            cmd[1] = J::from(last);
        }
        let op = cmd[0].as_str().unwrap().to_string();
        let rest = &cmd[1..];
        let sname = |i: usize| rest[i].as_str().unwrap().to_string();
        let path = |i: usize| dir.join(rest[i].as_str().unwrap());
        match op.as_str() {
            "echo" => emit(&st, &format!("# {}", sname(0))),
            "new" => {
                let name = sname(0);
                let z = Var::Zip(Rc::new(ZipArchive::new()));
                let mut s = st.borrow_mut();
                if let Some(slot) = s.vars.iter_mut().find(|(n, _)| *n == name) {
                    slot.1 = z;
                } else {
                    s.vars.push((name, z));
                }
            }
            "call" | "extattr" => {
                let z = zip_var(&st, &sname(0));
                let m = sname(1);
                st.borrow_mut().func = format!("ZipArchive::{m}");
                let r = if op == "extattr" {
                    let a = Args {
                        st: &st,
                        a: &rest[2..],
                    };
                    let r = if m == "getExternalAttributesName" {
                        z.get_external_attributes_name(&a.s(0), a.i(1, 0))
                    } else {
                        z.get_external_attributes_index(a.i(0, 0), a.i(1, 0))
                    };
                    r.map(|o| match o {
                        Some((os, at)) => V::Raw(format!("true opsys={os} attr={at}")),
                        None => V::Raw("false opsys=null attr=null".into()),
                    })
                } else {
                    dispatch(&mut host, &z, &m, &rest[2..])
                };
                let v = sname(0);
                match r {
                    Ok(v2) => emit(&st, &format!("{v}->{m} = {}", ex(&v2))),
                    Err(e) => emit(&st, &err_line(&e)),
                }
            }
            "static" => {
                let m = sname(0);
                st.borrow_mut().func = format!("ZipArchive::{m}");
                let a = Args {
                    st: &st,
                    a: &rest[1..],
                };
                let r = match m.as_str() {
                    "isCompressionMethodSupported" => {
                        ZipArchive::is_compression_method_supported(a.i(0, 0), a.b(1, true))
                    }
                    _ => ZipArchive::is_encryption_method_supported(a.i(0, 0), a.b(1, true)),
                };
                match r {
                    Ok(b) => emit(&st, &format!("ZipArchive::{m} = {}", ex(&V::Bool(b)))),
                    Err(e) => emit(&st, &err_line(&e)),
                }
            }
            "prop" => {
                let z = zip_var(&st, &sname(0));
                let v = z.read_property(sname(1).as_bytes()).unwrap();
                emit(
                    &st,
                    &format!("{}->{} = {}", sname(0), sname(1), ex(&from_value(&v))),
                );
            }
            "props" => {
                let z = zip_var(&st, &sname(0));
                let items = z
                    .properties()
                    .into_iter()
                    .map(|(n, v)| (V::s(n), from_value(&v)))
                    .collect();
                emit(&st, &format!("{} props = {}", sname(0), ex(&V::Arr(items))));
            }
            "setprop" => {
                if ZipArchive::is_virtual_property(sname(1).as_bytes()) {
                    emit(
                        &st,
                        &err_line(&php_zip::error::readonly_property("ZipArchive", &sname(1))),
                    );
                } else {
                    emit(&st, &format!("[Deprecated] Creation of dynamic property ZipArchive::${} is deprecated", sname(1)));
                    emit(&st, "set ok");
                }
            }
            "unset" => {
                let name = sname(0);
                let zip = {
                    let s = st.borrow();
                    s.vars
                        .iter()
                        .find(|(n, _)| *n == name)
                        .and_then(|(_, v)| match v {
                            Var::Zip(z) => Some(Rc::clone(z)),
                            _ => None,
                        })
                };
                st.borrow_mut().func = "main".into();
                // The destructor runs while the variable is still reachable
                // (PHP code in callbacks may use it through a WeakReference).
                let failed = zip.as_ref().and_then(|z| z.destroy(&mut host).err());
                drop(zip);
                {
                    let mut s = st.borrow_mut();
                    let pos = s.vars.iter().position(|(n, _)| *n == name).unwrap();
                    s.vars[pos].1 = Var::Gone;
                }
                match failed {
                    Some(e) => emit(&st, &err_line(&e)),
                    None => emit(&st, &format!("unset {name}")),
                }
            }
            "stream" => {
                let z = zip_var(&st, &sname(0));
                let m = sname(1);
                st.borrow_mut().func = format!("ZipArchive::{m}");
                let a = Args {
                    st: &st,
                    a: &rest[2..],
                };
                let r = match m.as_str() {
                    "getStream" => z.get_stream(&a.s(0)),
                    "getStreamName" => z.get_stream_name(&a.s(0), a.i(1, 0)),
                    _ => z.get_stream_index(a.i(0, 0), a.i(1, 0)),
                };
                match r {
                    Ok(Some(s)) => {
                        let n = {
                            let mut sb = st.borrow_mut();
                            sb.streams.push(Some(s));
                            sb.streams.len() - 1
                        };
                        emit(&st, &format!("{}->{m} = stream #{n}", sname(0)));
                    }
                    Ok(None) => emit(&st, &format!("{}->{m} = false", sname(0))),
                    Err(e) => emit(&st, &err_line(&e)),
                }
            }
            "fopen" => {
                let p = subst(&st, rest[0].as_str().unwrap());
                st.borrow_mut().func = "fopen".into();
                let ctx = rest.get(1).map(|j| {
                    let mut c = php_streams::context::StreamContext::new();
                    for (w, opts) in j.as_object().unwrap() {
                        for (k, v) in opts.as_object().unwrap() {
                            let val = match v {
                                J::String(s) => php_streams::value::Value::Str(subst(&st, s)),
                                J::Number(n) => php_streams::value::Value::Int(n.as_i64().unwrap()),
                                _ => php_streams::value::Value::Null,
                            };
                            c.set(w, k, val);
                        }
                    }
                    c.shared()
                });
                let mut opened = None;
                let s = php_streams::open::open_wrapper(
                    &p,
                    "r",
                    php_streams::wrapper::REPORT_ERRORS,
                    &mut opened,
                    ctx.as_ref(),
                );
                match s {
                    Some(s) => {
                        let n = {
                            let mut sb = st.borrow_mut();
                            sb.streams.push(Some(s));
                            sb.streams.len() - 1
                        };
                        emit(&st, &format!("fopen = stream #{n}"));
                    }
                    None => emit(&st, "fopen = false"),
                }
            }
            "sread" => {
                let n = rest[0].as_u64().unwrap() as usize;
                let len = rest.get(1).and_then(J::as_u64);
                st.borrow_mut().func = if len.is_some() {
                    "fread"
                } else {
                    "stream_get_contents"
                }
                .into();
                let Some(mut s) = st.borrow_mut().streams.get_mut(n).and_then(Option::take) else {
                    emit(&st, &format!("read #{n} = no stream"));
                    continue;
                };
                let r = match len {
                    Some(l) => s.read_vec(l as usize),
                    // stream_get_contents() returns "" when nothing could be read.
                    None => Some(s.copy_to_mem(None).unwrap_or_default()),
                };
                let eof = s.eof();
                st.borrow_mut().streams[n] = Some(s);
                let v = r.map_or(V::Bool(false), V::Str);
                emit(
                    &st,
                    &format!("read #{n} = {} eof={}", ex(&v), ex(&V::Bool(eof))),
                );
            }
            "sstat" => {
                let n = rest[0].as_u64().unwrap() as usize;
                st.borrow_mut().func = "fstat".into();
                let Some(mut s) = st.borrow_mut().streams.get_mut(n).and_then(Option::take) else {
                    emit(&st, &format!("fstat #{n} = no stream"));
                    continue;
                };
                let r = s.stat();
                st.borrow_mut().streams[n] = Some(s);
                match r {
                    None => emit(&st, &format!("fstat #{n} = false")),
                    Some(sb) => {
                        let mt = if sb.mtime > 1_600_000_000 {
                            "NOW".to_string()
                        } else {
                            sb.mtime.to_string()
                        };
                        emit(
                            &st,
                            &format!(
                                "fstat #{n} = size={} mode={:o} mtime={mt} nlink={} ino={}",
                                sb.size, sb.mode, sb.nlink, sb.ino
                            ),
                        );
                    }
                }
            }
            "sclose" => {
                let n = rest[0].as_u64().unwrap() as usize;
                st.borrow_mut().func = "fclose".into();
                let s = st.borrow_mut().streams.get_mut(n).and_then(Option::take);
                if let Some(mut s) = s {
                    s.close();
                }
                emit(&st, &format!("fclose #{n} = true"));
            }
            "write" => {
                let p = path(0);
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(&p, subst(&st, rest[1].as_str().unwrap())).unwrap();
            }
            "copy" => {
                let p = path(1);
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::copy(fix.join(rest[0].as_str().unwrap()), &p).unwrap();
            }
            "poke" => {
                let mut raw = fs::read(path(0)).unwrap();
                let sig = subst(&st, rest[1].as_str().unwrap());
                let off = rest[2].as_u64().unwrap() as usize;
                let bytes = subst(&st, rest[3].as_str().unwrap());
                let mut pos = 0;
                while let Some(p) = raw[pos..].windows(sig.len()).position(|w| w == sig) {
                    let at = pos + p + off;
                    raw[at..at + bytes.len()].copy_from_slice(&bytes);
                    pos += p + sig.len();
                }
                fs::write(path(0), raw).unwrap();
            }
            "sub" => {
                let raw = fs::read(path(0)).unwrap();
                let needle = subst(&st, rest[1].as_str().unwrap());
                let rep = subst(&st, rest[2].as_str().unwrap());
                let p = raw.windows(needle.len()).position(|w| w == needle).unwrap();
                let out = [&raw[..p], &rep[..], &raw[p + needle.len()..]].concat();
                fs::write(path(0), out).unwrap();
            }
            "entries" => {
                let z = zip_var(&st, &sname(0));
                for i in 0..z.num_files() {
                    let n = z.get_name_index(i, 0).ok().flatten();
                    emit(&st, &format!("entry {i} = {}", ex(&V::opt_str(n))));
                }
            }
            "open_basedir" => {
                let v = subst(&st, rest[0].as_str().unwrap());
                php_streams::state::update_settings(|s| s.open_basedir = v);
            }
            "mkdir" => fs::create_dir_all(path(0)).unwrap(),
            "chmod" => {
                let mode = u32::from_str_radix(rest[1].as_str().unwrap(), 8).unwrap();
                fs::set_permissions(path(0), fs::Permissions::from_mode(mode)).unwrap();
            }
            "unlink" => fs::remove_file(path(0)).unwrap(),
            "ls" => {
                let mut out = Vec::new();
                ls(&path(0), "", &mut out);
                for l in out {
                    emit(&st, &format!("ls {l}"));
                }
            }
            "cat" => {
                let c = fs::read(path(0)).unwrap();
                emit(&st, &format!("cat {} = {}", sname(0), ex(&V::Str(c))));
            }
            "size" => {
                let v = fs::metadata(path(0)).map_or(V::Bool(false), |m| V::Int(m.len() as i64));
                emit(&st, &format!("size {} = {}", sname(0), ex(&v)));
            }
            "exists" => {
                let v = V::Bool(path(0).exists());
                emit(&st, &format!("exists {} = {}", sname(0), ex(&v)));
            }
            "zip_open" => {
                deprecation(&st, "zip_open");
                st.borrow_mut().func = "zip_open".into();
                match procedural::zip_open(&subst(&st, rest[0].as_str().unwrap())) {
                    Ok(ZipOpenResult::Dir(d)) => {
                        st.borrow_mut().vars.push((sname(1), Var::Dir(d)));
                        emit(&st, "zip_open = resource");
                    }
                    Ok(ZipOpenResult::False) => emit(&st, "zip_open = false"),
                    Ok(ZipOpenResult::Error(n)) => emit(&st, &format!("zip_open = {n}")),
                    Err(e) => emit(&st, &err_line(&e)),
                }
            }
            "zip_read" => {
                deprecation(&st, "zip_read");
                let closed = {
                    let s = st.borrow();
                    matches!(s.vars.iter().find(|(n, _)| *n == sname(0)), Some((_, Var::Dir(d))) if d.is_closed())
                };
                if closed {
                    emit(&st, "! TypeError: zip_read(): supplied resource is not a valid Zip Directory resource");
                    continue;
                }
                let entry = {
                    let s = st.borrow();
                    match s.vars.iter().find(|(n, _)| *n == sname(0)) {
                        Some((_, Var::Dir(d))) => d.read(),
                        _ => None,
                    }
                };
                match entry {
                    Some(e) => {
                        let mut s = st.borrow_mut();
                        if let Some(slot) = s.vars.iter_mut().find(|(n, _)| *n == sname(1)) {
                            slot.1 = Var::Entry(e);
                        } else {
                            s.vars.push((sname(1), Var::Entry(e)));
                        }
                        drop(s);
                        emit(&st, "zip_read = resource");
                    }
                    None => emit(&st, "zip_read = false"),
                }
            }
            "zipfn" => {
                let f = sname(0);
                deprecation(&st, &f);
                let var = rest[1]
                    .as_str()
                    .unwrap()
                    .strip_prefix("var:")
                    .unwrap()
                    .to_string();
                let mut s = st.borrow_mut();
                let slot = s.vars.iter_mut().find(|(n, _)| *n == var).map(|(_, v)| v);
                let v = match (f.as_str(), slot) {
                    ("zip_close", Some(Var::Dir(d))) => {
                        if d.is_closed() {
                            Err("zip_close(): supplied resource is not a valid Zip Directory resource")
                        } else {
                            d.close();
                            Ok(V::Null)
                        }
                    }
                    ("zip_entry_open", _) => {
                        let ev = rest[2]
                            .as_str()
                            .unwrap()
                            .strip_prefix("var:")
                            .unwrap()
                            .to_string();
                        match s.vars.iter().find(|(n, _)| *n == ev) {
                            Some((_, Var::Entry(e))) => Ok(V::Bool(e.open())),
                            _ => Err("zip_entry_open(): supplied resource is not a valid Zip Entry resource"),
                        }
                    }
                    ("zip_entry_close", Some(Var::Entry(e))) => {
                        if e.open() {
                            e.close();
                            Ok(V::Bool(true))
                        } else {
                            Err("zip_entry_close(): supplied resource is not a valid Zip Entry resource")
                        }
                    }
                    ("zip_entry_read", Some(Var::Entry(e))) => {
                        let len = rest.get(2).and_then(J::as_i64).unwrap_or(1024);
                        Ok(V::opt_str(e.read(len)))
                    }
                    ("zip_entry_name", Some(Var::Entry(e))) => Ok(V::opt_str(e.name())),
                    ("zip_entry_filesize", Some(Var::Entry(e))) => Ok(V::opt_int(e.filesize())),
                    ("zip_entry_compressedsize", Some(Var::Entry(e))) => {
                        Ok(V::opt_int(e.compressed_size()))
                    }
                    ("zip_entry_compressionmethod", Some(Var::Entry(e))) => {
                        Ok(e.compression_method().map_or(V::Bool(false), V::s))
                    }
                    _ => Err("bad procedural call"),
                };
                drop(s);
                match v {
                    Ok(v) => emit(&st, &format!("{f} = {}", ex(&v))),
                    Err(m) => emit(&st, &format!("! TypeError: {m}")),
                }
            }
            other => emit(&st, &format!("UNKNOWN OP {other}")),
        }
    }
    // Shutdown: destroy the variables in order, then close leftover streams.
    let vars = std::mem::take(&mut st.borrow_mut().vars);
    st.borrow_mut().func = String::new();
    for (_, v) in vars {
        if let Var::Zip(z) = &v {
            if let Err(e) = z.destroy(&mut host) {
                emit(&st, &err_line(&e));
            }
        }
        drop(v);
    }
    let streams = std::mem::take(&mut st.borrow_mut().streams);
    for mut s in streams.into_iter().flatten() {
        s.close();
    }
    emit(&st, "END");
    php_streams::state::update_settings(|s| s.open_basedir = Vec::new());
    diag::set_handler(prev);
    let out = st.borrow().out.clone();
    out
}
