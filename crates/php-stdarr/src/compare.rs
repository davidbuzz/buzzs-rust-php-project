//! The bucket comparison functions of `ext/standard/array.c`
//! (`php_array_key_compare*`, `php_array_data_compare*`,
//! `php_array_natural*`), `strnatcmp_ex`, and the user-callback comparators
//! (`php_array_user_compare_unstable`, `php_array_user_key_compare_unstable`,
//! `zval_user_compare`).
//!
//! All comparators here are the *unstable* variants; the sort functions add
//! PHP's stable fallback (original order) themselves. They are public so
//! that SPL (`ArrayObject::asort` & co.) can reuse them.

use crate::util::{deprecated, threeway_f};
use crate::{ArrayContext, Callback, SORT_FLAG_CASE, SORT_LOCALE_STRING, SORT_NATURAL};
use crate::{SORT_NUMERIC, SORT_STRING};
use php_runtime::numeric::strtod;
use php_runtime::string::{binary_strcasecmp, binary_strcmp};
use php_runtime::{ops, ArrayKey, RtResult, RuntimeContext, Value, ZStr};

/// The comparison selected by a `SORT_*` flags value, as decoded by
/// `php_get_key_compare_func` / `php_get_data_compare_func`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKind {
    /// `SORT_REGULAR` (and any unknown value): `zend_compare`.
    Regular,
    /// `SORT_NUMERIC`.
    Numeric,
    /// `SORT_STRING`.
    String,
    /// `SORT_STRING | SORT_FLAG_CASE`.
    StringCase,
    /// `SORT_NATURAL`.
    Natural,
    /// `SORT_NATURAL | SORT_FLAG_CASE`.
    NaturalCase,
    /// `SORT_LOCALE_STRING` (compared with `strcoll` in the C locale).
    LocaleString,
}

impl SortKind {
    /// Decodes a `SORT_*` flags argument (`sort_type & ~SORT_FLAG_CASE`
    /// selects the comparison, `SORT_FLAG_CASE` the case-insensitive variant).
    #[must_use]
    pub fn from_flags(flags: i64) -> Self {
        let case = flags & SORT_FLAG_CASE != 0;
        match flags & !SORT_FLAG_CASE {
            SORT_NUMERIC => SortKind::Numeric,
            SORT_STRING if case => SortKind::StringCase,
            SORT_STRING => SortKind::String,
            SORT_NATURAL if case => SortKind::NaturalCase,
            SORT_NATURAL => SortKind::Natural,
            SORT_LOCALE_STRING => SortKind::LocaleString,
            _ => SortKind::Regular,
        }
    }
}

// ---------------------------------------------------------------------------
// strnatcmp
// ---------------------------------------------------------------------------

/// C `isspace` in the "C" locale.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// Byte at `i`, or the terminating NUL past the end (zend strings are
/// NUL-terminated and `strnatcmp_ex` relies on it).
fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

fn compare_right(a: &[u8], ai: &mut usize, b: &[u8], bi: &mut usize) -> i32 {
    let mut bias = 0;
    loop {
        let ad = *ai < a.len() && a[*ai].is_ascii_digit();
        let bd = *bi < b.len() && b[*bi].is_ascii_digit();
        if !ad && !bd {
            return bias;
        } else if !ad {
            return -1;
        } else if !bd {
            return 1;
        } else if a[*ai] < b[*bi] {
            if bias == 0 {
                bias = -1;
            }
        } else if a[*ai] > b[*bi] && bias == 0 {
            bias = 1;
        }
        *ai += 1;
        *bi += 1;
    }
}

fn compare_left(a: &[u8], ai: &mut usize, b: &[u8], bi: &mut usize) -> i32 {
    loop {
        let ad = *ai < a.len() && a[*ai].is_ascii_digit();
        let bd = *bi < b.len() && b[*bi].is_ascii_digit();
        if !ad && !bd {
            return 0;
        } else if !ad {
            return -1;
        } else if !bd {
            return 1;
        } else if a[*ai] < b[*bi] {
            return -1;
        } else if a[*ai] > b[*bi] {
            return 1;
        }
        *ai += 1;
        *bi += 1;
    }
}

/// Port of `strnatcmp_ex` (natural order comparison, `strnatcmp()` /
/// `strnatcasecmp()` / `SORT_NATURAL`).
#[must_use]
pub fn strnatcmp_ex(a: &[u8], b: &[u8], fold_case: bool) -> i32 {
    if a.is_empty() || b.is_empty() {
        return match a.len().cmp(&b.len()) {
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
            std::cmp::Ordering::Less => -1,
        };
    }
    let (mut ai, mut bi) = (0usize, 0usize);
    let mut ca = a[0];
    let mut cb = b[0];
    while ca == b'0' && ai + 1 < a.len() && a[ai + 1].is_ascii_digit() {
        ai += 1;
        ca = a[ai];
    }
    while cb == b'0' && bi + 1 < b.len() && b[bi + 1].is_ascii_digit() {
        bi += 1;
        cb = b[bi];
    }
    loop {
        while is_space(ca) {
            ai += 1;
            ca = at(a, ai);
        }
        while is_space(cb) {
            bi += 1;
            cb = at(b, bi);
        }
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let fractional = ca == b'0' || cb == b'0';
            let result = if fractional {
                compare_left(a, &mut ai, b, &mut bi)
            } else {
                compare_right(a, &mut ai, b, &mut bi)
            };
            if result != 0 {
                return result;
            } else if ai == a.len() && bi == b.len() {
                return 0;
            } else if ai == a.len() {
                return -1;
            } else if bi == b.len() {
                return 1;
            }
            ca = at(a, ai);
            cb = at(b, bi);
        }
        if fold_case {
            ca = ca.to_ascii_uppercase();
            cb = cb.to_ascii_uppercase();
        }
        if ca < cb {
            return -1;
        } else if ca > cb {
            return 1;
        }
        ai += 1;
        bi += 1;
        if ai >= a.len() && bi >= b.len() {
            return 0;
        } else if ai >= a.len() {
            return -1;
        } else if bi >= b.len() {
            return 1;
        }
        ca = at(a, ai);
        cb = at(b, bi);
    }
}

/// `strcoll` in the "C" locale: `strcmp` on the NUL-terminated prefixes.
#[must_use]
pub fn strcoll_c(a: &[u8], b: &[u8]) -> i32 {
    let a = a.split(|&c| c == 0).next().unwrap_or(&[]);
    let b = b.split(|&c| c == 0).next().unwrap_or(&[]);
    for (x, y) in a.iter().zip(b.iter()) {
        if x != y {
            return i32::from(*x) - i32::from(*y);
        }
    }
    match a.len().cmp(&b.len()) {
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Less => -i32::from(b[a.len()]),
        std::cmp::Ordering::Greater => i32::from(a[b.len()]),
    }
}

// ---------------------------------------------------------------------------
// Key comparisons
// ---------------------------------------------------------------------------

fn key_bytes(k: &ArrayKey) -> std::borrow::Cow<'_, [u8]> {
    match k {
        ArrayKey::Int(i) => std::borrow::Cow::Owned(i.to_string().into_bytes()),
        ArrayKey::Str(s) => std::borrow::Cow::Borrowed(s.as_bytes()),
    }
}

/// `php_array_key_compare_unstable_i` (`SORT_REGULAR` on keys).
pub fn key_compare<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    a: &ArrayKey,
    b: &ArrayKey,
) -> RtResult<i32> {
    ops::compare_keys(a, b, ctx)
}

/// `php_array_key_compare_numeric_unstable_i`.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn key_compare_numeric(a: &ArrayKey, b: &ArrayKey) -> i32 {
    if let (ArrayKey::Int(x), ArrayKey::Int(y)) = (a, b) {
        return if x > y { 1 } else { -1 };
    }
    let d = |k: &ArrayKey| match k {
        ArrayKey::Int(i) => *i as f64,
        ArrayKey::Str(s) => strtod(s.as_bytes()).0,
    };
    threeway_f(d(a), d(b))
}

/// `php_array_key_compare_string_unstable_i` (`SORT_STRING` on keys).
#[must_use]
pub fn key_compare_string(a: &ArrayKey, b: &ArrayKey) -> i32 {
    binary_strcmp(&key_bytes(a), &key_bytes(b))
}

/// `php_array_key_compare_string_case_unstable_i`.
#[must_use]
pub fn key_compare_string_case(a: &ArrayKey, b: &ArrayKey) -> i32 {
    binary_strcasecmp(&key_bytes(a), &key_bytes(b))
}

/// `php_array_key_compare_string_natural_general`.
#[must_use]
pub fn key_compare_natural(a: &ArrayKey, b: &ArrayKey, fold_case: bool) -> i32 {
    strnatcmp_ex(&key_bytes(a), &key_bytes(b), fold_case)
}

/// `php_array_key_compare_string_locale_unstable_i`.
#[must_use]
pub fn key_compare_locale(a: &ArrayKey, b: &ArrayKey) -> i32 {
    strcoll_c(&key_bytes(a), &key_bytes(b))
}

/// Compares two keys with the comparison selected by `kind`. With
/// `reverse`, the result is that of PHP's `php_array_reverse_key_compare*`
/// functions (negated, or arguments swapped for the natural variants).
pub fn compare_keys_by<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    kind: SortKind,
    reverse: bool,
    a: &ArrayKey,
    b: &ArrayKey,
) -> RtResult<i32> {
    let r = match kind {
        SortKind::Regular => key_compare(ctx, a, b)?,
        SortKind::Numeric => key_compare_numeric(a, b),
        SortKind::String => key_compare_string(a, b),
        SortKind::StringCase => key_compare_string_case(a, b),
        SortKind::LocaleString => key_compare_locale(a, b),
        SortKind::Natural | SortKind::NaturalCase => {
            let fold = kind == SortKind::NaturalCase;
            return Ok(if reverse {
                key_compare_natural(b, a, fold)
            } else {
                key_compare_natural(a, b, fold)
            });
        }
    };
    Ok(if reverse { r.wrapping_neg() } else { r })
}

// ---------------------------------------------------------------------------
// Data comparisons
// ---------------------------------------------------------------------------

/// `php_array_data_compare_unstable_i` (`SORT_REGULAR`): `zend_compare`
/// plus the enum special case used to group enum cases in `array_unique`
/// (uncomparable enums are ordered by identity, and sorted after other
/// values).
pub fn data_compare<C: ArrayContext + ?Sized>(ctx: &mut C, a: &Value, b: &Value) -> RtResult<i32> {
    let result = ops::compare(a, b, ctx)?;
    if result == 1 {
        let rhs = crate::util::deref(b);
        if let Value::Object(ro) = &rhs {
            if ctx.is_enum(ro) {
                let lhs = crate::util::deref(a);
                if let Value::Object(lo) = &lhs {
                    if ctx.is_enum(lo) {
                        let (l, r) = (lo.addr(), ro.addr());
                        return Ok(match l.cmp(&r) {
                            std::cmp::Ordering::Equal => 0,
                            std::cmp::Ordering::Less => -1,
                            std::cmp::Ordering::Greater => 1,
                        });
                    }
                }
                return Ok(-1);
            }
        }
    }
    Ok(result)
}

/// `numeric_compare_function` (`SORT_NUMERIC`): compares `(float)` casts.
pub fn data_compare_numeric<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    a: &Value,
    b: &Value,
) -> RtResult<i32> {
    let x = a.to_double(ctx)?;
    let y = b.to_double(ctx)?;
    Ok(threeway_f(x, y))
}

fn str_pair<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    a: &Value,
    b: &Value,
) -> RtResult<(ZStr, ZStr)> {
    let x = a.to_zstr(ctx)?;
    let y = b.to_zstr(ctx)?;
    Ok((x, y))
}

/// `string_compare_function` (`SORT_STRING`).
pub fn data_compare_string<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    a: &Value,
    b: &Value,
) -> RtResult<i32> {
    if let (Value::Str(x), Value::Str(y)) = (a, b) {
        return Ok(binary_strcmp(x.as_bytes(), y.as_bytes()));
    }
    let (x, y) = str_pair(ctx, a, b)?;
    Ok(binary_strcmp(x.as_bytes(), y.as_bytes()))
}

/// `string_case_compare_function` (`SORT_STRING | SORT_FLAG_CASE`).
pub fn data_compare_string_case<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    a: &Value,
    b: &Value,
) -> RtResult<i32> {
    let (x, y) = str_pair(ctx, a, b)?;
    Ok(binary_strcasecmp(x.as_bytes(), y.as_bytes()))
}

/// `php_array_natural_general_compare` (`SORT_NATURAL`).
pub fn data_compare_natural<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    a: &Value,
    b: &Value,
    fold_case: bool,
) -> RtResult<i32> {
    let (x, y) = str_pair(ctx, a, b)?;
    Ok(strnatcmp_ex(x.as_bytes(), y.as_bytes(), fold_case))
}

/// `string_locale_compare_function` (`SORT_LOCALE_STRING`, "C" locale).
pub fn data_compare_locale<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    a: &Value,
    b: &Value,
) -> RtResult<i32> {
    let (x, y) = str_pair(ctx, a, b)?;
    Ok(strcoll_c(x.as_bytes(), y.as_bytes()))
}

/// Compares two values with the comparison selected by `kind`; with
/// `reverse` the result is negated (`php_array_reverse_data_compare*`).
pub fn compare_data_by<C: ArrayContext + ?Sized>(
    ctx: &mut C,
    kind: SortKind,
    reverse: bool,
    a: &Value,
    b: &Value,
) -> RtResult<i32> {
    let r = match kind {
        SortKind::Regular => data_compare(ctx, a, b)?,
        SortKind::Numeric => data_compare_numeric(ctx, a, b)?,
        SortKind::String => data_compare_string(ctx, a, b)?,
        SortKind::StringCase => data_compare_string_case(ctx, a, b)?,
        SortKind::Natural => data_compare_natural(ctx, a, b, false)?,
        SortKind::NaturalCase => data_compare_natural(ctx, a, b, true)?,
        SortKind::LocaleString => data_compare_locale(ctx, a, b)?,
    };
    Ok(if reverse { r.wrapping_neg() } else { r })
}

// ---------------------------------------------------------------------------
// User comparators
// ---------------------------------------------------------------------------

/// State of a user comparison callback for the duration of one array
/// function call (`ARRAYG(compare_deprecation_thrown)` is per call).
pub struct UserCompare<'a, 'b, C: ?Sized> {
    /// The PHP function name, for the deprecation prefix.
    pub func: &'a str,
    /// The callback.
    pub callback: &'a mut Callback<'b, C>,
    /// Whether the "Returning bool from comparison function" deprecation was
    /// already emitted during this call.
    pub warned: bool,
}

impl<C: ?Sized> std::fmt::Debug for UserCompare<'_, '_, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserCompare")
            .field("func", &self.func)
            .field("warned", &self.warned)
            .finish_non_exhaustive()
    }
}

impl<'a, 'b, C: RuntimeContext + ?Sized> UserCompare<'a, 'b, C> {
    /// Creates the comparator state.
    pub fn new(func: &'a str, callback: &'a mut Callback<'b, C>) -> Self {
        UserCompare {
            func,
            callback,
            warned: false,
        }
    }

    /// `php_array_user_compare_unstable` / `php_array_user_key_compare_unstable`:
    /// calls the callback with `(a, b)`; a `bool` result is deprecated, and
    /// `false` is retried with swapped operands.
    pub fn compare(&mut self, ctx: &mut C, a: &Value, b: &Value) -> RtResult<i32> {
        let ret = (self.callback)(ctx, &[a.clone(), b.clone()])?;
        let ret = crate::util::deref(&ret);
        if let Value::Bool(bv) = ret {
            if !self.warned {
                self.warned = true;
                deprecated(
                    ctx,
                    self.func,
                    "Returning bool from comparison function is deprecated, return an integer less than, equal to, or greater than zero",
                )?;
            }
            if !bv {
                let ret2 = (self.callback)(ctx, &[b.clone(), a.clone()])?;
                let n = ret2.to_long(ctx)?;
                return Ok(-crate::util::normalize(n));
            }
        }
        let n = ret.to_long(ctx)?;
        Ok(crate::util::normalize(n))
    }

    /// `zval_user_compare` (used by `array_udiff_assoc` and
    /// `array_uintersect_assoc`): no bool handling.
    pub fn compare_plain(&mut self, ctx: &mut C, a: &Value, b: &Value) -> RtResult<i32> {
        let ret = (self.callback)(ctx, &[a.clone(), b.clone()])?;
        let n = ret.to_long(ctx)?;
        Ok(crate::util::normalize(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natcmp_basics() {
        assert_eq!(strnatcmp_ex(b"img12", b"img10", false), 1);
        assert_eq!(strnatcmp_ex(b"img2", b"img10", false), -1);
        assert_eq!(strnatcmp_ex(b"IMG2", b"img2", true), 0);
        assert_eq!(strnatcmp_ex(b"IMG2", b"img2", false), -1);
        assert_eq!(strnatcmp_ex(b"", b"a", false), -1);
        assert_eq!(strnatcmp_ex(b"0.5", b"0.25", false), -1);
        assert_eq!(strnatcmp_ex(b"a  1", b"a 1", false), 0);
        assert_eq!(strnatcmp_ex(b"007", b"7", false), 0);
    }

    #[test]
    fn sort_kind_flags() {
        assert_eq!(SortKind::from_flags(0), SortKind::Regular);
        assert_eq!(SortKind::from_flags(2 | 8), SortKind::StringCase);
        assert_eq!(SortKind::from_flags(6 | 8), SortKind::NaturalCase);
        assert_eq!(SortKind::from_flags(8), SortKind::Regular);
        assert_eq!(SortKind::from_flags(3), SortKind::Regular);
    }

    #[test]
    fn strcoll_stops_at_nul() {
        assert_eq!(strcoll_c(b"a\0b", b"a\0c"), 0);
        assert!(strcoll_c(b"a", b"ab") < 0);
    }
}
