//! Small helpers shared by the SPL modules.

use php_runtime::{Obj, RtResult, RuntimeError, ZStr};

/// The fetch type of a dimension read (`BP_VAR_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimKind {
    /// `BP_VAR_R`: a plain read.
    Read,
    /// `BP_VAR_IS`: `isset()`/`??` read (no errors for missing offsets).
    Is,
    /// `BP_VAR_W`: write context (`$o[k][] = v`).
    Write,
    /// `BP_VAR_RW`: read-write (`$o[k] .= v`).
    ReadWrite,
    /// `BP_VAR_UNSET`: `unset($o[k][j])`.
    Unset,
}

/// The `$this` object of an instance method.
pub fn this_obj(this: Option<&Obj>) -> RtResult<&Obj> {
    this.ok_or_else(|| RuntimeError::Error("Non-static method called statically".to_string()))
}

/// `zend_mangle_property_name(class, prop)` for a private property:
/// `"\0Class\0prop"`.
#[must_use]
pub fn private_prop(class: &str, prop: &str) -> ZStr {
    let mut v = Vec::with_capacity(class.len() + prop.len() + 2);
    v.push(0);
    v.extend_from_slice(class.as_bytes());
    v.push(0);
    v.extend_from_slice(prop.as_bytes());
    ZStr::from(v)
}

/// Converts a length or count to a PHP integer.
#[must_use]
pub fn to_long(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Lower-cases an ASCII method/class name.
#[must_use]
pub fn lc(s: &str) -> String {
    s.to_ascii_lowercase()
}
