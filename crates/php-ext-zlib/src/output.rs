//! Output compression: `ob_gzhandler()`, `zlib_get_coding_type()` and the
//! `zlib.output_compression*` / `zlib.output_handler` INI settings.
//!
//! The VM has no INI modify callbacks and no native output handlers, so:
//!
//! * INI changes are picked up lazily ([`sync_ini`]) at request startup,
//!   when `zlib_get_coding_type()`/`ob_gzhandler()` run and when the
//!   compression handler is invoked;
//! * the internal `"zlib output compression"` handler is a hidden native
//!   function of that name, started with `ob_start()` (so
//!   `ob_list_handlers()` reports PHP's name);
//! * `ob_start('ob_gzhandler')` calls the `ob_gzhandler()` function as a
//!   user handler (PHP's "legacy" path, with a per-request context).

use std::cell::RefCell;

use php_runtime::{Value, ZStr};
use php_support::ini::{parse_quantity, Access, IniDirective, ModifyHandler};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;
use php_zlib::output::{
    flags, output_compression_keyword, CompressionStart, IniUpdate, OutputContext, OutputEnv,
    ZlibOutput, OUTPUT_HANDLER_NAME,
};

use crate::bytes;

/// Per-request output state.
#[derive(Default)]
struct State {
    zlib: ZlibOutput,
    /// The context of the running internal compression handler.
    handler_ctx: Option<OutputContext>,
    /// The last `zlib.output_compression` INI string seen.
    seen_compression: Option<Vec<u8>>,
    /// The last `zlib.output_handler` INI string seen.
    seen_handler: Option<Vec<u8>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// The SAPI side of output compression for the CLI.
struct CliEnv {
    /// Whether the running handler has already processed output
    /// (`PHP_OUTPUT_HANDLER_STARTED`); `None` outside a handler.
    started: Option<bool>,
}

impl OutputEnv for CliEnv {
    fn accept_encoding(&mut self) -> Option<Vec<u8>> {
        use std::os::unix::ffi::OsStringExt;
        std::env::var_os("HTTP_ACCEPT_ENCODING").map(OsStringExt::into_vec)
    }

    fn add_header(&mut self, _header: &str, _replace: bool) {}

    fn headers_sent(&self) -> bool {
        false
    }

    fn running_handler_flags(&mut self) -> Option<i32> {
        self.started
            .map(|s| flags::STDFLAGS | if s { flags::STARTED } else { 0 })
    }

    fn set_running_handler_immutable(&mut self) {}
}

/// Parses a `zlib.output_compression` value (`On`/`Off`/a quantity).
fn compression_value(v: &[u8]) -> i64 {
    output_compression_keyword(v).unwrap_or_else(|| parse_quantity(v).0)
}

/// Starts the internal compression handler (and `zlib.output_handler`).
fn start(vm: &mut Vm, s: CompressionStart) -> VmResult<()> {
    STATE.with(|st| st.borrow_mut().handler_ctx = Some(s.context));
    let chunk = i64::try_from(s.chunk_size).unwrap_or(0);
    let ok = vm.call_value(
        &Value::Str(ZStr::from("ob_start")),
        vec![
            Value::Str(ZStr::from(OUTPUT_HANDLER_NAME)),
            Value::Int(chunk),
            Value::Int(i64::from(flags::STDFLAGS)),
        ],
    )?;
    if matches!(ok, Value::Bool(true)) {
        if let Some(h) = s.user_handler {
            vm.call_value(
                &Value::Str(ZStr::from("ob_start")),
                vec![Value::Str(ZStr::from(h.as_slice())), Value::Int(chunk)],
            )?;
        }
    }
    Ok(())
}

/// Applies INI changes made since the last check (the modify handlers).
fn sync_ini(vm: &mut Vm, startup: bool) -> VmResult<()> {
    let compression = vm.ini_string("zlib.output_compression").unwrap_or_default();
    let level = vm.ini_long("zlib.output_compression_level");
    let handler = vm.ini_string("zlib.output_handler").unwrap_or_default();
    let output_handler_set = vm
        .ini_string("output_handler")
        .is_some_and(|h| !h.is_empty());
    let mut env = CliEnv { started: None };
    let mut warning = None;
    let to_start = STATE.with(|st| {
        let mut st = st.borrow_mut();
        st.zlib.output_compression_level = level;
        if st.seen_handler.as_deref() != Some(handler.as_slice()) {
            st.seen_handler = Some(handler.clone());
            let _ = st.zlib.on_update_output_handler(&handler, !startup, false);
        }
        if st.seen_compression.as_deref() == Some(compression.as_slice()) {
            return None;
        }
        st.seen_compression = Some(compression.clone());
        let value = compression_value(&compression);
        let update = IniUpdate {
            runtime: !startup,
            output_handler_ini_set: output_handler_set,
            output_sent: false,
            handler_started: st.handler_ctx.is_some(),
        };
        match st
            .zlib
            .on_update_output_compression(&mut env, value, &update)
        {
            Ok(s) => s,
            Err((_, msg)) => {
                warning = Some(msg);
                None
            }
        }
    });
    if let Some(msg) = warning {
        vm.raise(2, msg)?;
    }
    if startup {
        let s = STATE.with(|st| st.borrow_mut().zlib.request_startup(&mut env));
        if let Some(s) = s {
            start(vm, s)?;
        }
    } else if let Some(s) = to_start {
        start(vm, s)?;
    }
    Ok(())
}

fn request_startup(vm: &mut Vm) {
    STATE.with(|st| *st.borrow_mut() = State::default());
    let _ = sync_ini(vm, true);
}

/// The internal `"zlib output compression"` output handler.
fn compression_handler(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    sync_ini(vm, false)?;
    let data = a.str(0);
    let op = i32::try_from(a.int(1)).unwrap_or(0);
    let mut env = CliEnv {
        started: Some(op & flags::START == 0),
    };
    let out = STATE.with(|st| {
        let mut st = st.borrow_mut();
        let st = &mut *st;
        let ctx = st.handler_ctx.get_or_insert_with(OutputContext::new);
        st.zlib.output_handler(ctx, &mut env, op, data.as_bytes())
    });
    Ok(out.map_or(Value::Bool(false), bytes))
}

fn ob_gzhandler(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    sync_ini(vm, false)?;
    let data = a.str(0);
    let fl = a.int(1);
    let mut env = CliEnv { started: None };
    let out = STATE.with(|st| {
        st.borrow_mut()
            .zlib
            .ob_gzhandler(&mut env, data.as_bytes(), fl)
    });
    Ok(out.map_or(Value::Bool(false), bytes))
}

fn zlib_get_coding_type(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    sync_ini(vm, false)?;
    let t = STATE.with(|st| st.borrow().zlib.coding_type());
    Ok(t.map_or(Value::Bool(false), bytes))
}

/// Registers the INI entries, functions and the startup hook.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.ini(IniDirective::new(
        "zlib.output_compression",
        Some("0"),
        Access::ALL,
        ModifyHandler::None,
        "zlib",
    ))
    .ini(IniDirective::new(
        "zlib.output_compression_level",
        Some("-1"),
        Access::ALL,
        ModifyHandler::Long,
        "zlib",
    ))
    .ini(IniDirective::new(
        "zlib.output_handler",
        Some(""),
        Access::ALL,
        ModifyHandler::String,
        "zlib",
    ));
    m.function(
        "ob_gzhandler(string $data, int $flags): string|false",
        ob_gzhandler,
    )
    .function("zlib_get_coding_type(): string|false", zlib_get_coding_type)
    .function(
        &format!("{OUTPUT_HANDLER_NAME}(string $data, int $flags): string|false"),
        compression_handler,
    );
    m.on_request_startup(request_startup);
}
