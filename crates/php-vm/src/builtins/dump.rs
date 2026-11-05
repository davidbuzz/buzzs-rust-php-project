//! `var_dump()`, `debug_zval_dump()`, `print_r()` and `var_export()`
//! (`ext/standard/var.c`, `Zend/zend.c`).
//!
//! Unlike the context-free printers in `php_runtime::dump`, these use the
//! process-wide recursion guards ([`RecursionGuard`]), so a nested call made
//! from `__debugInfo()` (or from a `jsonSerialize()` it triggers) sees the
//! objects already being dumped, and they stream their output like
//! `php_printf()`: everything produced so far is written before user code
//! (`__debugInfo()`) runs.

use crate::vm::{Vm, VmResult};
use php_runtime::dump::{export_string, unmangle_property_name};
use php_runtime::float::{format_double, format_double_h};
use php_runtime::ops::{GuardKind, RecursionGuard};
use php_runtime::{
    ArrayKey, ErrorLevel, Obj, ObjectDump, ObjectKind, PropertyPurpose, PropertySlot,
    RuntimeContext, Value,
};

/// Output of a dump: streamed through the output layer, or collected
/// (`print_r($v, true)`, `var_export($v, true)`).
pub(crate) struct Out {
    buf: Vec<u8>,
    stream: bool,
}

impl Out {
    /// Output written with `echo` semantics.
    pub(crate) fn stream() -> Self {
        Out {
            buf: Vec::new(),
            stream: true,
        }
    }

    /// Output collected into a string.
    pub(crate) fn collect() -> Self {
        Out {
            buf: Vec::new(),
            stream: false,
        }
    }

    fn put(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    fn spaces(&mut self, n: usize) {
        self.buf.extend(std::iter::repeat_n(b' ', n));
    }

    /// Writes what has been produced so far (before user code runs).
    pub(crate) fn flush(&mut self, vm: &mut Vm) {
        if self.stream && !self.buf.is_empty() {
            let b = std::mem::take(&mut self.buf);
            vm.echo(&b);
        }
    }

    /// The collected bytes (streamed output is flushed instead).
    pub(crate) fn finish(mut self, vm: &mut Vm) -> Vec<u8> {
        self.flush(vm);
        self.buf
    }
}

/// Bytes up to the first NUL (`%s` semantics).
fn cstr(b: &[u8]) -> &[u8] {
    b.iter().position(|&c| c == 0).map_or(b, |p| &b[..p])
}

fn object_dump(
    vm: &mut Vm,
    out: &mut Out,
    o: &Obj,
    purpose: PropertyPurpose,
) -> VmResult<ObjectDump> {
    out.flush(vm);
    if purpose == PropertyPurpose::Debug {
        if let Some(d) = debug_info(vm, o)? {
            return Ok(d);
        }
    }
    vm.object_dump(o, purpose).map_err(|e| vm.rt_throw(e))
}

/// `zend_std_get_debug_info()` for classes with a user `__debugInfo()`:
/// a returned reference is unwrapped, `null` is deprecated (empty table),
/// anything else is fatal.
fn debug_info(vm: &mut Vm, o: &Obj) -> VmResult<Option<ObjectDump>> {
    let cls = crate::object::class_of(o).clone();
    if cls
        .hooks
        .as_ref()
        .is_some_and(|h| h.properties_for.is_some())
    {
        return Ok(None);
    }
    let Some(m) = cls.magic.debug_info.clone() else {
        return Ok(None);
    };
    let table = match vm.call_method(o, &m, Vec::new())?.deref() {
        Value::Array(a) => a,
        Value::Null => {
            vm.deprecated(&format!(
                "Returning null from {}::__debugInfo() is deprecated, return an empty array instead",
                cls.name
            ))?;
            php_runtime::ZArray::new()
        }
        _ => return Err(vm.fatal("__debugInfo() must return an array")),
    };
    let kind = if cls.is_enum() {
        ObjectKind::Enum {
            case_name: vm.enum_case_name(o).unwrap_or_default(),
            backing_type: cls.backing.map(|b| {
                if b == crate::types::mask::LONG {
                    "int"
                } else {
                    "string"
                }
            }),
            has_debug_info: true,
        }
    } else if cls.lc_name.as_bytes() == b"stdclass" {
        ObjectKind::StdClass
    } else {
        ObjectKind::Regular {
            lazy_prefix: vm.lazy_prefix(o),
        }
    };
    let properties = table
        .iter()
        .map(|(k, v)| (k.clone(), PropertySlot::Value(v.clone())))
        .collect();
    Ok(Some(ObjectDump {
        kind,
        properties: Some(properties),
    }))
}

/// The property table of an enum case (`name`, `value`) for printers that
/// show it (`print_r`, `debug_zval_dump`).
fn enum_props(vm: &mut Vm, o: &Obj) -> VmResult<Vec<(ArrayKey, PropertySlot)>> {
    let t = vm
        .object_properties(o, PropertyPurpose::Debug)
        .map_err(|e| vm.rt_throw(e))?;
    Ok(t.map(|t| {
        t.iter()
            .map(|(k, v)| (k.clone(), PropertySlot::Value(v.clone())))
            .collect()
    })
    .unwrap_or_default())
}

// ---------------------------------------------------------------------------
// Native stack limit (`ZEND_CHECK_STACK_LIMIT`)
// ---------------------------------------------------------------------------

/// Approximate C stack bytes PHP uses per nesting level of the printers.
const FRAME_BYTES: i64 = 128;
/// The stack PHP assumes when `zend.max_allowed_stack_size` is 0 (auto).
const DEFAULT_STACK: i64 = 8 << 20;

thread_local! {
    static DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The usable stack in bytes (`zend.max_allowed_stack_size -
/// zend.reserved_stack_size`).
fn stack_budget(vm: &Vm) -> i64 {
    let max = vm.ini_long("zend.max_allowed_stack_size");
    let max = if max <= 0 { DEFAULT_STACK } else { max };
    (max - vm.ini_long("zend.reserved_stack_size").max(0)).max(FRAME_BYTES)
}

/// One nesting level of a printer; `None` when PHP's C stack would have
/// overflowed (`zend_call_stack_overflowed()`).
pub(super) struct Depth;

impl Depth {
    pub(super) fn enter(vm: &Vm) -> Option<Depth> {
        let limit = usize::try_from(stack_budget(vm) / FRAME_BYTES).unwrap_or(usize::MAX);
        DEPTH.with(|d| {
            if d.get() >= limit {
                return None;
            }
            d.set(d.get() + 1);
            Some(Depth)
        })
    }
}

impl Drop for Depth {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// `zend_call_stack_size_error()`.
pub(crate) fn stack_size_error(vm: &mut Vm) -> crate::vm::Throw {
    let size = stack_budget(vm);
    vm.make_throw(
        "Error",
        &format!(
            "Maximum call stack size of {size} bytes (zend.max_allowed_stack_size - zend.reserved_stack_size) reached. Infinite recursion?"
        ),
    )
}

// ---------------------------------------------------------------------------
// var_dump / debug_zval_dump
// ---------------------------------------------------------------------------

/// Refcount bookkeeping for `debug_zval_dump()` (`None` for `var_dump()`):
/// the number of handles held by the dumper/VM beyond the ones PHP counts.
#[derive(Clone, Copy)]
pub(crate) struct Zval {
    extra: usize,
}

fn put_key(out: &mut Out, k: &ArrayKey, level: usize) {
    out.spaces(level + 1);
    match k {
        ArrayKey::Int(i) => out.put(format!("[{i}]=>\n").as_bytes()),
        ArrayKey::Str(s) => {
            out.put(b"[\"");
            out.put(s.as_bytes());
            out.put(b"\"]=>\n");
        }
    }
}

fn put_property_key(out: &mut Out, k: &ArrayKey, level: usize) {
    out.spaces(level + 1);
    match k {
        ArrayKey::Int(i) => out.put(format!("[{i}]=>\n").as_bytes()),
        ArrayKey::Str(s) => {
            out.put(b"[\"");
            match unmangle_property_name(s.as_bytes()) {
                (Some(class), prop) => {
                    out.put(cstr(prop));
                    if class.first() == Some(&b'*') {
                        out.put(b"\":protected");
                    } else {
                        out.put(b"\":\"");
                        out.put(cstr(class));
                        out.put(b"\":private");
                    }
                }
                (None, prop) => {
                    out.put(prop);
                    out.put(b"\"");
                }
            }
            out.put(b"]=>\n");
        }
    }
}

fn rc(count: usize, z: Zval) -> usize {
    count.saturating_sub(z.extra).max(1)
}

/// `php_var_dump()` (`zv == None`) / `php_debug_zval_dump()` of one value.
#[allow(clippy::too_many_lines)]
pub(crate) fn var_dump(
    vm: &mut Vm,
    out: &mut Out,
    value: &Value,
    level: usize,
    zv: Option<Zval>,
) -> VmResult<()> {
    if level > 1 {
        out.spaces(level - 1);
    }
    let mut common: &[u8] = b"";
    let deref;
    let value = match value {
        Value::Reference(r) => {
            if let Some(z) = zv {
                out.put(format!("reference refcount({}) {{\n", rc(r.refcount(), z)).as_bytes());
                let inner = r.get();
                var_dump(vm, out, &inner, level + 2, Some(Zval { extra: 1 }))?;
                if level > 1 {
                    out.spaces(level - 1);
                }
                out.put(b"}\n");
                return Ok(());
            }
            if r.refcount() > 1 {
                common = b"&";
            }
            deref = r.get();
            &deref
        }
        v => v,
    };
    match value {
        Value::Null => {
            out.put(common);
            out.put(b"NULL\n");
        }
        Value::Bool(b) => {
            out.put(common);
            out.put(if *b {
                b"bool(true)\n"
            } else {
                b"bool(false)\n"
            });
        }
        Value::Int(i) => {
            out.put(common);
            out.put(format!("int({i})\n").as_bytes());
        }
        Value::Float(f) => {
            out.put(common);
            let s = format_double_h(*f, vm.serialize_precision());
            out.put(format!("float({s})\n").as_bytes());
        }
        Value::Str(s) => {
            out.put(common);
            out.put(format!("string({}) \"", s.len()).as_bytes());
            out.put(s.as_bytes());
            match zv {
                Some(z) if !s.is_interned() => {
                    out.put(format!("\" refcount({})\n", rc(s.refcount(), z)).as_bytes());
                }
                Some(_) => out.put(b"\" interned\n"),
                None => out.put(b"\"\n"),
            }
        }
        Value::Array(a) => {
            let Some(_guard) = RecursionGuard::try_enter(GuardKind::Debug, a.addr()) else {
                out.put(b"*RECURSION*\n");
                return Ok(());
            };
            out.put(common);
            match zv {
                Some(z) => {
                    let packed = if a.is_packed() { "packed " } else { "" };
                    if a.is_empty() {
                        out.put(b"array(0) interned {\n");
                    } else {
                        let n = rc(a.refcount(), z);
                        out.put(format!("array({}) {packed}refcount({n}){{\n", a.len()).as_bytes());
                    }
                }
                None => out.put(format!("array({}) {{\n", a.len()).as_bytes()),
            }
            let a = a.clone();
            for (k, v) in a.iter() {
                let Some(_depth) = Depth::enter(vm) else {
                    out.spaces(level + 1);
                    out.put(b"nesting level too deep");
                    continue;
                };
                put_key(out, k, level);
                var_dump(vm, out, v, level + 2, zv.map(|_| Zval { extra: 0 }))?;
            }
            if level > 1 {
                out.spaces(level - 1);
            }
            out.put(b"}\n");
        }
        Value::Object(o) => var_dump_object(vm, out, o, common, level, zv)?,
        Value::Resource(r) => {
            out.put(common);
            let t = r
                .type_name()
                .map_or_else(|| b"Unknown".to_vec(), |t| t.as_bytes().to_vec());
            out.put(format!("resource({}) of type (", r.handle()).as_bytes());
            out.put(&t);
            match zv {
                // Resources do not expose their handle count: report the
                // usual "variable + argument" (or "container") count.
                Some(_) => {
                    out.put(format!(") refcount({})\n", if level == 1 { 2 } else { 1 }).as_bytes());
                }
                None => out.put(b")\n"),
            }
        }
        Value::Reference(_) => out.put(b"UNKNOWN:0\n"),
    }
    Ok(())
}

fn var_dump_object(
    vm: &mut Vm,
    out: &mut Out,
    o: &Obj,
    common: &[u8],
    level: usize,
    zv: Option<Zval>,
) -> VmResult<()> {
    let cls = crate::object::class_of(o).clone();
    if zv.is_none() && cls.is_enum() && cls.magic.debug_info.is_none() {
        let case = vm.enum_case_name(o).unwrap_or_default();
        out.put(common);
        out.put(b"enum(");
        out.put(cls.name.as_bytes());
        out.put(b"::");
        out.put(case.as_bytes());
        out.put(b")\n");
        return Ok(());
    }
    // Protect before `__debugInfo()` runs (GH-8044).
    let Some(_guard) = RecursionGuard::try_enter(GuardKind::Debug, o.addr()) else {
        out.put(b"*RECURSION*\n");
        return Ok(());
    };
    let refs_before = o.refcount();
    let ObjectDump { kind, properties } = object_dump(vm, out, o, PropertyPurpose::Debug)?;
    // `php_debug_zval_dump` prints the refcount after
    // `zend_get_properties_for()`: a table a native handler builds
    // (`SplFixedArray`) holds its own references to the values, the
    // standard property table does not (GH-8044).
    let refs = if crate::ext::hooks_of(o).is_some_and(|h| h.properties_for.is_some()) {
        o.refcount()
    } else {
        refs_before
    };
    let properties = match (&kind, properties) {
        (ObjectKind::Enum { .. }, None) if zv.is_some() => Some(enum_props(vm, o)?),
        (_, p) => p,
    };
    let count = properties.as_ref().map_or(0, |p| {
        p.iter()
            .filter(|(_, s)| matches!(s, PropertySlot::Value(_)))
            .count()
    });
    out.put(common);
    // `%pS`: the full name (anonymous classes include `\0file:line$n`).
    let class = cls.name.clone();
    if let (ObjectKind::Enum { case_name, .. }, None) = (&kind, zv) {
        out.put(b"enum(");
        out.put(class.as_bytes());
        out.put(b"::");
        out.put(case_name.as_bytes());
        out.put(format!(") ({count}) {{\n").as_bytes());
    } else {
        if let ObjectKind::Regular { lazy_prefix } = &kind {
            out.put(lazy_prefix.as_bytes());
        }
        out.put(b"object(");
        out.put(class.as_bytes());
        out.put(format!(")#{} ({count}) ", o.handle()).as_bytes());
        match zv {
            Some(z) => out.put(format!("refcount({}){{\n", rc(refs, z)).as_bytes()),
            None => out.put(b"{\n"),
        }
    }
    for (k, slot) in properties.iter().flatten() {
        let Some(_depth) = Depth::enter(vm) else {
            out.spaces(level + 1);
            out.put(b"nesting level too deep");
            continue;
        };
        put_property_key(out, k, level);
        match slot {
            PropertySlot::Value(v) => {
                // The synthetic proxy instance table owns a PHP-visible reference.
                let extra = usize::from(
                    !crate::object::data(o)
                        .lazy
                        .borrow()
                        .as_ref()
                        .is_some_and(|info| info.proxy && info.initialized),
                );
                var_dump(vm, out, v, level + 2, zv.map(|_| Zval { extra }))?;
            }
            PropertySlot::Uninitialized(t) => {
                out.spaces(level + 1);
                out.put(b"uninitialized(");
                out.put(t.as_bytes());
                out.put(b")\n");
            }
        }
    }
    if level > 1 {
        out.spaces(level - 1);
    }
    out.put(b"}\n");
    Ok(())
}

/// `debug_zval_dump()` of one argument; `extra` is the number of handles
/// held beyond PHP's own (the variable and the argument slot).
pub(crate) fn debug_zval_dump(
    vm: &mut Vm,
    out: &mut Out,
    value: &Value,
    extra: usize,
) -> VmResult<()> {
    var_dump(vm, out, value, 1, Some(Zval { extra }))
}

// ---------------------------------------------------------------------------
// print_r
// ---------------------------------------------------------------------------

/// `zend_print_zval_r_to_buf()`.
pub(crate) fn print_r(vm: &mut Vm, out: &mut Out, value: &Value, indent: usize) -> VmResult<()> {
    match value {
        Value::Array(a) => {
            out.put(b"Array\n");
            let Some(_guard) = RecursionGuard::try_enter(GuardKind::Debug, a.addr()) else {
                out.put(b" *RECURSION*");
                return Ok(());
            };
            let entries: Vec<(ArrayKey, PropertySlot)> = a
                .iter()
                .map(|(k, v)| (k.clone(), PropertySlot::Value(v.clone())))
                .collect();
            print_hash(vm, out, &entries, indent, false)
        }
        Value::Object(o) => {
            let cls = crate::object::class_of(o).clone();
            out.put(cstr(o.class_name().as_bytes()));
            if cls.is_enum() {
                out.put(b" Enum");
                if let Some(b) = cls.backing {
                    out.put(if b == crate::types::mask::LONG {
                        b":int"
                    } else {
                        b":string"
                    });
                }
                out.put(b"\n");
            } else {
                out.put(b" Object\n");
            }
            let Some(_guard) = RecursionGuard::try_enter(GuardKind::Debug, o.addr()) else {
                out.put(b" *RECURSION*");
                return Ok(());
            };
            let info = object_dump(vm, out, o, PropertyPurpose::Debug)?;
            let entries = match (&info.kind, info.properties) {
                (ObjectKind::Enum { .. }, None) => enum_props(vm, o)?,
                (_, p) => p.unwrap_or_default(),
            };
            print_hash(vm, out, &entries, indent, true)
        }
        Value::Float(f) => {
            out.put(format_double(*f, vm.precision(), false).as_bytes());
            Ok(())
        }
        Value::Reference(r) => {
            let v = r.get();
            print_r(vm, out, &v, indent)
        }
        other => {
            let s = vm.to_zstr(other)?;
            out.put(s.as_bytes());
            Ok(())
        }
    }
}

fn print_hash(
    vm: &mut Vm,
    out: &mut Out,
    entries: &[(ArrayKey, PropertySlot)],
    indent: usize,
    is_object: bool,
) -> VmResult<()> {
    out.spaces(indent);
    out.put(b"(\n");
    let inner = indent + 4;
    for (k, slot) in entries {
        let PropertySlot::Value(v) = slot else {
            continue;
        };
        out.spaces(inner);
        out.put(b"[");
        match k {
            ArrayKey::Int(i) => out.put(i.to_string().as_bytes()),
            ArrayKey::Str(s) if is_object => match unmangle_property_name(s.as_bytes()) {
                (Some(class), prop) => {
                    out.put(prop);
                    if class.first() == Some(&b'*') {
                        out.put(b":protected");
                    } else {
                        out.put(b":");
                        out.put(cstr(class));
                        out.put(b":private");
                    }
                }
                (None, prop) => out.put(prop),
            },
            ArrayKey::Str(s) => out.put(s.as_bytes()),
        }
        out.put(b"] => ");
        print_r(vm, out, v, inner + 4)?;
        out.put(b"\n");
    }
    out.spaces(indent);
    out.put(b")\n");
    Ok(())
}

// ---------------------------------------------------------------------------
// var_export
// ---------------------------------------------------------------------------

fn circular(vm: &mut Vm, out: &mut Out) -> VmResult<()> {
    vm.diagnostic(
        ErrorLevel::Warning,
        "var_export does not handle circular references",
    )
    .map_err(|e| vm.rt_throw(e))?;
    out.put(b"NULL");
    Ok(())
}

/// `php_var_export_ex()`.
pub(crate) fn var_export(vm: &mut Vm, out: &mut Out, value: &Value, level: usize) -> VmResult<()> {
    let Some(_depth) = Depth::enter(vm) else {
        return Err(stack_size_error(vm));
    };
    match value {
        Value::Null | Value::Resource(_) => out.put(b"NULL"),
        Value::Bool(b) => out.put(if *b { b"true" } else { b"false" }),
        Value::Int(i) => {
            if *i == i64::MIN {
                out.put(b"-9223372036854775807-1");
            } else {
                out.put(i.to_string().as_bytes());
            }
        }
        Value::Float(f) => out.put(format_double(*f, vm.serialize_precision(), true).as_bytes()),
        Value::Str(s) => out.put(&export_string(s.as_bytes())),
        Value::Array(a) => {
            let Some(_guard) = RecursionGuard::try_enter(GuardKind::Export, a.addr()) else {
                return circular(vm, out);
            };
            if level > 1 {
                out.put(b"\n");
                out.spaces(level - 1);
            }
            out.put(b"array (\n");
            let a = a.clone();
            for (k, v) in a.iter() {
                out.spaces(level + 1);
                match k {
                    ArrayKey::Int(i) => out.put(i.to_string().as_bytes()),
                    ArrayKey::Str(s) => out.put(&export_string(s.as_bytes())),
                }
                out.put(b" => ");
                var_export(vm, out, v, level + 2)?;
                out.put(b",\n");
            }
            if level > 1 {
                out.spaces(level - 1);
            }
            out.put(b")");
        }
        Value::Object(o) => return var_export_object(vm, out, o, level),
        Value::Reference(r) => {
            let v = r.get();
            return var_export(vm, out, &v, level);
        }
    }
    Ok(())
}

fn var_export_object(vm: &mut Vm, out: &mut Out, o: &Obj, level: usize) -> VmResult<()> {
    let Some(_guard) = RecursionGuard::try_enter(GuardKind::Export, o.addr()) else {
        return circular(vm, out);
    };
    let info = object_dump(vm, out, o, PropertyPurpose::VarExport)?;
    if level > 1 {
        out.put(b"\n");
        out.spaces(level - 1);
    }
    let class = o.class_name();
    match &info.kind {
        ObjectKind::StdClass => out.put(b"(object) array(\n"),
        ObjectKind::Enum { case_name, .. } => {
            out.put(b"\\");
            out.put(class.as_bytes());
            out.put(b"::");
            out.put(case_name.as_bytes());
            return Ok(());
        }
        ObjectKind::Regular { .. } => {
            out.put(b"\\");
            out.put(class.as_bytes());
            out.put(b"::__set_state(array(\n");
        }
    }
    for (k, slot) in info.properties.iter().flatten() {
        let PropertySlot::Value(v) = slot else {
            continue;
        };
        out.spaces(level + 2);
        match k {
            ArrayKey::Int(i) => out.put(i.to_string().as_bytes()),
            ArrayKey::Str(s) => {
                let (_, prop) = unmangle_property_name(s.as_bytes());
                out.put(&export_string(prop));
            }
        }
        out.put(b" => ");
        var_export(vm, out, v, level + 2)?;
        out.put(b",\n");
    }
    if level > 1 {
        out.spaces(level - 1);
    }
    match info.kind {
        ObjectKind::StdClass => out.put(b")"),
        _ => out.put(b"))"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{cstr, rc, Zval};

    #[test]
    fn cstr_stops_at_nul() {
        assert_eq!(cstr(b"ab\0cd"), b"ab");
        assert_eq!(cstr(b"abc"), b"abc");
    }

    #[test]
    fn refcounts_subtract_own_handles() {
        assert_eq!(rc(5, Zval { extra: 2 }), 3);
        assert_eq!(rc(1, Zval { extra: 2 }), 1);
    }
}
