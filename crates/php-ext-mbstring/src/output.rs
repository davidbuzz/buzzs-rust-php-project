//! `mb_output_handler()` and the bits of SAPI header state it needs.
//!
//! The active SAPI's header binding supplies header additions. MIME state
//! and the running-handler state are shared with the VM output layer.

use php_mbstring::info::OutputSapi;
use php_runtime::Value;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::state::{self, bytes};

/// Request startup: `output_handler=mb_output_handler` in php.ini starts
/// the user handler before the script runs (`php_output_start_user()`).
/// The VM does not interpret `output_handler` itself.
pub(crate) fn request_startup(vm: &mut Vm) {
    if vm.ini.raw("output_handler") == Some(b"mb_output_handler".as_slice()) {
        php_support::output::start(
            vm,
            php_support::output::StartHandler::User {
                name: b"mb_output_handler".to_vec(),
                callback: Value::from("mb_output_handler"),
            },
            0,
            php_support::output::flags::STDFLAGS,
        );
    }
}

/// Snapshot SAPI state, queuing headers until the mbstring borrow is released.
struct VmSapi {
    default_mimetype: Vec<u8>,
    mimetype: Option<Vec<u8>>,
    send_default: bool,
    in_handler: bool,
    sent: bool,
    headers: Vec<Vec<u8>>,
}

impl OutputSapi for VmSapi {
    fn mimetype(&self) -> Option<Vec<u8>> {
        self.mimetype.clone()
    }

    fn mimetype_matches(&self, pattern: &[u8], mimetype: &[u8]) -> bool {
        crate::regex::simple_match(pattern, mimetype, true)
    }

    fn send_default_content_type(&self) -> bool {
        self.send_default
    }

    fn clear_send_default_content_type(&mut self) {
        self.send_default = false;
    }

    fn default_mimetype(&self) -> Vec<u8> {
        self.default_mimetype.clone()
    }

    fn in_output_handler(&self) -> bool {
        self.in_handler
    }

    fn add_header(&mut self, header: &[u8]) -> bool {
        self.headers.push(header.to_vec());
        !self.sent && !header.iter().any(|c| matches!(c, 0 | b'\r' | b'\n'))
    }
}

/// Runs the handler over one buffer.
fn handle(vm: &mut Vm, s: &[u8], status: i64) -> VmResult<Vec<u8>> {
    let sent = vm.response_headers_sent()?;
    let mut sapi = VmSapi {
        default_mimetype: vm
            .ini_string("default_mimetype")
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| b"text/html".to_vec()),
        mimetype: vm.response_mimetype(),
        send_default: vm.sends_default_content_type(),
        in_handler: vm.output.running_flags().is_some(),
        sent,
        headers: Vec::new(),
    };
    let out = state::with(|st| st.mb_output_handler(s, status, &mut sapi));
    for header in sapi.headers {
        if vm.add_response_header(&header, false)? {
            vm.clear_default_content_type();
        }
    }
    state::flush_diagnostics(vm)?;
    Ok(out)
}

fn mb_output_handler(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, status) = (a.str(0), a.int(1));
    handle(vm, s.as_bytes(), status).map(bytes)
}

/// Registers `mb_output_handler`.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function(
        "mb_output_handler(string $string, int $status): string",
        mb_output_handler,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use php_support::output::flags;

    #[test]
    fn mb_output_handler_is_a_user_handler() {
        let ext = crate::Mbstring;
        let mut vm = Vm::with_extensions(
            &php_vm::VmOptions::default(),
            Box::new(Vec::<u8>::new()),
            &[&ext],
        );
        let r = vm
            .call_value(
                &Value::from("ob_start"),
                vec![Value::from("mb_output_handler")],
            )
            .expect("ob_start");
        assert!(matches!(r, Value::Bool(true)));
        let st = vm.output.active_status().expect("handler");
        assert_eq!(st.name, b"mb_output_handler");
        assert_eq!(st.kind, flags::USER);
        assert_eq!(st.flags, flags::STDFLAGS | flags::USER);
    }

    #[test]
    fn invalid_content_type_does_not_change_shared_mime_state() {
        let ext = crate::Mbstring;
        let mut vm = Vm::with_extensions(
            &php_vm::VmOptions::default(),
            Box::new(Vec::<u8>::new()),
            &[&ext],
        );
        php_support::output::start(
            &mut vm,
            php_support::output::StartHandler::Default,
            0,
            flags::STDFLAGS,
        );
        assert!(!vm
            .add_response_header(b"Content-Type: application/json\r\nInjected: yes", true)
            .expect("header"));
        assert_eq!(vm.response_mimetype(), None);
        assert!(vm.sends_default_content_type());
        assert!(!vm.response_headers_sent().expect("headers_sent"));
        assert!(vm
            .add_response_header(b"Content-Type: text/plain", true)
            .expect("header"));
        assert_eq!(
            vm.response_mimetype(),
            Some(b"text/plain;charset=UTF-8".to_vec())
        );
        assert!(!vm.sends_default_content_type());
    }

    #[test]
    fn direct_output_handler_uses_real_header_services() {
        let ext = crate::Mbstring;
        let mut vm = Vm::with_extensions(
            &php_vm::VmOptions::default(),
            Box::new(Vec::<u8>::new()),
            &[&ext],
        );
        vm.call_value(&Value::from("mb_http_output"), vec![Value::from("UTF-8")])
            .expect("encoding");
        vm.call_value(
            &Value::from("mb_output_handler"),
            vec![
                Value::from("x"),
                Value::Int(i64::from(flags::START | flags::FINAL)),
            ],
        )
        .expect("handler");
        assert_eq!(
            vm.response_mimetype(),
            Some(b"text/html; charset=UTF-8".to_vec())
        );
        assert!(!vm.sends_default_content_type());
    }
}
