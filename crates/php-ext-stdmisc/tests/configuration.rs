// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Startup configuration lookup through the registered PHP function.

use php_support::ini::configuration::IniConfiguration;
use php_support::ini::{EnvHooks, IniKey, IniValue};
use php_vm::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run(configuration: Option<&[u8]>, code: &str) -> String {
    let sink = Sink::default();
    let extension = php_ext_stdmisc::StdMisc;
    let mut opts = VmOptions {
        ini: vec![("precision".into(), "8".into())],
    };
    let parsed = configuration.map(|input| {
        let mut config = IniConfiguration::default();
        config.parse(input, "php.ini", &mut EnvHooks).unwrap();
        for (key, value) in config.values.iter() {
            if let (IniKey::Str(key), IniValue::Scalar(value)) = (key, value) {
                opts.ini.push((
                    String::from_utf8_lossy(key).into_owned(),
                    String::from_utf8_lossy(&value.to_bytes()).into_owned(),
                ));
            }
        }
        config
    });
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &[&extension]);
    if let Some(config) = parsed {
        vm.set_configuration(config.values);
    }
    assert_eq!(
        vm.run_script(format!("<?php {code}").as_bytes(), "config.php"),
        0
    );
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).unwrap()
}

#[test]
fn configuration_arrays_sections_and_exact_keys() {
    let output = run(
        Some(b"123=global\nitems[2]=a\nitems[]=b\nitems[02]=c\nitems[-1]=d\n[PATH=/app]\n123=section\nitems[]=nested\n[HOST=EXAMPLE.COM]\nprecision=12\n"),
        r#"
        var_dump(get_cfg_var('123'));
        var_dump(get_cfg_var('items') === [2 => 'a', 3 => 'b', '02' => 'c', -1 => 'd']);
        var_dump(get_cfg_var('/app') === [123 => 'section', 'items' => ['nested']]);
        var_dump(get_cfg_var('example.com') === ['precision' => '12']);
        var_dump(get_cfg_var('EXAMPLE.COM'), get_cfg_var('123' . "\0"));
        "#,
    );
    assert_eq!(
        output,
        "string(6) \"global\"\nbool(true)\nbool(true)\nbool(true)\nbool(false)\nbool(false)\n"
    );
}

#[test]
fn configuration_arrays_are_fresh_and_runtime_ini_is_separate() {
    let output = run(
        Some(b"precision=8\n[PATH=/app]\nitems[]=original\n"),
        r#"
        $a = get_cfg_var('/app');
        $a['items'][0] = 'changed';
        var_dump(get_cfg_var('/app') === ['items' => ['original']]);
        ini_set('precision', '10');
        var_dump(get_cfg_var('precision'), ini_get('precision'));
        "#,
    );
    assert_eq!(output, "bool(true)\nstring(1) \"8\"\nstring(2) \"10\"\n");
}

#[test]
fn configuration_empty_false_missing_and_extension_entries() {
    let output = run(
        Some(b"empty=\nflag=off\nextension=missing\nzend_extension=missing\n[PATH=/app]\nextension=local\n"),
        r#"
        var_dump(get_cfg_var('empty'), get_cfg_var('flag'), get_cfg_var('missing'));
        var_dump(get_cfg_var('extension'), get_cfg_var('zend_extension'));
        var_dump(get_cfg_var('/app') === ['extension' => 'local']);
        var_dump(get_cfg_var('precision'));
        "#,
    );
    assert_eq!(
        output,
        "string(0) \"\"\nstring(0) \"\"\nbool(false)\nbool(false)\nbool(false)\nbool(true)\nbool(false)\n"
    );
}

#[test]
fn configuration_scalar_fallback_for_embedders() {
    let output = run(
        None,
        "ini_set('precision', '10'); var_dump(get_cfg_var('precision'), get_cfg_var('missing'));",
    );
    assert_eq!(output, "string(1) \"8\"\nbool(false)\n");
}

#[test]
fn configuration_numeric_section_keys_distinguish_scalars_and_arrays() {
    let output = run(
        Some(b"[PATH=/app]\n123=scalar\n124[]=array\n"),
        r#"
        foreach (get_cfg_var('/app') as $key => $value) {
            var_dump($key);
        }
        "#,
    );
    assert_eq!(output, "int(123)\nstring(3) \"124\"\n");
}
