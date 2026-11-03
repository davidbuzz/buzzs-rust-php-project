//! `Phar::webPhar()` over the CLI SAPI.
//!
//! The CLI has no request (`SG(request_info).request_method` is `NULL`), so
//! after opening the executing archive `webPhar()` returns without routing.
//! The host still implements every [`WebHost`] service for completeness.

use php_phar::web::{MimeOverride, Rewrite, ServerValue, WebHost, WebPharArgs, WebResult};
use php_runtime::Value;
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

use crate::util::{bytes, executed, halt_defined, opt_str, run};

/// The CLI web host: no request, output through the VM.
struct CliHost<'a> {
    vm: &'a mut Vm,
    rewrite: Option<Value>,
}

impl WebHost for CliHost<'_> {
    fn sapi_name(&self) -> String {
        "cli".into()
    }
    fn request_method(&self) -> Option<Vec<u8>> {
        None
    }
    fn request_uri(&self) -> Option<Vec<u8>> {
        None
    }
    fn has_server(&self) -> bool {
        false
    }
    fn server_get(&self, _key: &str) -> Option<ServerValue> {
        None
    }
    fn server_set(&mut self, _key: &str, _value: Vec<u8>) {}
    fn getenv(&self, key: &str) -> Option<Vec<u8>> {
        use std::os::unix::ffi::OsStringExt;
        std::env::var_os(key).map(OsStringExt::into_vec)
    }
    fn header(&mut self, _line: &[u8], _response_code: Option<u16>) {}
    fn send_headers(&mut self) -> bool {
        true
    }
    fn write(&mut self, data: &[u8]) {
        self.vm.output(data);
    }
    fn highlight_file(&mut self, path: &[u8]) {
        let _ = self.vm.call_function("highlight_file", vec![bytes(path)]);
    }
    fn include(&mut self, _path: &[u8]) -> bool {
        false
    }
    fn rewrite(&mut self, entry: &[u8]) -> Rewrite {
        let Some(cb) = self.rewrite.clone() else {
            return Rewrite::Failed { exception: false };
        };
        match self.vm.call_value(&cb, vec![bytes(entry)]) {
            Ok(Value::Str(s)) => Rewrite::Str(s.as_bytes().to_vec()),
            Ok(Value::Bool(_)) => Rewrite::Bool,
            Ok(_) => Rewrite::Other,
            Err(t) => {
                self.vm.pending_throw = Some(t);
                Rewrite::Failed { exception: true }
            }
        }
    }
}

/// `Phar::webPhar()`.
pub fn web_phar(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let alias = opt_str(a, 0);
    let index = opt_str(a, 1);
    let f404 = opt_str(a, 2);
    let mimes = a.opt(3).and_then(|v| match v {
        Value::Array(arr) => Some(
            arr.iter()
                .map(|(k, v)| {
                    let key = match k {
                        php_runtime::ArrayKey::Str(s) => s.as_bytes().to_vec(),
                        php_runtime::ArrayKey::Int(i) => i.to_string().into_bytes(),
                    };
                    let m = match v.deref() {
                        Value::Int(i) => MimeOverride::Int(i),
                        Value::Str(s) => MimeOverride::Str(s.as_bytes().to_vec()),
                        _ => MimeOverride::Other,
                    };
                    (key, m)
                })
                .collect(),
        ),
        _ => None,
    });
    let rewrite = a.opt(4);
    let ex = executed(vm);
    let halt = halt_defined(vm);
    let args = WebPharArgs {
        alias: alias.as_deref(),
        index: index.as_deref(),
        f404: f404.as_deref(),
        mimes,
        has_rewrite: rewrite.is_some(),
    };
    let res = {
        let mut host = CliHost { vm, rewrite };
        php_phar::web::web_phar(&mut host, ex.as_deref(), halt, &args)
    };
    if let Some(t) = vm.pending_throw.take() {
        return Err(t);
    }
    let res = run(vm, || res)?;
    if res == WebResult::Exit {
        return Err(php_vm::ext::Throw::Exit);
    }
    Ok(Value::Null)
}
