//! observer.c: the observer API printer. Needs the VM observer hooks of
//! `proposals/observer.diff` (enabled with the `observer` feature).
//!
//! With `zend_test.observer.enabled=1` every function (user or native),
//! file and generator resumption is reported on its first call
//! (`<!-- init foo() -->`), and the observed ones are wrapped in
//! `<foo>` ... `</foo>` lines indented by the nesting depth, optionally
//! with their return value or type.

use php_runtime::{Value, ZStr};
use php_vm::ext::{ModuleBuilder, ObservedFrame, ObserverHooks, VmResult};
use php_vm::vm::FrameKind;
use php_vm::Vm;
use std::cell::{Cell, RefCell};
use std::fmt::Write as _;

thread_local! {
    /// `ZT_G(observer_nesting_depth)`.
    static DEPTH: Cell<usize> = const { Cell::new(0) };
    /// Whether each active `begin` printed (and so whether its `end`
    /// prints), innermost last.
    static PRINTED: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
    /// `ZT_G(observer_throw_on_vm_interrupt)`.
    static THROW_ON_INTERRUPT: Cell<bool> = const { Cell::new(false) };
}

fn ini(vm: &Vm, name: &str) -> bool {
    vm.ini_bool(&format!("zend_test.observer.{name}"))
}

fn indent(extra: usize) -> String {
    " ".repeat(2 * DEPTH.with(Cell::get) + extra)
}

fn out(vm: &mut Vm, line: &str) {
    vm.output(line.as_bytes());
}

/// `Class::name` / `name`.
fn display(f: &ObservedFrame) -> Option<String> {
    let name = f.function.as_ref()?.to_string_lossy();
    Some(match &f.scope {
        Some(s) => format!("{}::{name}", s.to_string_lossy()),
        None => name,
    })
}

fn file_name(f: &ObservedFrame) -> String {
    f.filename
        .as_ref()
        .map_or_else(|| "[no active file]".to_string(), ZStr::to_string_lossy)
}

/// Whether the observer reports `f` (`observer_fcall_init`'s choice of
/// handlers), evaluated with the current INI values so that
/// `ini_set('zend_test.observer.observe_function_names', ...)` applies to
/// functions already called.
fn wants(vm: &Vm, f: &ObservedFrame) -> bool {
    if ini(vm, "observe_all") {
        return true;
    }
    match &f.function {
        Some(name) => {
            if ini(vm, "observe_functions") {
                return true;
            }
            let list = vm
                .ini_string("zend_test.observer.observe_function_names")
                .unwrap_or_default();
            !list.is_empty()
                && list
                    .split(|&c| c == b',')
                    .any(|n| n.eq_ignore_ascii_case(name.as_bytes()))
        }
        None => ini(vm, "observe_includes"),
    }
}

/// `observer_show_init_backtrace`.
fn show_init_backtrace(vm: &mut Vm) {
    let mut s = format!("{}<!--\n", indent(0));
    let pad = indent(4);
    for fr in vm.frames.iter().rev() {
        match (fr.kind, &fr.func) {
            (FrameKind::Function | FrameKind::Builtin, Some(func)) => {
                let name = func.name().to_string_lossy();
                match &fr.scope {
                    Some(c) if !name.starts_with("{closure") => {
                        let _ = writeln!(s, "{pad}{}::{name}()", c.name.to_string_lossy());
                    }
                    _ => {
                        let _ = writeln!(s, "{pad}{name}()");
                    }
                }
            }
            (FrameKind::ConstExpr, _) => {}
            _ => {
                let _ = writeln!(s, "{pad}{{main}} {}", fr.code.filename.to_string_lossy());
            }
        }
    }
    let _ = writeln!(s, "{}-->", indent(0));
    out(vm, &s);
}

/// `observer_fcall_init`: called once per function, on its first call.
fn init(vm: &mut Vm, f: &ObservedFrame) -> bool {
    if ini(vm, "show_output") {
        let line = match display(f) {
            Some(n) => format!("{}<!-- init {n}() -->\n", indent(0)),
            None => format!("{}<!-- init '{}' -->\n", indent(0), file_name(f)),
        };
        out(vm, &line);
        if ini(vm, "show_init_backtrace") {
            show_init_backtrace(vm);
        }
    }
    true
}

/// `observer_begin`.
fn begin(vm: &mut Vm, f: &ObservedFrame) {
    let show = wants(vm, f) && ini(vm, "show_output");
    PRINTED.with(|p| p.borrow_mut().push(show));
    if !show {
        return;
    }
    let line = match display(f) {
        Some(n) => format!("{}<{n}>\n", indent(0)),
        None => format!("{}<file '{}'>\n", indent(0), file_name(f)),
    };
    out(vm, &line);
    DEPTH.with(|d| d.set(d.get() + 1));
}

/// `php_var_export_ex(retval, level, buf)`.
fn var_export(vm: &Vm, v: &Value, level: usize, buf: &mut String) {
    match v {
        Value::Null => buf.push_str("NULL"),
        Value::Bool(b) => buf.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => {
            let _ = write!(buf, "{i}");
        }
        Value::Float(f) => {
            buf.push_str(&php_runtime::float::format_double(
                *f,
                vm.serialize_precision,
                true,
            ));
        }
        Value::Str(s) => {
            buf.push('\'');
            for &b in s.as_bytes() {
                match b {
                    b'\'' => buf.push_str("\\'"),
                    b'\\' => buf.push_str("\\\\"),
                    0 => buf.push_str("' . \"\\0\" . '"),
                    _ => buf.push(char::from(b)),
                }
            }
            buf.push('\'');
        }
        Value::Array(a) => {
            if level > 1 {
                buf.push('\n');
                buf.push_str(&" ".repeat(level - 1));
            }
            buf.push_str("array (\n");
            for (k, val) in a.iter() {
                buf.push_str(&" ".repeat(level + 1));
                match k {
                    php_runtime::ArrayKey::Int(i) => {
                        let _ = write!(buf, "{i}");
                    }
                    php_runtime::ArrayKey::Str(s) => {
                        var_export(vm, &Value::Str(s.clone()), level, buf);
                    }
                }
                buf.push_str(" => ");
                var_export(vm, val, level + 2, buf);
                buf.push_str(",\n");
            }
            if level > 1 {
                buf.push_str(&" ".repeat(level - 1));
            }
            buf.push(')');
        }
        Value::Object(o) => {
            let _ = write!(
                buf,
                "\\{}::__set_state(array(\n))",
                o.class_name().to_string_lossy()
            );
        }
        Value::Resource(_) => buf.push_str("NULL"),
        Value::Reference(r) => var_export(vm, &r.get(), level, buf),
    }
}

/// `get_retval_info`.
fn retval_info(vm: &Vm, retval: Option<&Value>) -> String {
    let show_value = ini(vm, "show_return_value");
    if !show_value && !ini(vm, "show_return_type") {
        return String::new();
    }
    let mut s = String::from(":");
    let v = retval.map(|v| match v {
        Value::Reference(r) => r.get(),
        v => v.clone(),
    });
    match v {
        None => s.push_str("NULL"),
        Some(Value::Object(o)) if show_value => {
            let _ = write!(s, "object({})#{}", o.class_name().to_string_lossy(), o.handle());
        }
        Some(v) if show_value => {
            var_export(vm, &v, 2 * DEPTH.with(Cell::get) + 3, &mut s);
        }
        Some(v) => s.push_str(&crate::util::type_name(&v)),
    }
    s
}

/// `observer_end`.
fn end(vm: &mut Vm, f: &ObservedFrame, retval: Option<&Value>) {
    let shown = PRINTED.with(|p| p.borrow_mut().pop()).unwrap_or(false);
    if ini(vm, "set_vm_interrupt_on_end") && vm.current_exception().is_none() && wants(vm, f) {
        THROW_ON_INTERRUPT.with(|t| t.set(true));
        php_vm::interrupt::request_interrupt();
    }
    if !shown {
        return;
    }
    if let Some(e) = vm.current_exception() {
        let line = format!(
            "{}<!-- Exception: {} -->\n",
            indent(0),
            e.class_name().to_string_lossy()
        );
        out(vm, &line);
    }
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    let line = match display(f) {
        Some(n) => format!("{}</{n}{}>\n", indent(0), retval_info(vm, retval)),
        None => format!("{}</file '{}'>\n", indent(0), file_name(f)),
    };
    out(vm, &line);
}

/// `zend_test_execute_internal`: around every native call.
fn internal_enter(vm: &mut Vm, f: &ObservedFrame) {
    if let Some(n) = display(f) {
        let line = format!("{}<!-- internal enter {n}() -->\n", indent(0));
        out(vm, &line);
    }
    DEPTH.with(|d| d.set(d.get() + 1));
}

fn internal_leave(vm: &mut Vm, f: &ObservedFrame, retval: Option<&Value>) {
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    let Some(n) = display(f) else {
        return;
    };
    if let Some(e) = vm.current_exception() {
        let line = format!(
            "{}<!-- Exception: {} -->\n",
            indent(0),
            e.class_name().to_string_lossy()
        );
        out(vm, &line);
    }
    let line = format!(
        "{}<!-- internal leave {n}(){} -->\n",
        indent(0),
        retval_info(vm, retval)
    );
    out(vm, &line);
}

/// `zend_test_interrupt_function`.
fn on_interrupt(vm: &mut Vm) -> VmResult<()> {
    if THROW_ON_INTERRUPT.with(|t| t.replace(false)) {
        return Err(vm.throw("Exception", "VM interrupt"));
    }
    Ok(())
}

fn request_startup(_vm: &mut Vm) {
    DEPTH.with(|d| d.set(0));
    PRINTED.with(|p| p.borrow_mut().clear());
    THROW_ON_INTERRUPT.with(|t| t.set(false));
}

/// `zend_test_observer_init`: installs the hooks when enabled (the INI
/// values are final at module startup: `PHP_INI_SYSTEM`).
fn module_startup(vm: &mut Vm) {
    let enabled = ini(vm, "enabled");
    let execute_internal = ini(vm, "execute_internal");
    if !enabled && !execute_internal {
        return;
    }
    vm.set_observer(ObserverHooks {
        init: enabled.then_some(init as fn(&mut Vm, &ObservedFrame) -> bool),
        begin: Some(begin),
        end: Some(end),
        internal_enter: execute_internal.then_some(internal_enter as fn(&mut Vm, &ObservedFrame)),
        internal_leave: execute_internal
            .then_some(internal_leave as fn(&mut Vm, &ObservedFrame, Option<&Value>)),
    });
    if ini(vm, "set_vm_interrupt_on_end") {
        vm.register_interrupt_handler(on_interrupt);
    }
}

pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.on_module_startup(module_startup);
    m.on_request_startup(request_startup);
}
