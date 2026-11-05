//! The php.ini configuration hash, extension queues and request overrides.
//! Reference: `main/php_ini.c`.

use std::cell::RefCell;

use super::{
    parse_ini, Access, IniHooks, IniKey, IniMap, IniParserCallback, IniRegistry, IniScalar,
    IniValue, ScannerMode, Stage,
};
use crate::diag::{Diagnostic, DiagnosticSink};

/// Configuration collected before directive registration.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct IniConfiguration {
    /// Global entries and PATH/HOST arrays share PHP's configuration namespace.
    pub values: IniMap,
    /// Global `extension=` directives, in load order.
    pub extensions: Vec<Vec<u8>>,
    /// Global `zend_extension=` directives, in load order.
    pub zend_extensions: Vec<Vec<u8>>,
    has_per_dir: bool,
    has_per_host: bool,
}

impl IniConfiguration {
    /// Parses a file; entries delivered before a syntax error are retained.
    /// Section state resets for each file, but the configuration hash persists.
    pub fn parse(
        &mut self,
        input: &[u8],
        filename: &str,
        hooks: &mut dyn IniHooks,
    ) -> Result<(), Diagnostic> {
        let shared = RefCell::new(self);
        let mut lookup = Lookup {
            config: &shared,
            hooks,
        };
        let mut collector = Collector {
            config: &shared,
            active: None,
            special: false,
        };
        parse_ini(
            input,
            filename,
            ScannerMode::Normal,
            &mut lookup,
            &mut collector,
        )
    }

    /// Applies PATH sections from outermost directory to innermost.
    /// `path` is an absolute script filename (or a directory ending in `/`).
    pub fn activate_path(
        &self,
        path: &[u8],
        registry: &mut IniRegistry,
        diag: &mut dyn DiagnosticSink,
    ) {
        if !self.has_per_dir || path.len() > 4096 || !path.starts_with(b"/") {
            return;
        }
        for (i, &byte) in path.iter().enumerate().skip(1) {
            if byte == b'/' {
                self.activate(&path[..i], registry, diag);
            }
        }
    }

    /// Applies an exact HOST section; the SAPI supplies a lowercase hostname.
    pub fn activate_host(
        &self,
        host: &[u8],
        registry: &mut IniRegistry,
        diag: &mut dyn DiagnosticSink,
    ) {
        if self.has_per_host && !host.is_empty() {
            self.activate(host, registry, diag);
        }
    }

    fn activate(&self, key: &[u8], registry: &mut IniRegistry, diag: &mut dyn DiagnosticSink) {
        let Some(IniValue::Array(section)) = self.values.get(&IniKey::Str(key.to_vec())) else {
            return;
        };
        for (key, value) in section.iter() {
            if let (IniKey::Str(key), IniValue::Scalar(value)) = (key, value) {
                if let Ok(name) = std::str::from_utf8(key) {
                    registry.alter(
                        name,
                        &value.to_bytes(),
                        Access::SYSTEM,
                        Stage::Activate,
                        false,
                        diag,
                    );
                }
            }
        }
    }
}

struct Lookup<'a, 'b> {
    config: &'a RefCell<&'b mut IniConfiguration>,
    hooks: &'a mut dyn IniHooks,
}

impl IniHooks for Lookup<'_, '_> {
    fn constant(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        self.hooks.constant(name)
    }

    fn variable(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        if let Some(IniValue::Scalar(value)) =
            self.config.borrow().values.get(&IniKey::Str(name.to_vec()))
        {
            return Some(value.to_bytes());
        }
        self.hooks.variable(name)
    }
}

struct Collector<'a, 'b> {
    config: &'a RefCell<&'b mut IniConfiguration>,
    active: Option<IniKey>,
    special: bool,
}

impl Collector<'_, '_> {
    fn with_target(&mut self, f: impl FnOnce(&mut IniMap)) {
        let mut config = self.config.borrow_mut();
        if let Some(key) = &self.active {
            if let Some(IniValue::Array(section)) = config.values.get_mut(key) {
                f(section);
                return;
            }
        }
        f(&mut config.values);
    }
}

impl IniParserCallback for Collector<'_, '_> {
    fn section(&mut self, name: &[u8]) {
        let mut config = self.config.borrow_mut();
        let upper = name.to_ascii_uppercase();
        let mut key = if upper.starts_with(b"PATH") {
            config.has_per_dir = true;
            name[4..].to_vec()
        } else if upper.starts_with(b"HOST") {
            config.has_per_host = true;
            name[4..].to_ascii_lowercase()
        } else {
            self.special = false;
            return;
        };
        self.special = true;
        if key.is_empty() {
            return;
        }
        while matches!(key.last(), Some(b'/' | b'\\')) {
            key.pop();
        }
        let start = key
            .iter()
            .take_while(|&&c| matches!(c, b'=' | b' ' | b'\t'))
            .count();
        let key = IniKey::Str(key[start..].to_vec());
        if config.values.get(&key).is_none() {
            config
                .values
                .insert(key.clone(), IniValue::Array(IniMap::new()));
        }
        if matches!(config.values.get(&key), Some(IniValue::Array(_))) {
            self.active = Some(key);
        }
    }

    fn entry(&mut self, key: &[u8], value: Option<IniScalar>) {
        let Some(value) = value else {
            return;
        };
        if !self.special && key.eq_ignore_ascii_case(b"extension") {
            self.config.borrow_mut().extensions.push(value.to_bytes());
        } else if !self.special && key.eq_ignore_ascii_case(b"zend_extension") {
            self.config
                .borrow_mut()
                .zend_extensions
                .push(value.to_bytes());
        } else {
            self.with_target(|target| {
                target.insert(IniKey::Str(key.to_vec()), IniValue::Scalar(value));
            });
        }
    }

    fn pop_entry(&mut self, key: &[u8], value: IniScalar, offset: &[u8]) {
        self.with_target(|target| {
            let key = IniKey::Str(key.to_vec());
            if !matches!(target.get(&key), Some(IniValue::Array(_))) {
                target.insert(key.clone(), IniValue::Array(IniMap::new()));
            }
            if let Some(IniValue::Array(array)) = target.get_mut(&key) {
                if offset.is_empty() {
                    array.push(IniValue::Scalar(value));
                } else {
                    array.insert(IniKey::symtable(offset), IniValue::Scalar(value));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ini::EnvHooks;

    fn parse(config: &mut IniConfiguration, input: &[u8]) {
        config.parse(input, "php.ini", &mut EnvHooks).unwrap();
    }

    fn scalar(map: &IniMap, name: &str) -> Option<Vec<u8>> {
        match map.get(&IniKey::Str(name.as_bytes().to_vec())) {
            Some(IniValue::Scalar(value)) => Some(value.to_bytes()),
            _ => None,
        }
    }

    #[test]
    fn current_file_interpolation_and_extension_queues() {
        let mut config = IniConfiguration::default();
        parse(
            &mut config,
            b"base=/tmp\nnext=${base}/ext\nextension=one\nEXTENSION=two\nzend_extension=engine\n[PATH=/tmp]\nextension=local\n[HOST=EXAMPLE.COM/]\nzend_extension=local-engine\n",
        );
        assert_eq!(scalar(&config.values, "next"), Some(b"/tmp/ext".to_vec()));
        assert_eq!(config.extensions, vec![b"one".to_vec(), b"two".to_vec()]);
        assert_eq!(config.zend_extensions, vec![b"engine".to_vec()]);
        assert!(scalar(&config.values, "extension").is_none());
        let Some(IniValue::Array(section)) = config.values.get_str("/tmp") else {
            panic!("missing PATH section");
        };
        assert_eq!(scalar(section, "extension"), Some(b"local".to_vec()));
        assert!(config.values.get_str("example.com").is_some());
    }

    #[test]
    fn repeated_sections_arrays_and_file_reset() {
        let mut config = IniConfiguration::default();
        parse(&mut config, b"123=global\nitems=old\nitems[2]=a\nitems[]=b\n[PATH = /app/]\nprecision=8\n[PATH=/app]\nmax_input_vars=7\n[ordinary]\nprecision=9\nextension=global\n");
        let Some(IniValue::Array(items)) = config.values.get_str("items") else {
            panic!("missing array");
        };
        assert_eq!(
            items.get(&IniKey::Int(3)),
            Some(&IniValue::Scalar(IniScalar::Str(b"b".to_vec())))
        );
        assert_eq!(scalar(&config.values, "123"), Some(b"global".to_vec()));
        let Some(IniValue::Array(section)) = config.values.get_str("/app") else {
            panic!("missing merged section");
        };
        assert_eq!(scalar(section, "precision"), Some(b"9".to_vec()));
        assert_eq!(scalar(section, "max_input_vars"), Some(b"7".to_vec()));
        assert_eq!(config.extensions, vec![b"global".to_vec()]);
        parse(&mut config, b"precision=10\ncopy=${123}\n");
        assert_eq!(scalar(&config.values, "precision"), Some(b"10".to_vec()));
        assert_eq!(scalar(&config.values, "copy"), Some(b"global".to_vec()));
    }

    #[test]
    fn path_host_activation_and_deactivation() {
        let mut config = IniConfiguration::default();
        parse(&mut config, b"[PATH=/app]\nprecision=8\n[PATH=/app/sub]\nprecision=9\n[HOST=EXAMPLE.COM]\nprecision=12\n");
        let mut diag = Vec::new();
        let mut registry = IniRegistry::with_core(Vec::<(String, Vec<u8>)>::new(), &mut diag);
        config.activate_path(b"/application/test.php", &mut registry, &mut diag);
        assert_eq!(registry.get_long("precision"), 14);
        config.activate_path(b"/app/sub/test.php", &mut registry, &mut diag);
        assert_eq!(registry.get_long("precision"), 9);
        assert!(registry.ini_set("precision", b"11", &mut diag).is_none());
        config.activate_host(b"example.com", &mut registry, &mut diag);
        assert_eq!(registry.get_long("precision"), 12);
        registry.deactivate(&mut diag);
        assert_eq!(registry.get_long("precision"), 14);
        assert!(registry.ini_set("precision", b"11", &mut diag).is_some());
    }

    #[test]
    fn syntax_error_keeps_preceding_entries() {
        let mut config = IniConfiguration::default();
        assert!(config
            .parse(b"precision=8\nbroken=(\n", "bad.ini", &mut EnvHooks)
            .is_err());
        assert_eq!(scalar(&config.values, "precision"), Some(b"8".to_vec()));
    }

    #[test]
    fn configuration_precedes_environment_and_constants_delegate() {
        let mut config = IniConfiguration::default();
        let mut hooks = crate::ini::FnHooks {
            constant: |name: &[u8]| (name == b"VALUE").then(|| b"7".to_vec()),
            variable: |_name: &[u8]| Some(b"environment".to_vec()),
        };
        config
            .parse(
                b"base=config\nempty=\ncopy=${base}\nblank=${empty}\nexternal=${other}\nnumber=VALUE\n",
                "php.ini",
                &mut hooks,
            )
            .unwrap();
        assert_eq!(scalar(&config.values, "copy"), Some(b"config".to_vec()));
        assert_eq!(scalar(&config.values, "blank"), Some(Vec::new()));
        assert_eq!(
            scalar(&config.values, "external"),
            Some(b"environment".to_vec())
        );
        assert_eq!(scalar(&config.values, "number"), Some(b"7".to_vec()));
    }
}
