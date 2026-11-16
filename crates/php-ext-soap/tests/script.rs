// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end tests: PHP scripts run on a VM with the `soap` extension
//! (plus the stream/string functions it relies on). Expected outputs follow
//! php-src master's ext/soap tests.

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

/// Runs `code` (without `<?php`) and returns its output, on a thread with
/// a large stack (the debug-build interpreter recurses deeply, like the CLI
/// which raises its own stack limit).
fn run(code: &str) -> String {
    let code = code.to_owned();
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || run_on_this_thread(&code))
        .expect("spawn")
        .join()
        .expect("script thread")
}

fn run_on_this_thread(code: &str) -> String {
    let sink = Sink::default();
    let stdstr = php_ext_stdstr::StdStr;
    let streams = php_ext_streams::Streams;
    let soap = php_ext_soap::Soap;
    let exts: [&dyn Extension; 3] = [&stdstr, &streams, &soap];
    let opts = VmOptions {
        ini: vec![("soap.wsdl_cache_enabled".into(), "0".into())],
    };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    let src = format!("<?php {code}");
    vm.run_script(src.as_bytes(), "test.php");
    // Dropping the VM releases the STDIN/STDOUT/STDERR stream resources,
    // which closes the test harness's own descriptors.
    std::mem::forget(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

const REQUEST: &str = r#"<?xml version="1.0"?><SOAP-ENV:Envelope xmlns:SOAP-ENV="http://schemas.xmlsoap.org/soap/envelope/"><SOAP-ENV:Body><ns1:test xmlns:ns1="http://testuri.org"><x>3</x></ns1:test></SOAP-ENV:Body></SOAP-ENV:Envelope>"#;

#[test]
fn server_handles_function_call() {
    let out = run(&format!(
        r#"function test($x) {{ return "got $x"; }}
        $s = new SoapServer(null, ['uri' => 'http://testuri.org']);
        $s->addFunction('test');
        $s->handle('{REQUEST}');
        echo "\nok\n";"#
    ));
    assert!(
        out.contains(
            r#"<ns1:testResponse><return xsi:type="xsd:string">got 3</return></ns1:testResponse>"#
        ),
        "{out}"
    );
    assert!(out.ends_with("\nok\n"), "{out}");
}

#[test]
fn server_fault_bails_out() {
    let out = run(&format!(
        r#"function test($x) {{ global $s; $s->fault("MyFault", "My fault string"); }}
        $s = new SoapServer(null, ['uri' => 'http://testuri.org']);
        $s->addFunction('test');
        $s->handle('{REQUEST}');
        echo "not reached\n";"#
    ));
    assert!(
        out.contains("<faultcode>MyFault</faultcode><faultstring>My fault string</faultstring>"),
        "{out}"
    );
    assert!(!out.contains("not reached"), "{out}");
}

#[test]
fn server_reports_errors_as_faults() {
    let out = run(&format!(
        r#"function test($x) {{ undefined_function_x(); }}
        $s = new SoapServer(null, ['uri' => 'http://testuri.org']);
        $s->addFunction('test');
        $s->handle('{REQUEST}');
        echo "ok\n";"#
    ));
    assert!(
        out.contains(
            "<faultstring>Call to undefined function undefined_function_x()</faultstring>"
        ),
        "{out}"
    );
}

#[test]
fn client_magic_call_and_do_request_override() {
    let out = run(r#"class C extends SoapClient {
            public $req;
            function __doRequest($req, $loc, $act, $ver, $one = false, $p = null): ?string {
                $this->req = $req;
                return '<?xml version="1.0"?><SOAP-ENV:Envelope xmlns:SOAP-ENV="http://schemas.xmlsoap.org/soap/envelope/"><SOAP-ENV:Body><ns1:addResponse xmlns:ns1="http://x"><r>42</r></ns1:addResponse></SOAP-ENV:Body></SOAP-ENV:Envelope>';
            }
        }
        $c = new C(null, ['uri' => 'http://x', 'location' => 'http://y']);
        var_dump($c->add(1, "a"));
        var_dump(str_contains($c->req, '<ns1:add><param0 xsi:type="xsd:int">1</param0><param1 xsi:type="xsd:string">a</param1></ns1:add>'));
        var_dump($c->__call('add', [2]));"#);
    assert_eq!(
        out, "string(2) \"42\"\nbool(true)\nstring(2) \"42\"\n",
        "{out}"
    );
}

#[test]
fn client_fault_and_exceptions_option() {
    let out = run(r#"class C extends SoapClient {
            function __doRequest($req, $loc, $act, $ver, $one = false, $p = null): ?string { return null; }
        }
        $c = new C(null, ['uri' => 'http://x', 'location' => 'http://y']);
        try { $c->f(); } catch (SoapFault $e) { echo get_class($e), ": ", $e->faultcode, " ", $e->getMessage(), "\n"; }
        $c = new C(null, ['uri' => 'http://x', 'location' => 'http://y', 'exceptions' => false]);
        $r = $c->f();
        var_dump(is_soap_fault($r));"#);
    assert_eq!(
        out, "SoapFault: Client SoapClient::__doRequest() returned non string value\nbool(true)\n",
        "{out}"
    );
}

#[test]
fn value_classes_validate_arguments() {
    let out = run(
        r#"try { new SoapParam(1, ""); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
        try { new SoapHeader("", "n"); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
        try { new SoapVar(1, 12345); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
        $f = new SoapFault("Server", "boom");
        echo $f->faultcode, "|", $f->faultstring, "|", $f->getMessage(), "\n";
        $v = new SoapVar("x", XSD_STRING, "string", XSD_NAMESPACE);
        echo $v->enc_type, " ", $v->enc_stype, "\n";
        try { new Soap\Url; } catch (Error $e) { echo $e->getMessage(), "\n"; }"#,
    );
    assert_eq!(
        out,
        "SoapParam::__construct(): Argument #2 ($name) must not be empty\n\
         SoapHeader::__construct(): Argument #1 ($namespace) must not be empty\n\
         SoapVar::__construct(): Argument #2 ($encoding) is not a valid encoding\n\
         Server|boom|boom\n\
         101 string\n\
         Cannot directly construct Soap\\Url\n",
        "{out}"
    );
}

#[test]
fn constants_and_ini() {
    let out = run(
        r"var_dump(SOAP_1_2, XSD_ANYXML, WSDL_CACHE_BOTH, ini_get('soap.wsdl_cache_dir'));
        var_dump(use_soap_error_handler(true), use_soap_error_handler(false));",
    );
    assert_eq!(
        out, "int(2)\nint(147)\nint(3)\nstring(4) \"/tmp\"\nbool(false)\nbool(true)\n",
        "{out}"
    );
}
