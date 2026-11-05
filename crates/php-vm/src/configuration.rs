//! Startup configuration returned by `get_cfg_var()`, distinct from INI values.
//! References: `main/php_ini.c`, `ext/standard/basic_functions.c`.

use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};
use php_support::ini::{IniKey, IniMap, IniValue};

use crate::Vm;

impl Vm {
    /// Installs the complete SAPI configuration hash without altering directives.
    /// Runtime `ini_set()` changes do not change this startup snapshot.
    pub fn set_configuration(&mut self, configuration: IniMap) {
        self.configuration = Some(configuration);
    }

    /// Looks up an exact configuration key, returning a fresh array when needed.
    /// Embedders without a complete hash retain the scalar registry lookup.
    pub fn configuration_value(&self, name: &[u8]) -> Option<Value> {
        if let Some(configuration) = &self.configuration {
            return configuration
                .get(&IniKey::Str(name.to_vec()))
                .map(configuration_value);
        }
        let name = String::from_utf8_lossy(name);
        self.ini
            .config_value(&name)
            .map(|v| Value::Str(ZStr::from(v)))
    }
}

fn configuration_value(value: &IniValue) -> Value {
    match value {
        IniValue::Scalar(value) => Value::Str(ZStr::from(value.to_bytes())),
        IniValue::Array(map) => {
            let mut table = HashTable::new();
            for (key, value) in map.iter() {
                let converted = configuration_value(value);
                match key {
                    IniKey::Int(key) => {
                        table.insert(ArrayKey::Int(*key), converted);
                    }
                    IniKey::Str(key) if matches!(value, IniValue::Scalar(_)) => {
                        // add_config_entry() uses symtable semantics only for scalars.
                        table.insert(ArrayKey::from_bytes(key), converted);
                    }
                    IniKey::Str(key) => {
                        table.insert_str_raw(ZStr::from(key.as_slice()), converted);
                    }
                }
            }
            Value::Array(ZArray::from_table(table))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VmOptions;
    use php_support::ini::IniScalar;

    #[test]
    fn configuration_snapshot_uses_exact_bytes_and_replaces_previous_hash() {
        let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
        let mut config = IniMap::new();
        config.insert(
            IniKey::Str(vec![0xff]),
            IniValue::Scalar(IniScalar::Str(b"value".to_vec())),
        );
        vm.set_configuration(config);
        assert!(matches!(
            vm.configuration_value(&[0xff]),
            Some(Value::Str(value)) if value.as_bytes() == b"value"
        ));
        assert!(vm.configuration_value("\u{fffd}".as_bytes()).is_none());
        assert!(vm.configuration_value(&[0xff, 0]).is_none());
        vm.set_configuration(IniMap::new());
        assert!(vm.configuration_value(&[0xff]).is_none());
        assert!(vm.configuration_value(b"precision").is_none());
    }
}
