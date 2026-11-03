//! INI handling for test runs (ref: `run-tests.php` `$ini_overwrites`, `settings2array()`,
//! `settings2arguments()`, `settings2params()`).

/// `E_ALL` as defined by the php-src version the tests come from (no `E_STRICT`).
pub const E_ALL: u32 = 30719;

/// The INI settings run-tests.php passes to every test with `-d`.
pub fn default_ini_overwrites() -> Vec<String> {
    [
        "output_handler=",
        "open_basedir=",
        "disable_functions=",
        "output_buffering=Off",
        &format!("error_reporting={E_ALL}"),
        "fatal_error_backtraces=Off",
        "display_errors=1",
        "display_startup_errors=1",
        "error_include_args=0",
        "log_errors=0",
        "html_errors=0",
        "track_errors=0",
        "report_zend_debug=0",
        "docref_root=",
        "docref_ext=.html",
        "error_prepend_string=",
        "error_append_string=",
        "auto_prepend_file=",
        "auto_append_file=",
        "ignore_repeated_errors=0",
        "precision=14",
        "serialize_precision=-1",
        "memory_limit=128M",
        "opcache.fast_shutdown=0",
        "opcache.file_update_protection=0",
        "opcache.revalidate_freq=0",
        "opcache.jit_hot_loop=1",
        "opcache.jit_hot_func=1",
        "opcache.jit_hot_return=1",
        "opcache.jit_hot_side_exit=1",
        "opcache.jit_max_root_traces=100000",
        "opcache.jit_max_side_traces=100000",
        "opcache.jit_max_exit_counters=100000",
        "opcache.protect_memory=1",
        "zend.assertions=1",
        "zend.exception_ignore_args=0",
        "zend.exception_string_param_max_len=15",
        "short_open_tag=0",
        "date.timezone=UTC",
    ]
    .iter()
    .map(ToString::to_string)
    .collect()
}

/// An ordered INI settings map with PHP array semantics: re-assigning a key keeps its original
/// position, and `extension` / `zend_extension` accumulate values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IniSettings {
    entries: Vec<(String, Vec<String>)>,
}

impl IniSettings {
    /// Create an empty settings map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or, for extensions, append) a value.
    pub fn set(&mut self, name: &str, value: &str) {
        let multi = name == "extension" || name == "zend_extension";
        if let Some((_, values)) = self.entries.iter_mut().find(|(n, _)| n == name) {
            if multi {
                values.push(value.to_string());
            } else {
                *values = vec![value.to_string()];
            }
        } else {
            self.entries
                .push((name.to_string(), vec![value.to_string()]));
        }
    }

    /// Current value of a single-valued setting.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| v.last())
            .map(String::as_str)
    }

    /// `settings2array()`: apply `name=value` lines; lines without `=` are ignored.
    pub fn apply_lines<'a>(&mut self, lines: impl IntoIterator<Item = &'a str>) {
        for line in lines {
            if let Some((name, value)) = line.split_once('=') {
                self.set(php_trim(name), php_trim(value));
            }
        }
    }

    /// `settings2arguments()`: `-d name=value` pairs.
    pub fn to_arguments(&self) -> Vec<String> {
        let mut args = Vec::new();
        for (name, values) in &self.entries {
            for value in values {
                args.push("-d".to_string());
                args.push(format!("{name}={value}"));
            }
        }
        args
    }

    /// `settings2params()`: the shell-quoted string exported as `TEST_PHP_EXTRA_ARGS`.
    pub fn to_params(&self) -> String {
        let mut out = String::new();
        for (name, values) in &self.entries {
            for value in values {
                out.push_str(&format!(" -d \"{name}={}\"", addslashes(value)));
            }
        }
        out
    }
}

/// PHP `trim()` on a `str`.
fn php_trim(s: &str) -> &str {
    s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\0' | '\x0B'))
}

/// PHP `addslashes()`.
pub fn addslashes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\'' | '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\0' => out.push_str("\\0"),
            _ => out.push(c),
        }
    }
    out
}

/// PHP `escapeshellarg()` (POSIX flavour).
pub fn escapeshellarg(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_and_override() {
        let mut ini = IniSettings::new();
        ini.apply_lines(["a=1", "b = 2 ", "junk", "a=3", "extension=x", "extension=y"]);
        assert_eq!(
            ini.to_arguments(),
            ["-d", "a=3", "-d", "b=2", "-d", "extension=x", "-d", "extension=y"]
        );
        assert_eq!(ini.get("a"), Some("3"));
    }

    #[test]
    fn value_may_contain_equals() {
        let mut ini = IniSettings::new();
        ini.apply_lines(["arg_separator.input=="]);
        assert_eq!(ini.get("arg_separator.input"), Some("="));
    }

    #[test]
    fn params_and_quoting() {
        let mut ini = IniSettings::new();
        ini.set("x", "a\"b");
        assert_eq!(ini.to_params(), " -d \"x=a\\\"b\"");
        assert_eq!(escapeshellarg("it's"), "'it'\\''s'");
    }

    #[test]
    fn defaults_contain_error_reporting() {
        let d = default_ini_overwrites();
        assert!(d.contains(&"error_reporting=30719".to_string()));
        assert_eq!(d.len(), 39);
    }
}
