//! `mb_output_handler()` and the bits of SAPI header state it needs.
//!
//! The VM's `header()` keeps no header state, so this module wraps it to
//! track `SG(sapi_headers).mimetype` and `send_default_content_type`
//! (`sapi_header_op` for `Content-Type:` lines), then calls the original.

use std::cell::RefCell;

use php_mbstring::info::OutputSapi;
use php_runtime::Value;
use php_vm::ext::{Args, ModuleBuilder, NativeFn, VmResult};
use php_vm::Vm;

use crate::state::{self, bytes};

#[derive(Default)]
struct Headers {
    /// `SG(sapi_headers).mimetype`.
    mimetype: Option<Vec<u8>>,
    /// `SG(sapi_headers).send_default_content_type` is *cleared*.
    default_sent: bool,
}

thread_local! {
    static HEADERS: RefCell<Headers> = RefCell::new(Headers::default());
    static ORIG_HEADER: std::cell::Cell<Option<NativeFn>> = const { std::cell::Cell::new(None) };
}

/// Installs the `header()` wrapper.
pub(crate) fn install_hooks(m: &mut ModuleBuilder<'_>) {
    let vm = m.vm();
    if let Some(b) = vm.native_function("header") {
        ORIG_HEADER.with(|c| c.set(Some(b.handler)));
        vm.register_function(
            "header(string $header, bool $replace = true, int $response_code = 0): void",
            header_hook,
        );
    }
}

/// Request startup: `output_handler=mb_output_handler` in php.ini starts
/// the handler before the script runs (`php_output_start_user()` with
/// mbstring's handler alias). The VM does not interpret `output_handler`
/// itself.
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

/// `RSHUTDOWN`.
pub(crate) fn reset() {
    HEADERS.with(|h| *h.borrow_mut() = Headers::default());
}

fn header_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let line = a.str(0);
    let line = line.as_bytes();
    if let Some(colon) = line.iter().position(|&b| b == b':') {
        if line[..colon].eq_ignore_ascii_case(b"Content-Type") {
            let mut mime: Vec<u8> = line[colon + 1..]
                .iter()
                .copied()
                .skip_while(|&b| b == b' ')
                .collect();
            // sapi_apply_default_charset()
            let charset = vm.ini_string("default_charset").unwrap_or_default();
            if !charset.is_empty()
                && mime.starts_with(b"text/")
                && !mime.windows(8).any(|w| w == b"charset=")
            {
                mime.extend_from_slice(b";charset=");
                mime.extend_from_slice(&charset);
            }
            HEADERS.with(|h| {
                let mut h = h.borrow_mut();
                if h.mimetype.is_none() {
                    h.mimetype = Some(mime);
                }
                h.default_sent = true;
            });
        }
    }
    match ORIG_HEADER.with(std::cell::Cell::get) {
        Some(f) => f(vm, a),
        None => Ok(Value::Null),
    }
}

/// The CLI SAPI as seen by `mb_output_handler`.
struct CliSapi {
    default_mimetype: Vec<u8>,
}

impl OutputSapi for CliSapi {
    fn mimetype(&self) -> Option<Vec<u8>> {
        HEADERS.with(|h| h.borrow().mimetype.clone())
    }

    fn mimetype_matches(&self, pattern: &[u8], mimetype: &[u8]) -> bool {
        crate::regex::simple_match(pattern, mimetype, true)
    }

    fn send_default_content_type(&self) -> bool {
        HEADERS.with(|h| !h.borrow().default_sent)
    }

    fn clear_send_default_content_type(&mut self) {
        HEADERS.with(|h| h.borrow_mut().default_sent = true);
    }

    fn default_mimetype(&self) -> Vec<u8> {
        self.default_mimetype.clone()
    }

    fn in_output_handler(&self) -> bool {
        false
    }

    fn add_header(&mut self, _header: &[u8]) -> bool {
        true
    }
}

/// Runs the handler over one buffer.
fn handle(vm: &mut Vm, s: &[u8], status: i64) -> VmResult<Vec<u8>> {
    let mut sapi = CliSapi {
        default_mimetype: vm
            .ini_string("default_mimetype")
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| b"text/html".to_vec()),
    };
    let out = state::with(|st| st.mb_output_handler(s, status, &mut sapi));
    state::flush_diagnostics(vm)?;
    Ok(out)
}

fn mb_output_handler(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, status) = (a.str(0), a.int(1));
    handle(vm, s.as_bytes(), status).map(bytes)
}

/// The named native handler (`php_mb_output_handler_init` alias), used by
/// `ob_start('mb_output_handler')` and `output_handler=mb_output_handler`.
fn native_handler(vm: &mut Vm, buf: &[u8], mode: i32) -> VmResult<Option<Vec<u8>>> {
    handle(vm, buf, i64::from(mode)).map(Some)
}

/// Registers `mb_output_handler`.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function(
        "mb_output_handler(string $string, int $status): string",
        mb_output_handler,
    );
    m.output_handler("mb_output_handler", native_handler);
}
