// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end tests: PHP scripts run on a VM with the `curl` extension
//! (plus `streams`), using `file://` transfers. Expected outputs come from
//! `/usr/bin/php8.5`.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use php_vm::ext::Extension;
use php_vm::{Vm, VmOptions};

/// A `Write` sink shared with the test.
#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Runs `code` (without `<?php`) and returns its output.
fn run(code: &str) -> String {
    let sink = Sink::default();
    let (streams, curl) = (php_ext_streams::Streams, php_ext_curl::Curl);
    let exts: [&dyn Extension; 2] = [&streams, &curl];
    let opts = VmOptions { ini: vec![] };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

fn temp_file(name: &str, data: &str) -> String {
    let p = std::env::temp_dir().join(format!("php-ext-curl-{}-{name}", std::process::id()));
    std::fs::write(&p, data).expect("write");
    p.display().to_string()
}

#[test]
fn exec_returntransfer_and_callbacks() {
    let f = temp_file("a", "hello");
    let out = run(&format!(
        r#"$ch = curl_init("file://{f}");
var_dump($ch);
curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
var_dump(curl_exec($ch), curl_errno($ch), curl_error($ch));
curl_setopt($ch, CURLOPT_WRITEFUNCTION, function ($h, $d) {{ echo "cb:", $d, "\n"; return strlen($d); }});
var_dump(curl_exec($ch));
var_dump(curl_getinfo($ch, CURLINFO_EFFECTIVE_URL) === "file://{f}");
var_dump((int) $ch);
"#
    ));
    assert_eq!(
        out,
        "object(CurlHandle)#1 (0) {\n}\nstring(5) \"hello\"\nint(0)\nstring(0) \"\"\ncb:hello\nbool(true)\nbool(true)\nint(1)\n"
    );
    let _ = std::fs::remove_file(f);
}

#[test]
fn errors_and_validation() {
    let out = run(r#"$ch = curl_init();
try { curl_setopt($ch, 1000, 0); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
try { curl_setopt_array($ch, ["x" => 1]); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
try { curl_setopt($ch, CURLOPT_WRITEFUNCTION, "nope"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { curl_setopt($ch, CURLOPT_URL, "a\0b"); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
try { curl_setopt($ch, CURLOPT_HTTPHEADER, "x"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { new CurlHandle; } catch (Error $e) { echo $e->getMessage(), "\n"; }
var_dump(curl_strerror(1), curl_multi_strerror(1), curl_share_strerror(1));
curl_setopt($ch, CURLOPT_URL, "nope://x");
var_dump(curl_exec($ch), curl_errno($ch));
"#);
    let share_err = php_curl::share::strerror(1);
    let expected = format!(
        r#"curl_setopt(): Argument #2 ($option) is not a valid cURL option
curl_setopt_array(): Argument #2 ($options) contains an invalid cURL option
curl_setopt(): Argument #3 ($value) must be a valid callback for option CURLOPT_WRITEFUNCTION, function "nope" not found or invalid function name
curl_setopt(): cURL option CURLOPT_URL must not contain any null bytes
curl_setopt(): The CURLOPT_HTTPHEADER option must have an array value
Cannot directly construct CurlHandle, use curl_init() instead
string(20) "Unsupported protocol"
string(20) "Invalid multi handle"
string({}) "{share_err}"
bool(false)
int(1)
"#,
        share_err.len()
    );
    assert_eq!(out, expected);
}

#[test]
fn multi_and_escape() {
    let f = temp_file("b", "multi");
    let out = run(&format!(
        r#"$mh = curl_multi_init();
$ch = curl_init("file://{f}");
curl_setopt($ch, CURLOPT_RETURNTRANSFER, 1);
var_dump(curl_multi_add_handle($mh, $ch));
do {{ $s = curl_multi_exec($mh, $running); }} while ($running > 0);
var_dump(curl_multi_getcontent($ch));
$info = curl_multi_info_read($mh);
var_dump($info["msg"], $info["result"], $info["handle"] === $ch);
var_dump(curl_multi_remove_handle($mh, $ch), curl_multi_get_handles($mh) === []);
var_dump(curl_escape($ch, "a b"), curl_unescape($ch, "a%20b"));
"#
    ));
    assert_eq!(
        out,
        "int(0)\nstring(5) \"multi\"\nint(1)\nint(0)\nbool(true)\nint(0)\nbool(true)\nstring(5) \"a%20b\"\nstring(3) \"a b\"\n"
    );
    let _ = std::fs::remove_file(f);
}
