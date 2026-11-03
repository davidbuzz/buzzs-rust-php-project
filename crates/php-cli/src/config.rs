//! The configuration hash of the CLI SAPI (`php_init_config()` of
//! `main/php_ini.c`): `sapi_cli_ini_defaults`, the `php.ini` search
//! (`-c`, `$PHPRC`, the binary's directory, the configured path; first
//! `php-cli.ini`, then `php.ini`), `$PHP_INI_SCAN_DIR`, then the
//! hard-coded CLI entries and the `-d` defines.

use php_support::ini::{self, IniHooks, IniParserCallback, IniScalar, ScannerMode};
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
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Config {
    /// The configuration hash in insertion order (later entries win).
    pub entries: Vec<(String, String)>,
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
    #[allow(clippy::too_many_lines)]
    pub fn load(opts: &Options) -> Config {
        let mut cfg = Config {
            entries: vec![("display_errors".into(), "1".into())],
            ..Config::default()
        };
        let mut file_name: Option<Vec<u8>> = None;
        let mut search: Vec<Vec<u8>> = Vec::new();
        if let Some(p) = &opts.path_override {
            file_name = Some(p.clone());
            search = split_search_path(p);
        } else if !opts.ignore {
            if let Some(env) = std::env::var_os("PHPRC").filter(|v| !v.is_empty()) {
                let env = env.as_bytes().to_vec();
                search.extend(split_search_path(&env));
                file_name = Some(env);
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
            for name in ["php-cli.ini", "php.ini"] {
                if found.is_some() {
                    break;
                }
                found = search.iter().find_map(|dir| {
                    let p = Path::new(std::ffi::OsStr::from_bytes(dir)).join(name);
                    (p.is_file() && std::fs::File::open(&p).is_ok()).then_some(p)
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
                    cfg.opened_path = Some(shown);
                }
            }
        }
        let scan = std::env::var_os("PHP_INI_SCAN_DIR").map_or_else(
            || CONFIG_FILE_SCAN_DIR.as_bytes().to_vec(),
            |v| v.as_bytes().to_vec(),
        );
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
            cfg.scanned_path = Some(scan);
        }
        for (k, v) in HARDCODED_INI {
            cfg.entries.push(((*k).to_string(), (*v).to_string()));
        }
        for (k, v) in &opts.defines {
            if k.eq_ignore_ascii_case("extension") {
                cfg.extensions.push(v.clone());
            } else if k.eq_ignore_ascii_case("zend_extension") {
                cfg.zend_extensions.push(v.clone());
            } else {
                cfg.entries.push((k.clone(), v.clone()));
            }
        }
        cfg
    }

    /// Parses one ini file into the hash; false on a syntax error.
    fn parse(&mut self, data: &[u8], filename: &str) -> bool {
        let mut collector = Collector {
            cfg: self,
            special_section: false,
        };
        let mut hooks = Hooks {
            entries: Vec::new(),
        };
        hooks.entries.clone_from(&collector.cfg.entries);
        match ini::parse_ini(
            data,
            filename,
            ScannerMode::Normal,
            &mut hooks,
            &mut collector,
        ) {
            Ok(()) => true,
            Err(d) => {
                self.warnings.push(d.message.trim_end().to_string());
                false
            }
        }
    }
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

struct Collector<'a> {
    cfg: &'a mut Config,
    special_section: bool,
}

fn scalar_bytes(v: &IniScalar) -> Vec<u8> {
    match v {
        IniScalar::Str(s) => s.clone(),
        other => other.to_bytes(),
    }
}

impl IniParserCallback for Collector<'_> {
    fn section(&mut self, name: &[u8]) {
        let upper = name.to_ascii_uppercase();
        self.special_section = upper.starts_with(b"PATH") || upper.starts_with(b"HOST");
    }

    fn entry(&mut self, key: &[u8], value: Option<IniScalar>) {
        let Some(value) = value else {
            return;
        };
        if self.special_section {
            return;
        }
        let key = String::from_utf8_lossy(key).into_owned();
        let value = String::from_utf8_lossy(&scalar_bytes(&value)).into_owned();
        if key.eq_ignore_ascii_case("extension") {
            self.cfg.extensions.push(value);
        } else if key.eq_ignore_ascii_case("zend_extension") {
            self.cfg.zend_extensions.push(value);
        } else {
            self.cfg.entries.push((key, value));
        }
    }

    fn pop_entry(&mut self, _key: &[u8], _value: IniScalar, _offset: &[u8]) {}
}

/// Constants and `${var}` lookups available while the configuration is
/// parsed (core constants registered before `php_init_config()`).
struct Hooks {
    entries: Vec<(String, String)>,
}

impl IniHooks for Hooks {
    fn constant(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        startup_constant(name)
    }

    fn variable(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        let n = String::from_utf8_lossy(name);
        if let Some((_, v)) = self.entries.iter().rev().find(|(k, _)| *k == n) {
            return Some(v.as_bytes().to_vec());
        }
        std::env::var_os(n.as_ref()).map(|v| v.as_bytes().to_vec())
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
    fn absolute_paths_are_normalised() {
        assert_eq!(
            absolute(Path::new("/a/./b/../c//d")),
            Some(PathBuf::from("/a/c/d"))
        );
        let long = "./".repeat(MAXPATHLEN);
        assert_eq!(absolute(Path::new(&long)), None);
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
