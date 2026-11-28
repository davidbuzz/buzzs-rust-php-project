// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `PDOStatement` and `PDORow` classes and statement iteration.

use crate::host::{raw_prop, stmt_of, with_host, VmHost};
use php_pdo::consts::{FETCH_DEFAULT, FETCH_ORI_NEXT, PARAM_STR};
use php_pdo::{ParamId, PdoRow, PdoStatement, StmtIterator};
use php_runtime::{Obj, PropertyPurpose, Value, ZArray, ZStr};
use php_vm::ext::{
    acc, class_flags, Args, ClassBuilder, ClassHooks, DimensionCtx, NativeIterator, VmResult,
};
use php_vm::func::Func;
use php_vm::object::class_of;
use php_vm::Vm;

/// The `create_object` handler of `PDOStatement` (`pdo_dbstmt_new`).
fn stmt_create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    let s = PdoStatement::new();
    s.attach_object(o);
    php_vm::object::set_internal(o, s);
    Ok(())
}

fn this_stmt(a: &Args) -> PdoStatement {
    let o = a.this_obj();
    stmt_of(&o).unwrap_or_else(|| {
        let s = PdoStatement::new();
        s.attach_object(&o);
        php_vm::object::set_internal(&o, s.clone());
        s
    })
}

fn param_id(v: &Value) -> ParamId {
    match v.deref() {
        Value::Int(i) => ParamId::Position(i),
        Value::Str(s) => ParamId::Name(s),
        _ => ParamId::Name(ZStr::empty()),
    }
}

fn int_or(a: &Args, n: usize, default: i64) -> i64 {
    if a.has(n) {
        a.int(n)
    } else {
        default
    }
}

/// The by-reference argument `n` as passed (a [`Value::Reference`]).
fn by_ref(a: &Args, n: usize) -> Value {
    a.values.get(n).cloned().unwrap_or(Value::Null)
}

fn bind_column(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    let id = param_id(&a.at(0));
    let var = by_ref(a, 1);
    let ty = int_or(a, 2, PARAM_STR);
    let max = a.int(3);
    let opts = a.opt(4);
    with_host(vm, |h| s.bind_column(h, &id, var, ty, max, opts)).map(Value::Bool)
}

fn bind_param(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    let id = param_id(&a.at(0));
    let var = by_ref(a, 1);
    let ty = int_or(a, 2, PARAM_STR);
    let max = a.int(3);
    let opts = a.opt(4);
    with_host(vm, |h| s.bind_param(h, &id, var, ty, max, opts)).map(Value::Bool)
}

fn bind_value(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    let id = param_id(&a.at(0));
    let value = a.at(1);
    let ty = int_or(a, 2, PARAM_STR);
    with_host(vm, |h| s.bind_value(h, &id, &value, ty)).map(Value::Bool)
}

fn close_cursor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    with_host(vm, |h| s.close_cursor(h)).map(Value::Bool)
}

fn column_count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    with_host(vm, |h| s.column_count(h)).map(Value::Int)
}

fn debug_dump_params(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    with_host(vm, |h| s.debug_dump_params(h))
}

fn error_code(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    with_host(vm, |h| s.error_code(h))
}

fn error_info(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    with_host(vm, |h| s.error_info(h)).map(Value::Array)
}

fn execute(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    let params = a.opt(0).map(|_| a.array(0));
    with_host(vm, |h| s.execute(h, params.as_ref())).map(Value::Bool)
}

fn fetch(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    let mode = int_or(a, 0, FETCH_DEFAULT);
    let ori = int_or(a, 1, FETCH_ORI_NEXT);
    let off = a.int(2);
    with_host(vm, |h| s.fetch(h, mode, ori, off))
}

fn fetch_all(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    let mode = if a.has(0) { Some(a.int(0)) } else { None };
    let rest: Vec<Value> = a.values.iter().skip(1).map(Value::deref).collect();
    with_host(vm, |h| s.fetch_all(h, mode, &rest)).map(Value::Array)
}

fn fetch_column(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    let col = a.int(0);
    with_host(vm, |h| s.fetch_column(h, col))
}

fn fetch_object(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    let class = a.opt(0).map(|_| a.str(0));
    let args: Option<ZArray> = if a.has(1) { Some(a.array(1)) } else { None };
    with_host(vm, |h| s.fetch_object(h, class.as_ref(), args.as_ref()))
}

fn get_attribute(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    let attr = a.int(0);
    with_host(vm, |h| s.get_attribute(h, attr))
}

fn get_column_meta(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    let col = a.int(0);
    with_host(vm, |h| s.get_column_meta(h, col))
}

fn next_rowset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    with_host(vm, |h| s.next_rowset(h)).map(Value::Bool)
}

fn row_count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    with_host(vm, |h| s.row_count(h)).map(Value::Int)
}

fn set_attribute(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    let attr = a.int(0);
    let value = a.at(1);
    with_host(vm, |h| s.set_attribute(h, attr, &value)).map(Value::Bool)
}

fn set_fetch_mode(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(a);
    let mode = a.int(0);
    let rest: Vec<Value> = a.values.iter().skip(1).map(Value::deref).collect();
    with_host(vm, |h| s.set_fetch_mode(h, mode, &rest)).map(Value::Bool)
}

fn get_iterator(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let it = native_iterator(vm, &o, false)?;
    vm.new_internal_iterator(it).map(Value::Object)
}

// ---------------------------------------------------------------------------
// Iteration
// ---------------------------------------------------------------------------

/// `pdo_stmt_iter_funcs`: `foreach` over a statement.
struct PdoIter(StmtIterator);

impl NativeIterator for PdoIter {
    fn rewind(&mut self, _vm: &mut Vm) -> VmResult<()> {
        Ok(())
    }
    fn valid(&mut self, _vm: &mut Vm) -> VmResult<bool> {
        Ok(self.0.valid())
    }
    fn current(&mut self, _vm: &mut Vm) -> VmResult<Value> {
        Ok(self.0.current().unwrap_or(Value::Null))
    }
    fn key(&mut self, _vm: &mut Vm) -> VmResult<Value> {
        Ok(self.0.key())
    }
    fn next(&mut self, vm: &mut Vm) -> VmResult<()> {
        let it = &mut self.0;
        with_host(vm, |h| it.move_forward(h))
    }
}

/// `zend_user_it_get_new_iterator`: iteration through a user `Iterator`
/// returned by an overridden `getIterator()`.
struct UserIter(Obj);

impl NativeIterator for UserIter {
    fn rewind(&mut self, vm: &mut Vm) -> VmResult<()> {
        vm.call_method_by_name(&self.0, "rewind", Vec::new())
            .map(|_| ())
    }
    fn valid(&mut self, vm: &mut Vm) -> VmResult<bool> {
        let v = vm.call_method_by_name(&self.0, "valid", Vec::new())?;
        vm.value_to_bool(&v)
    }
    fn current(&mut self, vm: &mut Vm) -> VmResult<Value> {
        vm.call_method_by_name(&self.0, "current", Vec::new())
    }
    fn key(&mut self, vm: &mut Vm) -> VmResult<Value> {
        vm.call_method_by_name(&self.0, "key", Vec::new())
    }
    fn next(&mut self, vm: &mut Vm) -> VmResult<()> {
        vm.call_method_by_name(&self.0, "next", Vec::new())
            .map(|_| ())
    }
}

fn native_iterator(vm: &mut Vm, o: &Obj, by_ref: bool) -> VmResult<Box<dyn NativeIterator>> {
    let st = stmt_of(o).unwrap_or_default();
    let it = with_host(vm, |h| st.iterator(h, by_ref))?;
    Ok(Box::new(PdoIter(it)))
}

/// The `get_iterator` handler: user subclasses overriding `getIterator()`
/// iterate through it.
fn stmt_get_iterator(vm: &mut Vm, o: &Obj, by_ref: bool) -> VmResult<Box<dyn NativeIterator>> {
    let cls = class_of(o).clone();
    let user = cls
        .find_method(b"getiterator")
        .is_some_and(|m| matches!(m.func, Func::User(_)));
    if user {
        let mut cur = vm.call_method_by_name(o, "getIterator", Vec::new())?;
        for _ in 0..64 {
            let Value::Object(it) = &cur else { break };
            let icls = class_of(it).clone();
            if icls.instance_of_name(b"iterator") {
                return Ok(Box::new(UserIter(it.clone())));
            }
            if icls.instance_of_name(b"iteratoraggregate") {
                if icls
                    .hooks
                    .as_ref()
                    .is_some_and(|h| h.get_iterator.is_some())
                {
                    if let Some(f) = icls.hooks.as_ref().and_then(|h| h.get_iterator) {
                        return f(vm, it, by_ref);
                    }
                }
                let next = vm.call_method_by_name(it, "getIterator", Vec::new())?;
                cur = next;
                continue;
            }
            break;
        }
        return Err(vm.throw(
            "Exception",
            &format!(
                "Objects returned by {}::getIterator() must be traversable or implement interface Iterator",
                cls.name.to_string_lossy()
            ),
        ));
    }
    native_iterator(vm, o, by_ref)
}

/// `write_property`: `queryString` is read-only once set.
fn stmt_write_property(vm: &mut Vm, o: &Obj, name: &ZStr, _v: &Value) -> VmResult<bool> {
    let initialized = raw_prop(o, "queryString").is_some();
    match PdoStatement::check_property_write(name.as_bytes(), initialized) {
        Ok(handled) => Ok(handled),
        Err(e) => Err(crate::host::rt_throw(vm, e)),
    }
}

/// `unset_property`: `queryString` cannot be unset.
fn stmt_unset_property(vm: &mut Vm, _o: &Obj, name: &ZStr) -> VmResult<bool> {
    match PdoStatement::check_property_unset(name.as_bytes()) {
        Ok(()) => Ok(false),
        Err(e) => Err(crate::host::rt_throw(vm, e)),
    }
}

// ---------------------------------------------------------------------------
// PDORow
// ---------------------------------------------------------------------------

fn row_of(o: &Obj) -> PdoRow {
    php_vm::object::internal::<PdoRow, _>(o, |r| r.clone()).unwrap_or_else(PdoRow::detached)
}

fn row_read_property(vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<Option<Value>> {
    let row = row_of(o);
    with_host(vm, |h| row.read_property(h, name)).map(Some)
}

fn row_write_property(vm: &mut Vm, _o: &Obj, _name: &ZStr, _v: &Value) -> VmResult<bool> {
    Err(crate::host::rt_throw(vm, PdoRow::write_property_error()))
}

fn row_has_property(vm: &mut Vm, o: &Obj, name: &ZStr, empty: bool) -> VmResult<Option<bool>> {
    let row = row_of(o);
    with_host(vm, |h| row.has_property(h, name, empty)).map(Some)
}

fn row_unset_property(vm: &mut Vm, _o: &Obj, _name: &ZStr) -> VmResult<bool> {
    Err(crate::host::rt_throw(vm, PdoRow::unset_property_error()))
}

/// `row_dim_read` as reached from `zend_fetch_dimension_address`: `&$row[]`
/// (no offset) throws "Cannot append", and a write-context fetch of a
/// scalar column raises the "Indirect modification" notice.
fn row_read_dimension(vm: &mut Vm, o: &Obj, k: &Value) -> VmResult<Option<Value>> {
    let ctx = vm.dimension_context();
    let offset = (!matches!(ctx, DimensionCtx::Append | DimensionCtx::AppendRw)).then_some(k);
    let row = row_of(o);
    let v = with_host(vm, |h| row.read_dimension(h, offset))?;
    if !matches!(ctx, DimensionCtx::Read | DimensionCtx::Is)
        && !matches!(v, Value::Reference(_) | Value::Object(_))
    {
        vm.raise(
            8,
            &format!(
                "Indirect modification of overloaded element of {} has no effect",
                o.class_name().to_string_lossy()
            ),
        )?;
    }
    Ok(Some(v))
}

fn row_write_dimension(vm: &mut Vm, _o: &Obj, k: Option<&Value>, _v: &Value) -> VmResult<bool> {
    Err(crate::host::rt_throw(
        vm,
        PdoRow::write_dimension_error(k.is_none()),
    ))
}

fn row_has_dimension(vm: &mut Vm, o: &Obj, k: &Value, empty: bool) -> VmResult<Option<bool>> {
    let row = row_of(o);
    with_host(vm, |h| row.has_dimension(h, k, empty)).map(Some)
}

fn row_unset_dimension(vm: &mut Vm, _o: &Obj, _k: &Value) -> VmResult<bool> {
    Err(crate::host::rt_throw(vm, PdoRow::unset_dimension_error()))
}

fn row_properties_for(vm: &mut Vm, o: &Obj, purpose: PropertyPurpose) -> VmResult<Option<ZArray>> {
    if purpose != PropertyPurpose::Debug {
        return Ok(None);
    }
    let row = row_of(o);
    with_host(vm, |h| row.debug_properties(h)).map(Some)
}

thread_local! {
    /// Set while [`create_row`] instantiates a `PDORow`.
    static CREATING_ROW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `row_get_ctor`: `new PDORow` (and reflection) throw; only the fetch
/// code creates rows.
fn row_create(vm: &mut Vm, _o: &Obj) -> VmResult<()> {
    if CREATING_ROW.with(std::cell::Cell::get) {
        return Ok(());
    }
    let e = {
        let mut h = VmHost::new(vm);
        PdoRow::constructor_error(&mut h)
    };
    Err(crate::host::rt_throw(vm, e))
}

/// Creates the `PDORow` object of a `PDO::FETCH_LAZY` fetch.
pub fn create_row(vm: &mut Vm, row: PdoRow) -> VmResult<Obj> {
    let Some(cls) = vm.find_class(b"PDORow") else {
        return Err(vm.throw("Error", "Class \"PDORow\" not found"));
    };
    CREATING_ROW.with(|c| c.set(true));
    let r = vm.new_object(&cls);
    CREATING_ROW.with(|c| c.set(false));
    let o = r?;
    php_vm::object::set_internal(&o, row);
    Ok(o)
}

/// `dbstmt_get_gc`: the bound variables and driver options, the fetch
/// target/constructor arguments and the owning `PDO` object.
fn stmt_gc(s: &PdoStatement, v: &mut php_vm::gc::GcVisitor<'_>) {
    for x in s.gc_values() {
        v.value(&x);
    }
}

/// Registers `PDOStatement` and `PDORow`.
pub fn register(vm: &mut Vm) {
    php_vm::gc::register_traverse_with::<PdoStatement>(stmt_gc);
    let pub_ = acc::PUBLIC;
    ClassBuilder::new("PDOStatement")
        .flags(class_flags::NOT_SERIALIZABLE | class_flags::NO_CLONE)
        .implements(vm, &["IteratorAggregate"])
        .prop("queryString", pub_, Some("string"), None)
        .hooks(ClassHooks {
            create: Some(stmt_create),
            get_iterator: Some(stmt_get_iterator),
            write_property: Some(stmt_write_property),
            unset_property: Some(stmt_unset_property),
            ..Default::default()
        })
        .method(
            "bindColumn(string|int $column, mixed &$var, int $type = PDO::PARAM_STR, int $maxLength = 0, mixed $driverOptions = null): bool",
            pub_,
            bind_column,
        )
        .method(
            "bindParam(string|int $param, mixed &$var, int $type = PDO::PARAM_STR, int $maxLength = 0, mixed $driverOptions = null): bool",
            pub_,
            bind_param,
        )
        .method(
            "bindValue(string|int $param, mixed $value, int $type = PDO::PARAM_STR): bool",
            pub_,
            bind_value,
        )
        .method("closeCursor(): bool", pub_, close_cursor)
        .method("columnCount(): int", pub_, column_count)
        .method("debugDumpParams(): ?bool", pub_, debug_dump_params)
        .method("errorCode(): ?string", pub_, error_code)
        .method("errorInfo(): array", pub_ | acc::PURE, error_info)
        .method("execute(?array $params = null): bool", pub_, execute)
        .method(
            "fetch(int $mode = PDO::FETCH_DEFAULT, int $cursorOrientation = PDO::FETCH_ORI_NEXT, int $cursorOffset = 0): mixed",
            pub_,
            fetch,
        )
        .method(
            "fetchAll(int $mode = PDO::FETCH_DEFAULT, mixed ...$args): array",
            pub_,
            fetch_all,
        )
        .method("fetchColumn(int $column = 0): mixed", pub_, fetch_column)
        .method(
            "fetchObject(?string $class = \"stdClass\", array $constructorArgs = []): object|false",
            pub_,
            fetch_object,
        )
        .method("getAttribute(int $name): mixed", pub_, get_attribute)
        .method("getColumnMeta(int $column): array|false", pub_, get_column_meta)
        .method("nextRowset(): bool", pub_, next_rowset)
        .method("rowCount(): int", pub_, row_count)
        .method(
            "setAttribute(int $attribute, mixed $value): bool",
            pub_,
            set_attribute,
        )
        .method("setFetchMode(int $mode, mixed ...$args): true", pub_, set_fetch_mode)
        .method("getIterator(): Iterator", pub_, get_iterator)
        .register(vm);

    // ALLOW_DYNAMIC: the VM has no `get_property_ptr_ptr` hook; nested
    // writes (`$row->a->b = 1`) must not raise the dynamic-property
    // deprecation (direct writes still go through `write_property`).
    ClassBuilder::new("PDORow")
        .flags(
            class_flags::FINAL
                | class_flags::NOT_SERIALIZABLE
                | class_flags::NO_CLONE
                | class_flags::ALLOW_DYNAMIC,
        )
        .prop("queryString", pub_, Some("string"), None)
        .hooks(ClassHooks {
            read_property: Some(row_read_property),
            write_property: Some(row_write_property),
            has_property: Some(row_has_property),
            unset_property: Some(row_unset_property),
            read_dimension: Some(row_read_dimension),
            write_dimension: Some(row_write_dimension),
            has_dimension: Some(row_has_dimension),
            unset_dimension: Some(row_unset_dimension),
            properties_for: Some(row_properties_for),
            create: Some(row_create),
            ..Default::default()
        })
        .register(vm);
}
