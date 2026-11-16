// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `PDORow`, the object `PDO::FETCH_LAZY` returns: its property and
//! dimension handlers read columns of the statement's current row on
//! demand (`row_prop_read`, `row_dim_read`, ...).

use crate::fetch::fetch_value;
use crate::host::{Ctx, PdoException, PdoHost};
use crate::stmt::{PdoStatement, Stmt};
use php_runtime::numeric::{is_numeric_string, Number};
use php_runtime::{ArrayKey, PropertyPurpose, RtResult, RuntimeError, Value, ZArray, ZStr};

/// The state of a `PDORow` object (`pdo_row_t`): the statement it reads
/// from. The VM stores it in the object created by
/// [`PdoHost::create_row`].
#[derive(Debug, Clone)]
pub struct PdoRow {
    stmt: Option<PdoStatement>,
    /// Keeps the statement object alive (`GC_ADDREF(&stmt->std)`).
    stmt_object: Option<Value>,
}

impl PdoRow {
    pub(crate) fn new(stmt: PdoStatement) -> Self {
        let stmt_object = stmt.object();
        PdoRow {
            stmt: Some(stmt),
            stmt_object,
        }
    }

    /// A row object without statement (created without running the
    /// constructor, e.g. through reflection); every handler sees no
    /// columns.
    pub fn detached() -> Self {
        PdoRow {
            stmt: None,
            stmt_object: None,
        }
    }

    /// The statement the row belongs to.
    pub fn statement(&self) -> Option<&PdoStatement> {
        self.stmt.as_ref()
    }

    /// `row_get_ctor`: `new PDORow` throws this `PDOException`.
    pub fn constructor_error(host: &mut dyn PdoHost) -> RuntimeError {
        host.new_pdo_exception(PdoException::new("You may not create a PDORow manually", 0))
    }

    fn stmt(&self) -> Option<&Stmt> {
        self.stmt.as_ref().map(|s| &*s.0)
    }

    fn read_column_name(&self, cx: &mut Ctx<'_>, name: &[u8]) -> Option<Value> {
        let st = self.stmt()?;
        for colno in 0..st.column_count.get() {
            if st.column_name(colno).as_bytes() == name {
                return Some(fetch_value(cx, st, colno, None));
            }
        }
        None
    }

    fn read_column_number(&self, cx: &mut Ctx<'_>, column: i64) -> Option<Value> {
        let st = self.stmt()?;
        if column >= 0 && column < st.column_count.get() {
            return Some(fetch_value(cx, st, column, None));
        }
        None
    }

    fn numeric_name(name: &[u8]) -> Option<i64> {
        match is_numeric_string(name, false) {
            Some(Number::Int(i)) => Some(i),
            _ => None,
        }
    }

    fn prop_read(&self, cx: &mut Ctx<'_>, name: &ZStr) -> Value {
        if name.as_bytes() == b"queryString" {
            if let Some(Value::Object(o)) = &self.stmt_object {
                let r = cx.host().property_read(o, name);
                return cx.check(r).flatten().unwrap_or(Value::Null);
            }
            return Value::Null;
        }
        let v = match Self::numeric_name(name.as_bytes()) {
            Some(n) => self.read_column_number(cx, n),
            None => self.read_column_name(cx, name.as_bytes()),
        };
        v.unwrap_or(Value::Null)
    }

    /// `read_property` (`$row->name`, `$row->{'0'}`, `$row->queryString`).
    pub fn read_property(&self, host: &mut dyn PdoHost, name: &ZStr) -> RtResult<Value> {
        let mut cx = Ctx::new(host);
        let v = self.prop_read(&mut cx, name);
        cx.finish(v)
    }

    /// `read_dimension` (`$row[0]`, `$row['name']`); `None` offset is
    /// `$row[]`.
    pub fn read_dimension(
        &self,
        host: &mut dyn PdoHost,
        offset: Option<&Value>,
    ) -> RtResult<Value> {
        let mut cx = Ctx::new(host);
        let Some(offset) = offset else {
            return Err(RuntimeError::Error("Cannot append to PDORow offset".into()));
        };
        let v = match offset.deref() {
            Value::Int(i) => self.read_column_number(&mut cx, i).unwrap_or(Value::Null),
            other => {
                let r = other.to_zstr(cx.host());
                match cx.check(r) {
                    Some(name) => self.prop_read(&mut cx, &name),
                    None => Value::Null,
                }
            }
        };
        cx.finish(v)
    }

    fn prop_exists(&self, cx: &mut Ctx<'_>, name: &[u8], check_empty: bool) -> bool {
        let v = match Self::numeric_name(name) {
            Some(n) => self.read_column_number(cx, n),
            None => self.read_column_name(cx, name),
        };
        let Some(v) = v else {
            return false;
        };
        if check_empty {
            let r = v.to_bool(cx.host());
            cx.check(r).unwrap_or(false)
        } else {
            !v.is_null()
        }
    }

    /// `has_property` (`isset($row->x)`, `empty($row->x)` with
    /// `check_empty`).
    pub fn has_property(
        &self,
        host: &mut dyn PdoHost,
        name: &ZStr,
        check_empty: bool,
    ) -> RtResult<bool> {
        let mut cx = Ctx::new(host);
        let r = self.prop_exists(&mut cx, name.as_bytes(), check_empty);
        cx.finish(r)
    }

    /// `has_dimension` (`isset($row[0])`, `empty($row['x'])`).
    pub fn has_dimension(
        &self,
        host: &mut dyn PdoHost,
        offset: &Value,
        check_empty: bool,
    ) -> RtResult<bool> {
        let mut cx = Ctx::new(host);
        let r = match offset.deref() {
            Value::Int(column) => {
                let count = self.stmt().map_or(0, |s| s.column_count.get());
                if check_empty {
                    match self.read_column_number(&mut cx, column) {
                        Some(v) => {
                            let r = v.to_bool(cx.host());
                            cx.check(r).unwrap_or(false)
                        }
                        None => false,
                    }
                } else {
                    column >= 0 && column < count
                }
            }
            other => {
                let r = other.to_zstr(cx.host());
                match cx.check(r) {
                    Some(name) => self.prop_exists(&mut cx, name.as_bytes(), check_empty),
                    None => false,
                }
            }
        };
        cx.finish(r)
    }

    /// `write_property`: always an error.
    pub fn write_property_error() -> RuntimeError {
        RuntimeError::Error("Cannot write to PDORow property".into())
    }

    /// `write_dimension`: always an error (`append` for `$row[] = ...`).
    pub fn write_dimension_error(append: bool) -> RuntimeError {
        RuntimeError::Error(if append {
            "Cannot append to PDORow offset".into()
        } else {
            "Cannot write to PDORow offset".into()
        })
    }

    /// `unset_property`: always an error.
    pub fn unset_property_error() -> RuntimeError {
        RuntimeError::Error("Cannot unset PDORow property".into())
    }

    /// `unset_dimension`: always an error.
    pub fn unset_dimension_error() -> RuntimeError {
        RuntimeError::Error("Cannot unset PDORow offset".into())
    }

    /// `get_properties_for(ZEND_PROP_PURPOSE_DEBUG)` (`var_dump`,
    /// `print_r`): the statement's properties plus every column of the
    /// current row. Other purposes use the row's own (empty) properties.
    pub fn debug_properties(&self, host: &mut dyn PdoHost) -> RtResult<ZArray> {
        let mut cx = Ctx::new(host);
        let mut props = match &self.stmt_object {
            Some(Value::Object(o)) => {
                let r = cx.host().object_properties(o, PropertyPurpose::ArrayCast);
                cx.check(r).flatten().unwrap_or_default()
            }
            _ => ZArray::new(),
        };
        if let Some(st) = self.stmt() {
            for i in 0..st.column_count.get() {
                let name = st.column_name(i);
                if name.as_bytes() == b"queryString" {
                    continue;
                }
                let v = fetch_value(&mut cx, st, i, None);
                props.make_mut().insert(ArrayKey::Str(name), v);
            }
        }
        cx.finish(props)
    }
}
