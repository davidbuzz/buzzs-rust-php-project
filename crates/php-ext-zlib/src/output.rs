//! Output compression: `ob_gzhandler()`, `zlib_get_coding_type()` and the
//! `zlib.output_compression*` / `zlib.output_handler` INI settings.
//!
//! INI changes use the registry's on-modify hooks. The native
//! `ob_gzhandler` alias and automatic compression use the internal output
//! handler; direct function calls retain PHP's separate legacy context.

use std::cell::RefCell;

use php_runtime::{ArrayKey, Value, ZStr};
use php_support::ini::{parse_quantity, Access, IniDirective, ModifyHandler};
use php_support::output::{self, StartHandler};
use php_vm::ext::{Args, IniStage, ModuleBuilder, VmResult};
use php_vm::Vm;
use php_zlib::output::{
    flags, output_compression_keyword, CompressionStart, IniUpdate, OutputContext, OutputEnv,
    ZlibOutput, CONFLICTING_HANDLERS, OUTPUT_HANDLER_NAME,
};

use crate::bytes;

/// Per-request output state.
#[derive(Default)]
struct State {
    zlib: ZlibOutput,
    /// The context of the running internal compression handler.
    handler_ctx: Option<OutputContext>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// `php_zlib_output_encoding()` and the SAPI/output hooks in `zlib.c`.
/// Apply side effects after releasing STATE so diagnostics can re-enter.
#[derive(Default)]
struct VmEnv {
    encoding: Option<Vec<u8>>,
    sent: bool,
    running_flags: Option<i32>,
    headers: Vec<(String, bool)>,
    immutable: bool,
}

impl VmEnv {
    fn new(vm: &mut Vm, negotiate: bool) -> VmResult<Self> {
        let value = if negotiate && STATE.with(|st| st.borrow().zlib.compression_coding == 0) {
            let mut g = vm.globals.borrow_mut();
            g.find(b"_SERVER").and_then(|i| {
                match g
                    .slots
                    .get(i as usize)
                    .and_then(Option::as_ref)
                    .map(Value::deref)
                {
                    Some(Value::Array(a)) => {
                        a.get(&ArrayKey::from("HTTP_ACCEPT_ENCODING")).cloned()
                    }
                    _ => None,
                }
            })
        } else {
            None
        };
        let encoding = value.map(|v| vm.value_to_string(&v)).transpose()?;
        if let Some(encoding) = &encoding {
            let mut g = vm.globals.borrow_mut();
            if let Some(i) = g.find(b"_SERVER") {
                if let Some(Some(Value::Array(a))) = g.slots.get_mut(i as usize) {
                    a.make_mut().insert(
                        ArrayKey::from("HTTP_ACCEPT_ENCODING"),
                        Value::Str(encoding.clone()),
                    );
                }
            }
        }
        Ok(Self {
            encoding: encoding.map(|s| s.as_bytes().to_vec()),
            sent: vm.response_headers_sent()?,
            running_flags: vm.output.running_flags(),
            ..Self::default()
        })
    }

    fn apply(self, vm: &mut Vm) -> VmResult<()> {
        for (line, replace) in self.headers {
            vm.add_response_header(line.as_bytes(), replace)?;
        }
        if self.immutable {
            vm.output.set_running_immutable();
        }
        Ok(())
    }
}

impl OutputEnv for VmEnv {
    fn accept_encoding(&mut self) -> Option<Vec<u8>> {
        self.encoding.clone()
    }

    fn add_header(&mut self, header: &str, replace: bool) {
        self.headers.push((header.to_string(), replace));
    }

    fn headers_sent(&self) -> bool {
        self.sent
    }

    fn running_handler_flags(&mut self) -> Option<i32> {
        self.running_flags
    }

    fn set_running_handler_immutable(&mut self) {
        self.immutable = true;
    }
}

/// Parses a `zlib.output_compression` value (`On`/`Off`/a quantity).
#[cfg(test)]
fn compression_value(v: &[u8]) -> i64 {
    output_compression_keyword(v).unwrap_or_else(|| parse_quantity(v).0)
}

fn parse_compression(vm: &mut Vm, value: &[u8]) -> VmResult<i64> {
    if let Some(value) = output_compression_keyword(value) {
        return Ok(value);
    }
    let (value, error) = parse_quantity(value);
    if let Some(error) = error {
        vm.php_warning(&format!(
            "Invalid \"zlib.output_compression\" setting. {error}"
        ))?;
    }
    Ok(value)
}

/// Starts the internal compression handler (and `zlib.output_handler`).
fn start(vm: &mut Vm, s: CompressionStart) -> VmResult<()> {
    let chunk = i64::try_from(s.chunk_size).unwrap_or(0);
    let ok = output::start(
        vm,
        StartHandler::Alias {
            name: OUTPUT_HANDLER_NAME.as_bytes().to_vec(),
            callback: Value::from("ob_gzhandler"),
        },
        s.chunk_size,
        flags::STDFLAGS,
    );
    if let Some(t) = vm.pending_throw.take() {
        return Err(t);
    }
    if ok {
        STATE.with(|st| st.borrow_mut().handler_ctx = Some(s.context));
        if let Some(h) = s.user_handler {
            vm.call_value(
                &Value::Str(ZStr::from("ob_start")),
                vec![Value::Str(ZStr::from(h.as_slice())), Value::Int(chunk)],
            )?;
        }
    }
    Ok(())
}

/// `OnUpdate_zlib_output_compression` (`ext/zlib/zlib.c`).
fn update_compression(vm: &mut Vm, value: &[u8]) -> VmResult<bool> {
    let value = parse_compression(vm, value)?;
    let update = IniUpdate {
        runtime: vm.ini_modify_stage() == IniStage::Runtime,
        output_handler_ini_set: vm
            .ini_string("output_handler")
            .is_some_and(|h| !h.is_empty()),
        output_sent: vm.output.status() & output::status::SENT != 0,
        handler_started: vm.output.handler_started(OUTPUT_HANDLER_NAME.as_bytes()),
    };
    let negotiate = update.runtime
        && value != 0
        && !update.handler_started
        && !update.output_sent
        && !update.output_handler_ini_set;
    let mut env = VmEnv::new(vm, negotiate)?;
    let result = STATE.with(|st| {
        st.borrow_mut()
            .zlib
            .on_update_output_compression(&mut env, value, &update)
    });
    let to_start = match result {
        Ok(s) => s,
        Err((core, msg)) => {
            if core {
                vm.raise(16, msg)?;
            } else {
                vm.php_warning(msg)?;
            }
            return Ok(false);
        }
    };
    env.apply(vm)?;
    if let Some(s) = to_start {
        start(vm, s)?;
    }
    Ok(true)
}

fn update_handler(vm: &mut Vm, value: &[u8]) -> VmResult<bool> {
    let runtime = vm.ini_modify_stage() == IniStage::Runtime;
    let sent = vm.output.status() & output::status::SENT != 0;
    let result = STATE.with(|st| {
        st.borrow_mut()
            .zlib
            .on_update_output_handler(value, runtime, sent)
    });
    if let Err(msg) = result {
        vm.php_warning(msg)?;
        return Ok(false);
    }
    Ok(true)
}

fn request_startup(vm: &mut Vm) {
    STATE.with(|st| *st.borrow_mut() = State::default());
    let result = (|| {
        // php_request_startup(): `output_handler=ob_gzhandler` starts the
        // alias (php_output_start_user() -> php_zlib_output_handler_init())
        // before RINIT, which then sees `handler_registered` (bug55544.phpt).
        // The VM does not interpret `output_handler` itself.
        if vm.ini.raw("output_handler") == Some(b"ob_gzhandler".as_slice()) {
            init_handler(vm, 0, flags::STDFLAGS)?;
            output::start(
                vm,
                StartHandler::Alias {
                    name: b"ob_gzhandler".to_vec(),
                    callback: Value::from("ob_gzhandler"),
                },
                0,
                flags::STDFLAGS,
            );
            if let Some(t) = vm.pending_throw.take() {
                return Err(t);
            }
        }
        let compression = vm.ini_string("zlib.output_compression").unwrap_or_default();
        let handler = vm.ini_string("zlib.output_handler").unwrap_or_default();
        let value = parse_compression(vm, &compression)?;
        let mut env = VmEnv::new(vm, value != 0)?;
        let update = IniUpdate {
            output_handler_ini_set: vm
                .ini_string("output_handler")
                .is_some_and(|h| !h.is_empty()),
            ..IniUpdate::default()
        };
        let result = STATE.with(|st| {
            let mut st = st.borrow_mut();
            st.zlib.output_compression_level = vm.ini_long("zlib.output_compression_level");
            st.zlib.output_handler = handler;
            st.zlib
                .on_update_output_compression(&mut env, value, &update)
        });
        if let Err((_, msg)) = result {
            vm.raise(16, msg)?;
            return Ok(());
        }
        let s = STATE.with(|st| st.borrow_mut().zlib.request_startup(&mut env));
        env.apply(vm)?;
        if let Some(s) = s {
            start(vm, s)?;
        }
        Ok(())
    })();
    if let Err(t) = result {
        vm.pending_throw = Some(t);
    }
}

/// Initializes the alias without replacing an already active context.
#[allow(clippy::unnecessary_wraps)] // native constructor signature
fn init_handler(vm: &mut Vm, chunk: usize, _flags: i32) -> VmResult<()> {
    let active = vm.output.handler_started(b"ob_gzhandler")
        || vm.output.handler_started(OUTPUT_HANDLER_NAME.as_bytes());
    STATE.with(|st| {
        let mut st = st.borrow_mut();
        let ctx = st.zlib.handler_init(chunk);
        if !active {
            st.handler_ctx = Some(ctx);
        }
    });
    Ok(())
}

fn conflict_check(vm: &mut Vm, name: &[u8]) -> bool {
    !CONFLICTING_HANDLERS
        .iter()
        .any(|old| output::handler_conflict(vm, name, old.as_bytes()))
}

/// The internal compression handler (not a userland function).
fn native_handler(vm: &mut Vm, data: &[u8], op: i32) -> VmResult<Option<Vec<u8>>> {
    let mut env = VmEnv::new(vm, true)?;
    let level = vm.ini_long("zlib.output_compression_level");
    let out = STATE.with(|st| {
        let mut st = st.borrow_mut();
        let st = &mut *st;
        st.zlib.output_compression_level = level;
        let ctx = st.handler_ctx.get_or_insert_with(OutputContext::new);
        st.zlib.output_handler(ctx, &mut env, op, data)
    });
    env.apply(vm)?;
    Ok(out)
}

fn ob_gzhandler(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = a.str(0);
    let fl = a.int(1);
    let mut env = VmEnv::new(vm, true)?;
    let level = vm.ini_long("zlib.output_compression_level");
    let out = STATE.with(|st| {
        let mut st = st.borrow_mut();
        st.zlib.output_compression_level = level;
        st.zlib.ob_gzhandler(&mut env, data.as_bytes(), fl)
    });
    env.apply(vm)?;
    Ok(out.map_or(Value::Bool(false), bytes))
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn zlib_get_coding_type(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
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
    m.function_static(
        "ob_gzhandler(string $data, int $flags): string|false",
        ob_gzhandler,
    )
    .function_static("zlib_get_coding_type(): string|false", zlib_get_coding_type);
    m.output_handler_with_init("ob_gzhandler", native_handler, Some(init_handler))
        .output_handler_conflict("ob_gzhandler", conflict_check)
        .output_handler_conflict(OUTPUT_HANDLER_NAME, conflict_check);
    m.ini_on_modify("zlib.output_compression", update_compression)
        .ini_on_modify("zlib.output_handler", update_handler);
    m.on_request_startup(request_startup);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_values() {
        assert_eq!(compression_value(b"On"), 1);
        assert_eq!(compression_value(b"off"), 0);
        assert_eq!(compression_value(b"4096"), 4096);
        assert_eq!(compression_value(b"4K"), 4096);
    }

    #[test]
    fn env_flags() {
        let mut env = VmEnv {
            running_flags: Some(flags::STDFLAGS),
            ..VmEnv::default()
        };
        assert_eq!(env.running_handler_flags(), Some(flags::STDFLAGS));
        env.running_flags = Some(flags::STDFLAGS | flags::STARTED);
        assert_eq!(
            env.running_handler_flags(),
            Some(flags::STDFLAGS | flags::STARTED)
        );
        assert!(!env.headers_sent());
    }

    #[test]
    fn registered_alias_checks_each_upstream_conflict() {
        let ext = crate::Zlib;
        for old in CONFLICTING_HANDLERS {
            let mut vm = Vm::with_extensions(
                &php_vm::VmOptions::default(),
                Box::new(Vec::<u8>::new()),
                &[&ext],
            );
            request_startup(&mut vm);
            assert!(output::start(
                &mut vm,
                StartHandler::Internal {
                    name: old.as_bytes().to_vec(),
                    func: Box::new(|buf, _| Some(buf.to_vec())),
                },
                0,
                flags::STDFLAGS,
            ));
            let r = vm
                .call_value(&Value::from("ob_start"), vec![Value::from("ob_gzhandler")])
                .expect("ob_start");
            assert!(matches!(r, Value::Bool(false)), "{old}");
            assert_eq!(vm.output.list_handlers(), vec![old.as_bytes().to_vec()]);
        }
    }

    #[test]
    fn alias_constructor_initializes_compression_state_at_start() {
        let ext = crate::Zlib;
        let mut vm = Vm::with_extensions(
            &php_vm::VmOptions::default(),
            Box::new(Vec::<u8>::new()),
            &[&ext],
        );
        request_startup(&mut vm);
        let r = vm
            .call_value(
                &Value::from("ob_start"),
                vec![Value::from("ob_gzhandler"), Value::Int(1024)],
            )
            .expect("ob_start");
        assert!(matches!(r, Value::Bool(true)));
        STATE.with(|s| {
            let s = s.borrow();
            assert_eq!(s.zlib.output_compression, 1024);
            assert!(s.zlib.handler_registered);
            assert!(s.handler_ctx.is_some());
        });
        assert_eq!(vm.output.active_status().expect("handler").kind, 0);
    }
}
