// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The configuration hash of the CLI SAPI (`php_init_config()` of
//! `main/php_ini.c`): `sapi_cli_ini_defaults`, the `php.ini` search
//! (`-c`, `$PHPRC`, the binary's directory, the configured path; first
//! `php-cli.ini`, then `php.ini`), `$PHP_INI_SCAN_DIR`, then the
//! hard-coded CLI entries and the `-d` defines.

use php_support::ini::configuration::IniConfiguration;
use php_support::ini::{IniHooks, IniKey, IniMap, IniScalar, IniValue};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// `PHP_CONFIG_FILE_PATH` (`--with-config-file-path`).
pub const CONFIG_FILE_PATH: &str = "/usr/local/lib";
/// The built-in `extension_dir` default.
pub const EXTENSION_DIR: &str = "/usr/lib/php/20230831";
/// `MAXPATHLEN`.
const MAXPATHLEN: usize = 4096;
/// `PHP_CONFIG_FILE_SCAN_DIR` (empty: no scan directory configured).
pub const CONFIG_FILE_SCAN_DIR: &str = "";

/// `HARDCODED_INI` of `php_cli.c`: prepended to the `-d` entries, so they
/// override `php.ini` but not `-d`.
pub const HARDCODED_INI: &[(&str, &str)] = &[
    ("html_errors", "0"),
    ("implicit_flush", "1"),
    ("output_buffering", "0"),
    ("max_execution_time", "0"),
    ("max_input_time", "-1"),
];

/// The result of `php_init_config()`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Config {
    /// The configuration hash in insertion order (later entries win).
    pub entries: Vec<(String, String)>,
    /// Complete configuration hash, including arrays and PATH/HOST sections.
    /// CLI requests do not activate per-directory or per-host overrides.
    pub configuration: IniConfiguration,
    /// `php_ini_opened_path`.
    pub opened_path: Option<Vec<u8>>,
    /// `php_ini_scanned_path`.
    pub scanned_path: Option<Vec<u8>>,
    /// `php_ini_scanned_files` (each file followed by `",\n"`, the last
    /// by `"\n"`).
    pub scanned_files: Option<Vec<u8>>,
    /// `extension=` entries (not stored in the configuration hash).
    pub extensions: Vec<String>,
    /// `zend_extension=` entries.
    pub zend_extensions: Vec<String>,
    /// Warnings raised while parsing (`PHP Warning:  ...`).
    pub warnings: Vec<String>,
}

/// How the configuration is searched.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// `-c <path>|<file>`.
    pub path_override: Option<Vec<u8>>,
    /// `-n`.
    pub ignore: bool,
    /// The `-d` defines, already turned into `name=value` pairs.
    pub defines: Vec<(String, String)>,
    /// `PG(php_binary)`.
    pub binary: Option<PathBuf>,
    /// The CGI SAPI (`php-cgi`): `php-cgi.ini`, no `sapi_cli_ini_defaults`
    /// and no `HARDCODED_INI`.
    pub cgi: bool,
}

impl Config {
    /// The value of `name` in the configuration hash (last one wins).
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Builds the configuration hash.
    #[must_use]
    pub fn load(opts: &Options) -> Config {
        let phprc = std::env::var_os("PHPRC").map(|v| v.as_bytes().to_vec());
        let scan = std::env::var_os("PHP_INI_SCAN_DIR").map_or_else(
            || CONFIG_FILE_SCAN_DIR.as_bytes().to_vec(),
            |v| v.as_bytes().to_vec(),
        );
        Self::load_from_environment(opts, phprc.as_deref(), &scan)
    }

    #[allow(clippy::too_many_lines)]
    fn load_from_environment(opts: &Options, phprc: Option<&[u8]>, scan: &[u8]) -> Config {
        let mut cfg = Config::default();
        if !opts.cgi {
            cfg.entries.push(("display_errors".into(), "1".into()));
            cfg.configuration.values.insert(
                IniKey::Str(b"display_errors".to_vec()),
                IniValue::Scalar(IniScalar::Str(b"1".to_vec())),
            );
        }
        let mut file_name: Option<Vec<u8>> = None;
        let mut search: Vec<Vec<u8>> = Vec::new();
        if let Some(p) = &opts.path_override {
            file_name = Some(p.clone());
            search = split_search_path(p);
        } else if !opts.ignore {
            if let Some(env) = phprc.filter(|v| !v.is_empty()) {
                search.extend(split_search_path(env));
                file_name = Some(env.to_vec());
            }
            if let Some(dir) = opts.binary.as_ref().and_then(|b| b.parent()) {
                if !dir.as_os_str().is_empty() {
                    search.push(dir.as_os_str().as_bytes().to_vec());
                }
            }
            search.push(CONFIG_FILE_PATH.as_bytes().to_vec());
        }
        if !opts.ignore || opts.path_override.is_some() {
            let mut found: Option<PathBuf> = None;
            if let Some(f) = file_name.as_ref().filter(|f| !f.is_empty()) {
                let p = Path::new(std::ffi::OsStr::from_bytes(f));
                if p.metadata().is_ok_and(|m| !m.is_dir()) && std::fs::File::open(p).is_ok() {
                    // A path `expand_filepath()` rejects is not used.
                    found = absolute(p);
                }
            }
            let sapi_ini = if opts.cgi {
                "php-cgi.ini"
            } else {
                "php-cli.ini"
            };
            for name in [sapi_ini, "php.ini"] {
                if found.is_some() {
                    break;
                }
                found = search.iter().find_map(|dir| {
                    let p = Path::new(std::ffi::OsStr::from_bytes(dir)).join(name);
                    if p.is_file() && std::fs::File::open(&p).is_ok() {
                        absolute(&p)
                    } else {
                        None
                    }
                });
            }
            if let Some(p) = found {
                if let Ok(data) = std::fs::read(&p) {
                    let shown = p.as_os_str().as_bytes().to_vec();
                    cfg.parse(&data, &String::from_utf8_lossy(&shown));
                    cfg.entries.push((
                        "cfg_file_path".into(),
                        String::from_utf8_lossy(&shown).into_owned(),
                    ));
                    cfg.configuration.values.insert(
                        IniKey::Str(b"cfg_file_path".to_vec()),
                        IniValue::Scalar(IniScalar::Str(shown.clone())),
                    );
                    cfg.opened_path = Some(shown);
                }
            }
        }
        if !opts.ignore && !scan.is_empty() {
            let mut files: Vec<Vec<u8>> = Vec::new();
            for dir in scan.split(|&c| c == b':') {
                let dir = if dir.is_empty() {
                    CONFIG_FILE_SCAN_DIR.as_bytes()
                } else {
                    dir
                };
                if dir.is_empty() {
                    continue;
                }
                let Ok(rd) = std::fs::read_dir(std::ffi::OsStr::from_bytes(dir)) else {
                    continue;
                };
                let mut names: Vec<Vec<u8>> = rd
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().as_bytes().to_vec())
                    .filter(|n| n.ends_with(b".ini"))
                    .collect();
                names.sort();
                for n in names {
                    let mut full = dir.to_vec();
                    if !full.ends_with(b"/") {
                        full.push(b'/');
                    }
                    full.extend_from_slice(&n);
                    let p = Path::new(std::ffi::OsStr::from_bytes(&full));
                    if !p.is_file() {
                        continue;
                    }
                    if let Ok(data) = std::fs::read(p) {
                        if cfg.parse(&data, &String::from_utf8_lossy(&full)) {
                            files.push(full);
                        }
                    }
                }
            }
            if !files.is_empty() {
                let mut s = Vec::new();
                for (i, f) in files.iter().enumerate() {
                    s.extend_from_slice(f);
                    s.extend_from_slice(if i + 1 < files.len() { b",\n" } else { b"\n" });
                }
                cfg.scanned_files = Some(s);
            }
            cfg.scanned_path = Some(scan.to_vec());
        }
        let hardcoded = if opts.cgi { &[][..] } else { HARDCODED_INI };
        for (k, v) in hardcoded {
            cfg.entries.push(((*k).to_string(), (*v).to_string()));
            cfg.configuration.values.insert(
                IniKey::Str(k.as_bytes().to_vec()),
                IniValue::Scalar(IniScalar::Str(v.as_bytes().to_vec())),
            );
        }
        for (k, v) in &opts.defines {
            if k.eq_ignore_ascii_case("extension") {
                cfg.extensions.push(v.clone());
                cfg.configuration.extensions.push(v.as_bytes().to_vec());
            } else if k.eq_ignore_ascii_case("zend_extension") {
                cfg.zend_extensions.push(v.clone());
                cfg.configuration
                    .zend_extensions
                    .push(v.as_bytes().to_vec());
            } else if let Some((base, offset)) = array_define(k) {
                // `ary[a]=1` (`ZEND_INI_PARSER_POP_ENTRY` in
                // `php_ini_parser_cb`): an array in the configuration hash
                // only, not a directive value.
                let key = IniKey::Str(base.as_bytes().to_vec());
                let values = &mut cfg.configuration.values;
                if !matches!(values.get(&key), Some(IniValue::Array(_))) {
                    values.insert(key.clone(), IniValue::Array(IniMap::new()));
                }
                if let Some(IniValue::Array(array)) = values.get_mut(&key) {
                    let value = IniValue::Scalar(IniScalar::Str(v.as_bytes().to_vec()));
                    if offset.is_empty() {
                        array.push(value);
                    } else {
                        array.insert(IniKey::symtable(offset.as_bytes()), value);
                    }
                }
            } else {
                cfg.entries.push((k.clone(), v.clone()));
                cfg.configuration.values.insert(
                    IniKey::Str(k.as_bytes().to_vec()),
                    IniValue::Scalar(IniScalar::Str(v.as_bytes().to_vec())),
                );
            }
        }
        cfg
    }

    /// Parses one ini file into the hash; false on a syntax error.
    fn parse(&mut self, data: &[u8], filename: &str) -> bool {
        let result = self.configuration.parse(data, filename, &mut Hooks);
        self.entries = self
            .configuration
            .values
            .iter()
            .filter_map(|(key, value)| {
                if let (IniKey::Str(key), IniValue::Scalar(value)) = (key, value) {
                    Some((
                        String::from_utf8_lossy(key).into_owned(),
                        String::from_utf8_lossy(&value.to_bytes()).into_owned(),
                    ))
                } else {
                    None
                }
            })
            .collect();
        self.extensions = self
            .configuration
            .extensions
            .iter()
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .collect();
        self.zend_extensions = self
            .configuration
            .zend_extensions
            .iter()
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .collect();
        match result {
            Ok(()) => true,
            Err(d) => {
                self.warnings.push(d.message.trim_end().to_string());
                false
            }
        }
    }
}

/// The line `php_ini_builder_define()` appends for one `-d` argument: a
/// value starting with a character that is neither alphanumeric nor a
/// quote is quoted, a bare name gets `=1` (as `php_vm::parse_cli_define`
/// builds it).
fn define_line(arg: &[u8]) -> Vec<u8> {
    let mut line = Vec::with_capacity(arg.len() + 4);
    match arg.iter().position(|&c| c == b'=') {
        Some(p) => {
            let val = &arg[p + 1..];
            match val.first() {
                Some(&c) if !c.is_ascii_alphanumeric() && c != b'"' && c != b'\'' => {
                    line.extend_from_slice(&arg[..=p]);
                    line.push(b'"');
                    line.extend_from_slice(val);
                    line.push(b'"');
                }
                _ => line.extend_from_slice(arg),
            }
        }
        None => {
            line.extend_from_slice(arg);
            line.extend_from_slice(b"=1");
        }
    }
    line.push(b'\n');
    line
}

/// Ignores what the syntax check parses.
struct NoEntries;

impl php_support::ini::IniParserCallback for NoEntries {
    fn section(&mut self, _name: &[u8]) {}
    fn entry(&mut self, _key: &[u8], _value: Option<IniScalar>) {}
    fn pop_entry(&mut self, _key: &[u8], _value: IniScalar, _offset: &[u8]) {}
}

/// php_cli.c joins `HARDCODED_INI` and every `-d` line into one buffer
/// (`sapi_module.ini_entries`), which `php_init_config()` parses after
/// the ini files. A syntax error stops that parse: it is reported as a
/// warning naming the buffer's line ("syntax error, unexpected '=' in
/// Unknown on line 6" for the first `-d`), and the defines after the one
/// that holds it are not applied. Returns how many defines apply (the
/// failing one keeps what precedes its error) and the warning.
#[must_use]
pub fn defines_syntax_error(args: &[Vec<u8>], cgi: bool) -> Option<(usize, String)> {
    let hardcoded = if cgi { &[][..] } else { HARDCODED_INI };
    let mut buf = Vec::new();
    for (k, v) in hardcoded {
        buf.extend_from_slice(k.as_bytes());
        buf.push(b'=');
        buf.extend_from_slice(v.as_bytes());
        buf.push(b'\n');
    }
    let mut first_line = hardcoded.len() + 1;
    let mut starts = Vec::with_capacity(args.len());
    for arg in args {
        let line = define_line(arg);
        starts.push(first_line);
        first_line += line.iter().filter(|&&c| c == b'\n').count();
        buf.extend_from_slice(&line);
    }
    let err = php_support::ini::parse_ini(
        &buf,
        "Unknown",
        php_support::ini::ScannerMode::Normal,
        &mut Hooks,
        &mut NoEntries,
    )
    .err()?;
    let line = err
        .message
        .trim_end()
        .rsplit_once(" on line ")
        .and_then(|(_, n)| n.parse::<usize>().ok())
        .unwrap_or(first_line);
    let failing = starts.iter().rposition(|&s| s <= line).unwrap_or(0);
    Some((failing + 1, err.message.trim_end().to_string()))
}

/// Splits a `-d` define name `base[offset]` (see `php_vm::parse_cli_define`).
fn array_define(name: &str) -> Option<(&str, &str)> {
    let (base, offset) = name.strip_suffix(']')?.split_once('[')?;
    (!base.is_empty()).then_some((base, offset))
}

fn split_search_path(p: &[u8]) -> Vec<Vec<u8>> {
    p.split(|&c| c == b':')
        .filter(|s| !s.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

/// `expand_filepath()`: the path made absolute against the working
/// directory and normalised lexically (`.`/`..`, repeated slashes);
/// `None` when the result would not fit in `MAXPATHLEN`.
fn absolute(p: &Path) -> Option<PathBuf> {
    let raw = p.as_os_str().as_bytes();
    let mut full: Vec<u8> = Vec::new();
    if !raw.starts_with(b"/") {
        let cwd = std::env::current_dir().ok()?;
        full.extend_from_slice(cwd.as_os_str().as_bytes());
        full.push(b'/');
    }
    full.extend_from_slice(raw);
    if full.len() >= MAXPATHLEN - 1 {
        return None;
    }
    let mut parts: Vec<&[u8]> = Vec::new();
    for seg in full.split(|&c| c == b'/') {
        match seg {
            b"" | b"." => {}
            b".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let mut out = Vec::with_capacity(full.len());
    for s in parts {
        out.push(b'/');
        out.extend_from_slice(s);
    }
    if out.is_empty() {
        out.push(b'/');
    }
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&out)))
}

/// Constants and `${var}` lookups available while the configuration is
/// parsed (core constants registered before `php_init_config()`).
struct Hooks;

impl IniHooks for Hooks {
    fn constant(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        startup_constant(name)
    }
}

/// Core constants usable in ini files.
#[must_use]
pub fn startup_constant(name: &[u8]) -> Option<Vec<u8>> {
    let v: i64 = match name {
        b"E_ERROR" => 1,
        b"E_WARNING" => 2,
        b"E_PARSE" => 4,
        b"E_NOTICE" | b"PHP_INT_SIZE" => 8,
        b"E_CORE_ERROR" => 16,
        b"E_CORE_WARNING" => 32,
        b"E_COMPILE_ERROR" => 64,
        b"E_COMPILE_WARNING" => 128,
        b"E_USER_ERROR" => 256,
        b"E_USER_WARNING" => 512,
        b"E_USER_NOTICE" => 1024,
        b"E_STRICT" => 2048,
        b"E_RECOVERABLE_ERROR" => 4096,
        b"E_DEPRECATED" => 8192,
        b"E_USER_DEPRECATED" => 16384,
        b"E_ALL" => 30719,
        b"PHP_INT_MAX" => i64::MAX,
        b"PHP_DEBUG" | b"PHP_ZTS" => 0,
        b"PHP_VERSION" => return Some(php_vm::PHP_VERSION.as_bytes().to_vec()),
        b"PHP_OS" | b"PHP_OS_FAMILY" => return Some(b"Linux".to_vec()),
        b"PHP_EOL" => return Some(b"\n".to_vec()),
        b"DEFAULT_INCLUDE_PATH" => return Some(b".:/usr/local/lib/php".to_vec()),
        _ => return None,
    };
    Some(v.to_string().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("php-cli-config-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    #[test]
    fn ignore_gives_defaults_hardcoded_and_defines() {
        let cfg = Config::load(&Options {
            ignore: true,
            defines: vec![
                ("precision".into(), "10".into()),
                ("extension".into(), "x.so".into()),
            ],
            ..Options::default()
        });
        assert_eq!(cfg.get("display_errors"), Some("1"));
        assert_eq!(cfg.get("html_errors"), Some("0"));
        assert_eq!(cfg.get("precision"), Some("10"));
        assert_eq!(cfg.extensions, vec!["x.so".to_string()]);
        assert!(cfg.opened_path.is_none());
    }

    /// `-d ary[a]=1`: an array in the configuration hash
    /// (`get_cfg_var_array.phpt`), not a directive.
    #[test]
    fn array_defines() {
        let cfg = Config::load(&Options {
            ignore: true,
            defines: vec![
                ("ary[a]".into(), "1".into()),
                ("ary[b]".into(), "2".into()),
                ("ary2[]".into(), "x".into()),
            ],
            ..Options::default()
        });
        assert_eq!(cfg.get("ary[a]"), None);
        let Some(IniValue::Array(a)) = cfg.configuration.values.get(&IniKey::Str(b"ary".to_vec()))
        else {
            panic!("ary is not an array");
        };
        assert_eq!(a.len(), 2);
        assert!(matches!(
            a.get(&IniKey::symtable(b"b")),
            Some(IniValue::Scalar(IniScalar::Str(v))) if v == b"2"
        ));
        let Some(IniValue::Array(a2)) = cfg.configuration.values.get_str("ary2") else {
            panic!("ary2 is not an array");
        };
        assert!(a2.get(&IniKey::Int(0)).is_some());
        assert_eq!(array_define("x[]"), Some(("x", "")));
        assert_eq!(array_define("[a]"), None);
    }

    #[test]
    fn path_override_file_and_sections() {
        let d = tmpdir("override");
        let f = d.join("my.ini");
        std::fs::write(
            &f,
            "max_input_vars = 4\nerror_reporting = E_ALL & ~E_NOTICE\nhtml_errors=1\n[PATH=/x]\nmax_input_vars = 5\n[HOST=h]\nmax_input_vars = 7\n",
        )
        .unwrap();
        let cfg = Config::load(&Options {
            path_override: Some(f.as_os_str().as_bytes().to_vec()),
            ignore: true,
            ..Options::default()
        });
        assert_eq!(cfg.get("max_input_vars"), Some("4"));
        assert_eq!(cfg.get("error_reporting"), Some("30711"));
        // The hard-coded CLI entries win over php.ini.
        assert_eq!(cfg.get("html_errors"), Some("0"));
        assert!(cfg.opened_path.is_some());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn directory_override_finds_php_ini() {
        let d = tmpdir("dir");
        std::fs::write(d.join("php.ini"), "foo=bar\n").unwrap();
        let cfg = Config::load(&Options {
            path_override: Some(d.as_os_str().as_bytes().to_vec()),
            ..Options::default()
        });
        assert_eq!(cfg.get("foo"), Some("bar"));
        assert_eq!(
            cfg.get("cfg_file_path").map(|s| s.ends_with("php.ini")),
            Some(true)
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn binary_directory_discovery_normalises_paths() {
        let d = tmpdir("binary");
        std::fs::write(d.join("php-cli.ini"), "source=binary\n").unwrap();
        let cfg = Config::load_from_environment(
            &Options {
                binary: Some(d.join(".").join("php")),
                ..Options::default()
            },
            None,
            b"",
        );
        assert_eq!(cfg.get("source"), Some("binary"));
        assert_eq!(
            cfg.opened_path,
            Some(d.join("php-cli.ini").as_os_str().as_bytes().to_vec())
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn phprc_and_sapi_file_precedence() {
        let d = tmpdir("phprc");
        std::fs::write(d.join("php.ini"), "source=generic\n").unwrap();
        std::fs::write(d.join("php-cli.ini"), "source=cli\n").unwrap();
        let opts = Options::default();
        let cfg = Config::load_from_environment(&opts, Some(d.as_os_str().as_bytes()), b"");
        assert_eq!(cfg.get("source"), Some("cli"));
        assert_eq!(
            cfg.opened_path,
            Some(d.join("php-cli.ini").as_os_str().as_bytes().to_vec())
        );
        let explicit = d.join("php.ini");
        let cfg = Config::load_from_environment(&opts, Some(explicit.as_os_str().as_bytes()), b"");
        assert_eq!(cfg.get("source"), Some("generic"));
        let cfg = Config::load_from_environment(
            &Options {
                path_override: Some(explicit.as_os_str().as_bytes().to_vec()),
                ..Options::default()
            },
            Some(d.as_os_str().as_bytes()),
            b"",
        );
        assert_eq!(cfg.get("source"), Some("generic"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn scan_order_interpolation_sections_and_defines() {
        let d = tmpdir("scan");
        let primary = d.join("primary.ini");
        let scan = d.join("conf.d");
        std::fs::create_dir_all(&scan).unwrap();
        std::fs::write(
            &primary,
            "base=/first\nextension=first\n[PATH=/app]\nprecision=8\n",
        )
        .unwrap();
        std::fs::write(scan.join("20-last.ini"), "precision=10\nextension=last\n").unwrap();
        std::fs::write(
            scan.join("10-first.ini"),
            "copy=${base}/copy\nprecision=9\nextension=middle\n[HOST=EXAMPLE.COM]\nprecision=12\n",
        )
        .unwrap();
        std::fs::write(scan.join("30-bad.ini"), "partial=kept\nbroken=(\n").unwrap();
        std::fs::write(scan.join("ignored.INI"), "precision=2\n").unwrap();
        let cfg = Config::load_from_environment(
            &Options {
                path_override: Some(primary.as_os_str().as_bytes().to_vec()),
                defines: vec![
                    ("precision".into(), "11".into()),
                    ("extension".into(), "defined".into()),
                ],
                ..Options::default()
            },
            None,
            scan.as_os_str().as_bytes(),
        );
        assert_eq!(cfg.get("copy"), Some("/first/copy"));
        assert_eq!(cfg.get("precision"), Some("11"));
        assert_eq!(cfg.get("partial"), Some("kept"));
        assert_eq!(cfg.extensions, vec!["first", "middle", "last", "defined"]);
        assert!(cfg.get("extension").is_none());
        assert!(cfg.configuration.values.get_str("/app").is_some());
        assert!(cfg.configuration.values.get_str("example.com").is_some());
        assert_eq!(cfg.warnings.len(), 1);
        assert_eq!(
            cfg.scanned_files,
            Some(
                format!(
                    "{}/10-first.ini,\n{}/20-last.ini\n",
                    scan.display(),
                    scan.display()
                )
                .into_bytes()
            )
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn new_vm_preserves_non_utf8_configuration_across_scan_files() {
        use php_runtime::{Value, ZStr};

        let d = tmpdir("non-utf8");
        let primary = d.join("primary.ini");
        let scan = d.join("conf.d");
        std::fs::create_dir_all(&scan).unwrap();
        std::fs::write(
            &primary,
            b"key\xff=\"value\xfe\"\nplain=\"value\xfd\"\nextension=\"ext\xfc\"\n",
        )
        .unwrap();
        std::fs::write(
            scan.join("10-copy.ini"),
            b"copy=${plain}\nsecond\xfe=\"scan\xfc\"\n",
        )
        .unwrap();
        std::fs::write(
            scan.join("20-copy.ini"),
            b"again=${copy}\nzend_extension=\"engine\xfb\"\n",
        )
        .unwrap();
        let cfg = Config::load_from_environment(
            &Options {
                path_override: Some(primary.as_os_str().as_bytes().to_vec()),
                defines: vec![("precision".into(), "11".into())],
                ..Options::default()
            },
            None,
            scan.as_os_str().as_bytes(),
        );
        assert_eq!(cfg.configuration.extensions, vec![b"ext\xfc".to_vec()]);
        assert_eq!(
            cfg.configuration.zend_extensions,
            vec![b"engine\xfb".to_vec()]
        );
        // The scalar registry still uses its existing lossy String representation.
        assert_eq!(cfg.get("plain"), Some("value\u{fffd}"));
        let mut vm = crate::new_vm(&cfg);
        for (key, expected) in [
            (&b"key\xff"[..], &b"value\xfe"[..]),
            (&b"plain"[..], &b"value\xfd"[..]),
            (&b"copy"[..], &b"value\xfd"[..]),
            (&b"again"[..], &b"value\xfd"[..]),
            (&b"second\xfe"[..], &b"scan\xfc"[..]),
            (&b"precision"[..], &b"11"[..]),
            (&b"html_errors"[..], &b"0"[..]),
        ] {
            let value = vm
                .call_function("get_cfg_var", vec![Value::Str(ZStr::from(key))])
                .unwrap();
            assert!(matches!(value, Value::Str(value) if value.as_bytes() == expected));
        }
        for key in ["key\u{fffd}", "second\u{fffd}"] {
            let value = vm
                .call_function("get_cfg_var", vec![Value::from(key)])
                .unwrap();
            assert!(matches!(value, Value::Bool(false)));
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn ignore_still_loads_override_but_not_scan() {
        let d = tmpdir("ignore-scan");
        let primary = d.join("primary.ini");
        std::fs::write(&primary, "precision=8\n").unwrap();
        std::fs::write(d.join("extra.ini"), "precision=9\n").unwrap();
        let cfg = Config::load_from_environment(
            &Options {
                path_override: Some(primary.as_os_str().as_bytes().to_vec()),
                ignore: true,
                ..Options::default()
            },
            None,
            d.as_os_str().as_bytes(),
        );
        assert_eq!(cfg.get("precision"), Some("8"));
        assert!(cfg.scanned_path.is_none());
        assert!(cfg.scanned_files.is_none());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn absolute_paths_are_normalised() {
        assert_eq!(
            absolute(Path::new("/a/./b/../c//d")),
            Some(PathBuf::from("/a/c/d"))
        );
        let long = "./".repeat(MAXPATHLEN);
        assert_eq!(absolute(Path::new(&long)), None);
    }

    #[test]
    fn define_syntax_errors_name_the_buffer_line() {
        // php8.5 -n -d =x: "syntax error, unexpected '=' in Unknown on
        // line 6" (five HARDCODED_INI lines come first).
        let args = |a: &[&str]| a.iter().map(|s| s.as_bytes().to_vec()).collect::<Vec<_>>();
        assert_eq!(
            defines_syntax_error(&args(&["=x"]), false),
            Some((
                1,
                "syntax error, unexpected '=' in Unknown on line 6".to_string()
            ))
        );
        // php8.5 -d precision=3 -d =x -d serialize_precision=4: line 7;
        // precision applies, serialize_precision does not.
        assert_eq!(
            defines_syntax_error(
                &args(&["precision=3", "=x", "serialize_precision=4"]),
                false
            ),
            Some((
                2,
                "syntax error, unexpected '=' in Unknown on line 7".to_string()
            ))
        );
        assert_eq!(
            defines_syntax_error(&args(&["precision=3", "a", "b=\"c d\""]), false),
            None
        );
        assert_eq!(define_line(b"x=/tmp"), b"x=\"/tmp\"\n");
        assert_eq!(define_line(b"flag"), b"flag=1\n");
    }

    #[test]
    fn syntax_errors_become_warnings() {
        let d = tmpdir("err");
        let f = d.join("bad.ini");
        std::fs::write(&f, "a = (\n").unwrap();
        let cfg = Config::load(&Options {
            path_override: Some(f.as_os_str().as_bytes().to_vec()),
            ..Options::default()
        });
        assert_eq!(cfg.warnings.len(), 1);
        let _ = std::fs::remove_dir_all(d);
    }
}
