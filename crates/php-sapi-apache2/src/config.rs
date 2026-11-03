//! Process-wide configuration: the apache2handler INI defaults, `php.ini`
//! loading (`PHPINIDir`, `$PHPRC`, `$PHP_INI_SCAN_DIR`) and the evaluation
//! of the directives the handler itself consults before a VM exists
//! (`engine`, `xbithack`, `last_modified`).

use crate::backend::DirEntry;
use php_support::ini::{self, IniHooks, IniParserCallback, IniScalar, ScannerMode};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// INI values the CLI SAPI overrides but a web SAPI keeps at PHP's
/// built-in defaults (`php_cli.c`'s `INI_DEFAULT`s undone).  The VM
/// applies these after its CLI defaults.
pub const APACHE_DEFAULTS: &[(&str, &str)] = &[
    ("display_errors", "1"),
    ("html_errors", "1"),
    ("implicit_flush", "0"),
    ("output_buffering", "0"),
    ("max_execution_time", "30"),
    ("max_input_time", "-1"),
    ("register_argc_argv", "0"),
];

/// The configuration shared by all requests of a process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessConfig {
    /// Configuration hash (`VmOptions::ini`): defaults, then `php.ini`.
    pub ini: Vec<(String, String)>,
    /// The loaded `php.ini`, if any (`php_ini_loaded_file()`).
    pub loaded_file: Option<PathBuf>,
    /// Additional scanned `.ini` files.
    pub scanned_files: Vec<PathBuf>,
}

impl ProcessConfig {
    /// The value of `name` in the configuration hash (last one wins).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.ini
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// An INI boolean as seen by a request with `dir` per-directory
    /// entries applied on top (`engine`, `xbithack`, `last_modified`).
    pub fn effective_bool(&self, name: &str, dir: &[DirEntry], default: bool) -> bool {
        if let Some(e) = dir.iter().rev().find(|e| e.name == name) {
            return ini::parse_bool(&e.value);
        }
        self.get(name)
            .map_or(default, |v| ini::parse_bool(v.as_bytes()))
    }
}

static CONFIG: Mutex<Option<Arc<ProcessConfig>>> = Mutex::new(None);

/// Installs the process configuration (module startup).
pub fn install(cfg: ProcessConfig) {
    if let Ok(mut c) = CONFIG.lock() {
        *c = Some(Arc::new(cfg));
    }
}

/// The current process configuration (defaults when never loaded).
pub fn current() -> Arc<ProcessConfig> {
    if let Ok(mut c) = CONFIG.lock() {
        return c
            .get_or_insert_with(|| Arc::new(ProcessConfig::with_defaults()))
            .clone();
    }
    Arc::new(ProcessConfig::with_defaults())
}

impl ProcessConfig {
    /// Only the apache2handler defaults.
    pub fn with_defaults() -> Self {
        ProcessConfig {
            ini: APACHE_DEFAULTS
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            loaded_file: None,
            scanned_files: Vec::new(),
        }
    }

    /// Loads `php.ini` following `php_init_config()`: `$PHPRC`, then the
    /// `PHPINIDir` directory; then `.ini` files in `$PHP_INI_SCAN_DIR`.
    /// Returns the configuration and any warnings (parse errors).
    pub fn load(ini_dir: Option<&Path>) -> (Self, Vec<String>) {
        let mut cfg = Self::with_defaults();
        let mut warnings = Vec::new();
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(rc) = std::env::var_os("PHPRC").filter(|v| !v.is_empty()) {
            candidates.push(PathBuf::from(rc));
        }
        if let Some(d) = ini_dir {
            candidates.push(d.to_path_buf());
        }
        for c in candidates {
            let file = if c.is_dir() { c.join("php.ini") } else { c };
            if file.is_file() {
                if let Err(w) = cfg.load_file(&file) {
                    warnings.push(w);
                }
                cfg.loaded_file = Some(file);
                break;
            }
        }
        if let Some(scan) = std::env::var_os("PHP_INI_SCAN_DIR").filter(|v| !v.is_empty()) {
            for dir in std::env::split_paths(&scan) {
                let Ok(rd) = std::fs::read_dir(&dir) else {
                    continue;
                };
                let mut files: Vec<PathBuf> = rd
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "ini") && p.is_file())
                    .collect();
                files.sort();
                for f in files {
                    if let Err(w) = cfg.load_file(&f) {
                        warnings.push(w);
                    }
                    cfg.scanned_files.push(f);
                }
            }
        }
        (cfg, warnings)
    }

    /// Parses one ini file into the configuration hash.
    pub fn load_file(&mut self, path: &Path) -> Result<(), String> {
        let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.load_str(&data, &path.display().to_string())
    }

    /// Parses ini text into the configuration hash.
    pub fn load_str(&mut self, data: &[u8], filename: &str) -> Result<(), String> {
        let mut collector = Collector::default();
        let r = ini::parse_ini(
            data,
            filename,
            ScannerMode::Normal,
            &mut ConstHooks,
            &mut collector,
        );
        self.ini.extend(collector.entries);
        r.map_err(|d| d.message.trim_end().to_string())
    }
}

/// Collects `key = value` entries outside `[PATH=]`/`[HOST=]` sections.
#[derive(Default)]
struct Collector {
    entries: Vec<(String, String)>,
    skip_section: bool,
}

fn scalar_bytes(v: &IniScalar) -> Vec<u8> {
    match v {
        IniScalar::Str(s) => s.clone(),
        IniScalar::Long(l) => l.to_string().into_bytes(),
        IniScalar::Double(d) => d.to_string().into_bytes(),
        IniScalar::Bool(b) => {
            if *b {
                b"1".to_vec()
            } else {
                Vec::new()
            }
        }
        IniScalar::Null => Vec::new(),
    }
}

impl IniParserCallback for Collector {
    fn section(&mut self, name: &[u8]) {
        let upper = name.to_ascii_uppercase();
        self.skip_section = upper.starts_with(b"PATH=") || upper.starts_with(b"HOST=");
    }

    fn entry(&mut self, key: &[u8], value: Option<IniScalar>) {
        if self.skip_section {
            return;
        }
        let key = String::from_utf8_lossy(key).into_owned();
        if key == "extension" || key == "zend_extension" {
            return;
        }
        let value = value.as_ref().map(scalar_bytes).unwrap_or_default();
        self.entries
            .push((key, String::from_utf8_lossy(&value).into_owned()));
    }

    fn pop_entry(&mut self, _key: &[u8], _value: IniScalar, _offset: &[u8]) {}
}

/// Constants usable in `php.ini` (`E_*` and friends).
struct ConstHooks;

impl IniHooks for ConstHooks {
    fn constant(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        let v: i64 = match name {
            b"E_ERROR" => 1,
            b"E_WARNING" => 2,
            b"E_PARSE" => 4,
            b"E_NOTICE" => 8,
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
            b"PHP_VERSION" => return Some(php_vm::PHP_VERSION.as_bytes().to_vec()),
            _ => return None,
        };
        Some(v.to_string().into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::DirStatus;

    #[test]
    fn parses_ini_with_constants_and_sections() {
        let mut c = ProcessConfig::with_defaults();
        c.load_str(
            b"[PHP]\nerror_reporting = E_ALL & ~E_DEPRECATED\ndisplay_errors = Off\nextension=foo\n[PATH=/x]\nmemory_limit=1\n[Date]\ndate.timezone = \"Europe/Paris\"\n",
            "php.ini",
        )
        .unwrap();
        assert_eq!(c.get("error_reporting"), Some("22527"));
        assert_eq!(c.get("display_errors"), Some(""));
        assert_eq!(c.get("memory_limit"), None);
        assert_eq!(c.get("extension"), None);
        assert_eq!(c.get("date.timezone"), Some("Europe/Paris"));
        assert_eq!(c.get("max_execution_time"), Some("30"));
    }

    #[test]
    fn syntax_errors_are_reported() {
        let mut c = ProcessConfig::with_defaults();
        let e = c.load_str(b"a = (\n", "x.ini").unwrap_err();
        assert!(e.contains("x.ini on line"), "{e}");
    }

    #[test]
    fn effective_bool_uses_dir_entries() {
        let mut c = ProcessConfig::with_defaults();
        c.ini.push(("engine".into(), "0".into()));
        assert!(!c.effective_bool("engine", &[], true));
        let dir = vec![DirEntry {
            name: "engine".into(),
            value: b"1".to_vec(),
            status: DirStatus::PerDir,
            htaccess: true,
        }];
        assert!(c.effective_bool("engine", &dir, true));
        assert!(!c.effective_bool("xbithack", &dir, false));
    }
}
