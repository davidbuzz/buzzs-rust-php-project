// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `Collator` / `collator_*` (php-src `ext/intl/collator`).

use std::cmp::Ordering;

use crate::common;
use crate::error::{Error, Result, Scope, UErrorCode};
use crate::sys::uchar::is_space;
use crate::sys::ucol::Collator as Ucol;
use crate::ustr;

/// `Collator::SORT_*`.
pub mod sort_flags {
    /// `SORT_REGULAR`
    pub const REGULAR: i64 = 0;
    /// `SORT_STRING`
    pub const STRING: i64 = 1;
    /// `SORT_NUMERIC`
    pub const NUMERIC: i64 = 2;
}

/// A PHP `Collator` object.
#[derive(Debug)]
pub struct Collator {
    coll: Ucol,
}

/// A value being sorted by `Collator::sort()`/`asort()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortValue<'a> {
    /// `null`
    Null,
    /// `bool`
    Bool(bool),
    /// `int`
    Int(i64),
    /// `float`
    Float(f64),
    /// `string`
    Str(&'a [u8]),
    /// An object; `as_string` is its `__toString()` result if it has one.
    Object {
        /// `__toString()`.
        as_string: Option<&'a [u8]>,
    },
    /// An array (compared as greater than scalars).
    Array,
}

/// A converted sort operand.
#[derive(Debug, Clone, PartialEq)]
enum Op {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// UTF-16 string (and its UTF-8 original).
    Str(Vec<u16>, Vec<u8>),
    Other,
    /// An object whose `__toString()` result is not valid UTF-8: comparing
    /// it fails (lazily, as php-src converts objects in the comparator).
    BadObject(UErrorCode),
}

/// `collator_is_numeric()` (no errors allowed) on a UTF-16 string.
pub fn collator_is_numeric(s: &[u16]) -> Option<common::Numeric> {
    if s.is_empty() {
        return None;
    }
    let at = |i: usize| -> u16 { s.get(i).copied().unwrap_or(0) };
    let skip_ws = |mut i: usize| {
        while i < s.len() && is_space(u32::from(s[i])) {
            i += 1;
        }
        i
    };
    // collator_u_strtol(base 10)
    let mut i = skip_ws(0);
    let mut neg = false;
    if at(i) == 0x2d {
        neg = true;
        i += 1;
    } else if at(i) == 0x2b {
        i += 1;
    }
    let mut acc: i128 = 0;
    let mut any = 0i32;
    let limit: i128 = if neg {
        i128::from(i64::MAX) + 1
    } else {
        i128::from(i64::MAX)
    };
    let start_digits = i;
    while i < s.len() && (0x30..=0x39).contains(&s[i]) {
        if any < 0 || acc * 10 + i128::from(s[i] - 0x30) > limit {
            any = -1;
        } else {
            any = 1;
            acc = acc * 10 + i128::from(s[i] - 0x30);
        }
        i += 1;
    }
    let erange = any < 0;
    if !erange {
        let end_long = if any != 0 { i } else { 0 };
        let _ = start_digits;
        let e = skip_ws(end_long);
        if any != 0 && e == s.len() {
            #[allow(clippy::cast_possible_truncation)]
            let v = if neg { (-acc) as i64 } else { acc as i64 };
            return Some(common::Numeric::Long(v));
        }
        if any == 0 && at(0) != 0 && at(0) != 0x2e && at(0) != 0x2d {
            return None;
        }
    }
    // collator_u_strtod
    let mut u = skip_ws(0);
    let nstart = u;
    if at(u) == 0x2d || at(u) == 0x2b {
        u += 1;
    }
    let mut anyd = false;
    while (0x30..=0x39).contains(&at(u)) {
        anyd = true;
        u += 1;
    }
    if at(u) == 0x2e {
        u += 1;
        while (0x30..=0x39).contains(&at(u)) {
            anyd = true;
            u += 1;
        }
    }
    if (at(u) == 0x65 || at(u) == 0x45) && anyd {
        let e = u;
        u += 1;
        if at(u) == 0x2d || at(u) == 0x2b {
            u += 1;
        }
        let mut any_exp = false;
        while (0x30..=0x39).contains(&at(u)) {
            any_exp = true;
            u += 1;
        }
        if !any_exp {
            u = e;
        }
    }
    if !anyd {
        return None;
    }
    let text: String = s[nstart..u]
        .iter()
        .map(|&c| char::from(u8::try_from(c).unwrap_or(b'?')))
        .collect();
    let v = common::php_strtod(text.as_bytes());
    if skip_ws(u) == s.len() {
        if !v.is_finite() {
            return None;
        }
        return Some(common::Numeric::Double(v));
    }
    None
}

impl Collator {
    /// `new Collator($locale)` / `collator_create()`.  On success the
    /// (possibly warning) status is returned too: php leaves it in the
    /// global error code (e.g. `U_USING_DEFAULT_WARNING`).
    pub fn new(locale: &str) -> Result<(Self, UErrorCode)> {
        common::check_locale_len(locale).map_err(|e| crate::timezone::rescope(e, Scope::Global))?;
        let l = common::locale_or_default(locale);
        let (coll, st) =
            Ucol::open(&l).map_err(|c| Error::intl(c, "unable to open ICU collator"))?;
        Ok((Self { coll }, st))
    }

    /// Clone (`clone $coll`).
    pub fn try_clone(&self) -> Result<Self> {
        let coll = self
            .coll
            .try_clone()
            .map_err(|_| Error::Thrown("Failed to clone Collator".into()))?;
        Ok(Self { coll })
    }

    /// `compare($string1, $string2)`.
    pub fn compare(&self, a: &[u8], b: &[u8]) -> Result<i64> {
        let ua = ustr::to_u16(a)
            .map_err(|c| Error::intl(c, "Error converting first argument to UTF-16"))?;
        let ub = ustr::to_u16(b)
            .map_err(|c| Error::intl(c, "Error converting second argument to UTF-16"))?;
        Ok(i64::from(self.coll.strcoll(&ua, &ub)))
    }

    /// `getAttribute($attribute)`.
    pub fn get_attribute(&self, attr: i64) -> Result<i64> {
        self.coll
            .attribute(i32_of(attr))
            .map(i64::from)
            .map_err(|c| Error::intl(c, "Error getting attribute value"))
    }

    /// `setAttribute($attribute, $value)`.
    pub fn set_attribute(&mut self, attr: i64, value: i64) -> Result<()> {
        self.coll
            .set_attribute(i32_of(attr), i32_of(value))
            .map_err(|c| Error::intl(c, "Error setting attribute value"))
    }

    /// `getStrength()`.
    pub fn strength(&self) -> i64 {
        i64::from(self.coll.strength())
    }

    /// `setStrength($strength)`.
    pub fn set_strength(&mut self, s: i64) {
        self.coll.set_strength(i32_of(s));
    }

    /// `getRules()`: `ucol_getRules()` in UTF-8.
    pub fn rules(&self) -> Result<String> {
        let rules = self.coll.rules();
        ustr::from_u16(&rules)
            .map_err(|c| Error::intl(c, "Error converting collation rules to UTF-8"))
    }

    /// `getLocale($type)`.
    pub fn locale(&self, ty: i64) -> Result<String> {
        self.coll
            .locale_by_type(i32_of(ty))
            .map_err(|c| Error::intl(c, "Error getting locale by type"))
    }

    /// `getSortKey($string)` (`Ok(None)` → `false` without error).
    pub fn sort_key(&self, s: &[u8]) -> Result<Option<Vec<u8>>> {
        let u = ustr::to_u16(s)
            .map_err(|c| Error::intl(c, "Error converting first argument to UTF-16"))?;
        Ok(self.coll.sort_key(&u))
    }

    /// Converts an element up front (`collator_convert_hash_from_utf8_to_utf16`
    /// for strings; objects are converted as the comparator would).
    fn convert(v: &SortValue<'_>, flags: i64) -> Result<Op> {
        Ok(match v {
            SortValue::Null => Op::Null,
            SortValue::Bool(b) => Op::Bool(*b),
            SortValue::Int(i) => Op::Int(*i),
            SortValue::Float(f) => Op::Float(*f),
            SortValue::Str(s) => Op::Str(
                ustr::to_u16(s)
                    .map_err(|c| Error::intl(c, "Error converting hash from UTF-8 to UTF-16"))?,
                s.to_vec(),
            ),
            // numeric_compare_function(): zval_get_double(object) is 1.0
            SortValue::Object { .. } if flags == sort_flags::NUMERIC => Op::Float(1.0),
            SortValue::Object { as_string: Some(s) } => match ustr::to_u16(s) {
                Ok(u) => Op::Str(u, s.to_vec()),
                Err(c) => Op::BadObject(c),
            },
            SortValue::Object { as_string: None } | SortValue::Array => Op::Other,
        })
    }

    fn icu_cmp(&self, a: &Op, b: &Op) -> Ordering {
        let s = |o: &Op| -> Vec<u16> {
            match o {
                Op::Str(u, _) => u.clone(),
                Op::Null | Op::Bool(false) | Op::Other | Op::BadObject(_) => Vec::new(),
                Op::Bool(true) => vec![u16::from(b'1')],
                Op::Int(i) => ustr::str_u16(&i.to_string()),
                Op::Float(f) => ustr::str_u16(&common::php_float_to_string(*f)),
            }
        };
        self.coll.strcoll(&s(a), &s(b)).cmp(&0)
    }

    fn numeric_of(o: &Op) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        match o {
            Op::Null | Op::Bool(false) | Op::Other | Op::BadObject(_) => 0.0,
            Op::Bool(true) => 1.0,
            Op::Int(i) => *i as f64,
            Op::Float(f) => *f,
            Op::Str(u, _) => match collator_is_numeric(u) {
                Some(common::Numeric::Long(l)) => l as f64,
                Some(common::Numeric::Double(d)) => d,
                None => 0.0,
            },
        }
    }

    fn regular_cmp(&self, a: &Op, b: &Op) -> Ordering {
        if let (Op::Str(ua, _), Op::Str(ub, _)) = (a, b) {
            let na = collator_is_numeric(ua);
            let nb = if na.is_some() {
                collator_is_numeric(ub)
            } else {
                None
            };
            if na.is_none() || nb.is_none() {
                return self.icu_cmp(a, b);
            }
        }
        let norm = |o: &Op| -> Op {
            match o {
                Op::Str(u, raw) => match collator_is_numeric(u) {
                    Some(common::Numeric::Long(l)) => Op::Int(l),
                    Some(common::Numeric::Double(d)) => Op::Float(d),
                    None => Op::Str(u.clone(), raw.clone()),
                },
                other => other.clone(),
            }
        };
        php_compare(&norm(a), &norm(b))
    }

    /// `sort()`/`asort()`: the sorted order of `values` as a permutation
    /// of indexes (the VM rebuilds the array, renumbering for `sort()`).
    ///
    /// An object whose string form is not valid UTF-8 makes the first
    /// comparison involving it fail; every later comparison then returns
    /// 0 and the sort reports `"Error converting string from UTF-8 to
    /// UTF-16"` (`SORT_STRING`) or `"Error converting object string from
    /// UTF-8 to UTF-16"` (`SORT_REGULAR`); the array stays unchanged.
    pub fn sort_order(&self, values: &[SortValue<'_>], flags: i64) -> Result<Vec<usize>> {
        let ops: Vec<Op> = values
            .iter()
            .map(|v| Self::convert(v, flags))
            .collect::<Result<_>>()?;
        let mut idx: Vec<usize> = (0..ops.len()).collect();
        let ord = |o: Ordering| o as i32;
        let mut failure: Option<UErrorCode> = None;
        let mut guard = |a: &Op, b: &Op| -> bool {
            if failure.is_some() {
                return false;
            }
            for o in [a, b] {
                if let Op::BadObject(c) = o {
                    failure = Some(*c);
                    return false;
                }
            }
            true
        };
        match flags {
            sort_flags::NUMERIC => common::zend_sort(&mut idx, &mut |&x, &y| {
                ord(Self::numeric_of(&ops[x])
                    .partial_cmp(&Self::numeric_of(&ops[y]))
                    .unwrap_or(Ordering::Equal))
            }),
            sort_flags::STRING => common::zend_sort(&mut idx, &mut |&x, &y| {
                if guard(&ops[x], &ops[y]) {
                    ord(self.icu_cmp(&ops[x], &ops[y]))
                } else {
                    0
                }
            }),
            _ => common::zend_sort(&mut idx, &mut |&x, &y| {
                if guard(&ops[x], &ops[y]) {
                    ord(self.regular_cmp(&ops[x], &ops[y]))
                } else {
                    0
                }
            }),
        }
        if let Some(c) = failure {
            return Err(Error::intl(
                c,
                if flags == sort_flags::STRING {
                    "Error converting string from UTF-8 to UTF-16"
                } else {
                    "Error converting object string from UTF-8 to UTF-16"
                },
            ));
        }
        Ok(idx)
    }

    /// `sortWithSortKeys()`: non-string values sort as empty strings;
    /// the result is renumbered.  Returns the permutation.
    pub fn sort_with_sort_keys_order(&self, values: &[SortValue<'_>]) -> Result<Vec<usize>> {
        let mut keys = Vec::with_capacity(values.len());
        for v in values {
            let u = match v {
                SortValue::Str(s) => {
                    ustr::to_u16(s).map_err(|c| Error::intl(c, "Sort with sort keys failed"))?
                }
                _ => Vec::new(),
            };
            let mut k = self.coll.sort_key(&u).unwrap_or_default();
            // keys are compared with strcmp(): stop at the first NUL
            if let Some(p) = k.iter().position(|&b| b == 0) {
                k.truncate(p);
            }
            keys.push(k);
        }
        let mut idx: Vec<usize> = (0..values.len()).collect();
        common::zend_sort(&mut idx, &mut |&a, &b| keys[a].cmp(&keys[b]) as i32);
        Ok(idx)
    }
}

fn i32_of(v: i64) -> i32 {
    #[allow(clippy::cast_possible_truncation)]
    {
        v as i32
    }
}

/// PHP 8's `zend_compare()` for the scalar operands the collator produces.
#[allow(clippy::many_single_char_names, clippy::match_same_arms)]
fn php_compare(a: &Op, b: &Op) -> Ordering {
    let truthy = |o: &Op| match o {
        Op::Null | Op::Bool(false) => false,
        Op::Bool(true) | Op::Other | Op::BadObject(_) => true,
        Op::Int(i) => *i != 0,
        Op::Float(f) => *f != 0.0,
        Op::Str(_, raw) => !(raw.is_empty() || raw.as_slice() == b"0"),
    };
    let f = |x: f64, y: f64| x.partial_cmp(&y).unwrap_or(Ordering::Equal);
    match (a, b) {
        (Op::Int(x), Op::Int(y)) => x.cmp(y),
        #[allow(clippy::cast_precision_loss)]
        (Op::Int(x), Op::Float(y)) => f(*x as f64, *y),
        #[allow(clippy::cast_precision_loss)]
        (Op::Float(x), Op::Int(y)) => f(*x, *y as f64),
        (Op::Float(x), Op::Float(y)) => f(*x, *y),
        (Op::Null, Op::Null) => Ordering::Equal,
        (Op::Null, Op::Str(_, s)) => {
            if s.is_empty() {
                Ordering::Equal
            } else {
                Ordering::Less
            }
        }
        (Op::Str(_, s), Op::Null) => {
            if s.is_empty() {
                Ordering::Equal
            } else {
                Ordering::Greater
            }
        }
        (Op::Null | Op::Bool(false), other) => {
            if truthy(other) {
                Ordering::Less
            } else {
                Ordering::Equal
            }
        }
        (Op::Bool(true), other) => {
            if truthy(other) {
                Ordering::Equal
            } else {
                Ordering::Greater
            }
        }
        (other, Op::Null | Op::Bool(false)) => {
            if truthy(other) {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }
        (other, Op::Bool(true)) => {
            if truthy(other) {
                Ordering::Equal
            } else {
                Ordering::Less
            }
        }
        (Op::Int(_) | Op::Float(_), Op::Str(_, s)) => {
            let n = num_str(a);
            php_num_vs_string(&n, s)
        }
        (Op::Str(_, s), Op::Int(_) | Op::Float(_)) => php_num_vs_string(&num_str(b), s).reverse(),
        (Op::Str(_, x), Op::Str(_, y)) => x.cmp(y),
        (Op::Other | Op::BadObject(_), Op::Other | Op::BadObject(_)) => Ordering::Equal,
        (Op::Other | Op::BadObject(_), _) => Ordering::Greater,
        (_, Op::Other | Op::BadObject(_)) => Ordering::Less,
    }
}

fn num_str(o: &Op) -> (Option<f64>, Vec<u8>) {
    #[allow(clippy::cast_precision_loss)]
    match o {
        Op::Int(i) => (Some(*i as f64), i.to_string().into_bytes()),
        Op::Float(f) => (Some(*f), common::php_float_to_string(*f).into_bytes()),
        _ => (None, Vec::new()),
    }
}

/// `compare_longs_to_string()` / `compare_doubles_to_string()`.
fn php_num_vs_string(n: &(Option<f64>, Vec<u8>), s: &[u8]) -> Ordering {
    match common::numeric_string(s) {
        #[allow(clippy::cast_precision_loss)]
        Some(common::Numeric::Long(l)) => {
            n.0.unwrap_or(0.0)
                .partial_cmp(&(l as f64))
                .unwrap_or(Ordering::Equal)
        }
        Some(common::Numeric::Double(d)) => {
            n.0.unwrap_or(0.0)
                .partial_cmp(&d)
                .unwrap_or(Ordering::Equal)
        }
        None => n.1.as_slice().cmp(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_and_sort() {
        let (c, _) = Collator::new("en_US").unwrap();
        assert_eq!(c.compare("côte".as_bytes(), b"cote").unwrap(), 1);
        assert_eq!(c.compare(b"a", b"B").unwrap(), -1);
        let vals = [
            SortValue::Str(b"b"),
            SortValue::Str(b"A"),
            SortValue::Str(b"a"),
            SortValue::Str(b"10"),
            SortValue::Str(b"9"),
        ];
        let o = c.sort_order(&vals, sort_flags::REGULAR).unwrap();
        assert_eq!(o, vec![4, 3, 2, 1, 0]);
        let o = c.sort_order(&vals, sort_flags::STRING).unwrap();
        assert_eq!(o, vec![3, 4, 2, 1, 0]);
    }

    #[test]
    fn numeric_detection() {
        let n = |s: &str| collator_is_numeric(&ustr::str_u16(s));
        assert_eq!(n("12"), Some(common::Numeric::Long(12)));
        assert_eq!(n(" 1.5 "), Some(common::Numeric::Double(1.5)));
        assert_eq!(n("abc"), None);
        assert_eq!(n("1e3"), Some(common::Numeric::Double(1000.0)));
        assert_eq!(n("0x1A"), None);
    }
}
