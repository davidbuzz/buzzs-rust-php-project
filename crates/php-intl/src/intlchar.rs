//! `IntlChar` (php-src `ext/intl/uchar/uchar.cpp`).
//!
//! Every method taking a code point accepts either an `int` or a string
//! containing exactly one UTF-8 encoded code point ([`CodePoint`]).  Errors
//! are global intl errors and the method returns `null` (or `false` where
//! noted).

use crate::error::{Error, Result, UErrorCode};
use crate::sys::uchar;
use crate::ustr;

/// A code point argument (`int|string`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodePoint<'a> {
    /// An integer.
    Int(i64),
    /// A string (must be exactly one UTF-8 code point).
    Str(&'a [u8]),
}

/// A result that mirrors the argument type (`IntlChar::tolower()` etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CharOrInt {
    /// The argument was a string: the UTF-8 encoding of the result.
    Str(Vec<u8>),
    /// The argument was an integer.
    Int(i64),
}

fn ill(m: &'static str) -> Error {
    Error::global(UErrorCode::ILLEGAL_ARGUMENT_ERROR, m)
}

/// `convert_cp()`.
pub fn convert_cp(arg: CodePoint<'_>) -> Result<i32> {
    let v = match arg {
        CodePoint::Int(i) => i,
        CodePoint::Str(s) => {
            if s.len() > i32::MAX as usize {
                return Err(ill("Input string is too long."));
            }
            let (c, n) = ustr::u8_next(s);
            let c = i64::from(c);
            if n != s.len() {
                return Err(ill(
                    "Passing a UTF-8 character for codepoint requires a string which is exactly one UTF-8 codepoint long.",
                ));
            }
            c
        }
    };
    if !(0..=0x0010_ffff).contains(&v) {
        return Err(ill("Codepoint out of range"));
    }
    #[allow(clippy::cast_possible_truncation)]
    Ok(v as i32)
}

fn utf8_of(c: i32) -> Vec<u8> {
    u32::try_from(c).ok().and_then(char::from_u32).map_or_else(
        || {
            // surrogate code points: U8_APPEND_UNSAFE emits the 3-byte form
            let c = u32::try_from(c).unwrap_or(0);
            vec![
                u8::try_from(0xe0 | (c >> 12)).unwrap_or(0),
                u8::try_from(0x80 | ((c >> 6) & 0x3f)).unwrap_or(0),
                u8::try_from(0x80 | (c & 0x3f)).unwrap_or(0),
            ]
        },
        |ch| ch.to_string().into_bytes(),
    )
}

/// `IntlChar::chr($codepoint)`.
pub fn chr(arg: CodePoint<'_>) -> Result<Vec<u8>> {
    Ok(utf8_of(convert_cp(arg)?))
}

/// `IntlChar::ord($character)`.
pub fn ord(arg: CodePoint<'_>) -> Result<i64> {
    Ok(i64::from(convert_cp(arg)?))
}

#[allow(clippy::cast_possible_truncation)]
fn i32_of(v: i64) -> i32 {
    v as i32
}

/// `IntlChar::hasBinaryProperty($codepoint, $property)`.
pub fn has_binary_property(arg: CodePoint<'_>, prop: i64) -> Result<bool> {
    let c = convert_cp(arg)?;
    Ok(uchar::has_binary_property(
        u32::try_from(c).unwrap_or(0),
        i32_of(prop),
    ))
}

/// `IntlChar::getIntPropertyValue($codepoint, $property)`.
pub fn get_int_property_value(arg: CodePoint<'_>, prop: i64) -> Result<i64> {
    Ok(i64::from(uchar::int_property_value(
        convert_cp(arg)?,
        i32_of(prop),
    )))
}

/// `IntlChar::getIntPropertyMinValue($property)`.
pub fn get_int_property_min_value(prop: i64) -> i64 {
    i64::from(uchar::int_property_min(i32_of(prop)))
}

/// `IntlChar::getIntPropertyMaxValue($property)`.
pub fn get_int_property_max_value(prop: i64) -> i64 {
    i64::from(uchar::int_property_max(i32_of(prop)))
}

/// `IntlChar::getNumericValue($codepoint)`.
pub fn get_numeric_value(arg: CodePoint<'_>) -> Result<f64> {
    Ok(uchar::numeric_value(convert_cp(arg)?))
}

/// The boolean predicates (`isalpha`, `isUAlphabetic`, …) by method name.
pub fn bool_method(name: &str, arg: CodePoint<'_>) -> Result<bool> {
    let c = convert_cp(arg)?;
    uchar::bool_property(name, c)
        .ok_or_else(|| Error::Thrown(format!("Call to undefined method IntlChar::{name}()")))
}

/// The integer methods (`charDirection`, `charType`,
/// `getCombiningClass`, `charDigitValue`, `getBlockCode`).
pub fn int_method(name: &str, arg: CodePoint<'_>) -> Result<i64> {
    let c = convert_cp(arg)?;
    uchar::int_property(name, c)
        .ok_or_else(|| Error::Thrown(format!("Call to undefined method IntlChar::{name}()")))
}

/// The mapping methods (`charMirror`, `tolower`, `toupper`, `totitle`,
/// `getBidiPairedBracket`), returning a string when given a string.
pub fn char_method(name: &str, arg: CodePoint<'_>) -> Result<CharOrInt> {
    let c = convert_cp(arg)?;
    let r = uchar::char_mapping(name, c)
        .ok_or_else(|| Error::Thrown(format!("Call to undefined method IntlChar::{name}()")))?;
    Ok(match arg {
        CodePoint::Str(_) => CharOrInt::Str(utf8_of(r)),
        CodePoint::Int(_) => CharOrInt::Int(i64::from(r)),
    })
}

/// `IntlChar::foldCase($codepoint, $options)`.
pub fn fold_case(arg: CodePoint<'_>, options: i64) -> Result<CharOrInt> {
    let c = convert_cp(arg)?;
    let r = uchar::fold_case(c, options);
    Ok(match arg {
        CodePoint::Str(_) => CharOrInt::Str(utf8_of(r)),
        CodePoint::Int(_) => CharOrInt::Int(i64::from(r)),
    })
}

/// `IntlChar::digit($codepoint, $base)` (`Err` with "Invalid digit" →
/// `false`).
pub fn digit(arg: CodePoint<'_>, radix: i64) -> Result<i64> {
    let c = convert_cp(arg)?;
    #[allow(clippy::cast_possible_truncation)]
    let r = uchar::digit(c, radix as i8);
    if r < 0 {
        return Err(ill("Invalid digit"));
    }
    Ok(i64::from(r))
}

/// `IntlChar::forDigit($digit, $base)`.
#[allow(clippy::cast_possible_truncation)]
pub fn for_digit(d: i64, radix: i64) -> i64 {
    i64::from(uchar::for_digit(d as i32, radix as i8))
}

/// `IntlChar::charAge($codepoint)`.
pub fn char_age(arg: CodePoint<'_>) -> Result<[i64; 4]> {
    Ok(uchar::char_age(convert_cp(arg)?).map(i64::from))
}

/// `IntlChar::getUnicodeVersion()`.
pub fn get_unicode_version() -> [i64; 4] {
    uchar::unicode_version().map(i64::from)
}

/// `IntlChar::getFC_NFKC_Closure($codepoint)` (`Err` → `false`).
pub fn get_fc_nfkc_closure(arg: CodePoint<'_>) -> Result<String> {
    let c = convert_cp(arg)?;
    let v = uchar::fc_nfkc_closure(c).map_err(|e| Error::global(e, "Failed getting closure"))?;
    ustr::from_u16(&v).map_err(|e| Error::global(e, "Failed converting output to UTF8"))
}

/// `IntlChar::charName($codepoint, $type)`.
pub fn char_name(arg: CodePoint<'_>, choice: i64) -> Result<Vec<u8>> {
    let c = convert_cp(arg)?;
    uchar::char_name(c, i32_of(choice))
        .map_err(|e| Error::global(e, "Failure getting character name"))
}

/// `IntlChar::charFromName($name, $type)` (errors set only the code).
pub fn char_from_name(name: &[u8], choice: i64) -> Result<i64> {
    uchar::char_from_name(i32_of(choice), name)
        .map(i64::from)
        .map_err(Error::code)
}

/// `IntlChar::enumCharTypes($callback)`; `f(start, limit, type)` returns
/// `false` to stop (e.g. when the callback threw).
pub fn enum_char_types(f: &mut dyn FnMut(i64, i64, i64) -> bool) {
    uchar::enum_char_types(&mut |a, b, c| f(i64::from(a), i64::from(b), i64::from(c)));
}

/// `IntlChar::enumCharNames($start, $end, $callback, $type)`.
/// `f(code, choice, name)` returns `false` to stop.  An invalid range
/// argument yields `false` (the intl error is set).
pub fn enum_char_names(
    start: CodePoint<'_>,
    limit: CodePoint<'_>,
    choice: i64,
    f: &mut dyn FnMut(i64, i64, &[u8]) -> bool,
) -> Result<()> {
    let s = convert_cp(start)?;
    let l = convert_cp(limit)?;
    uchar::enum_char_names(s, l, i32_of(choice), &mut |c, n, name| {
        f(i64::from(c), i64::from(n), name)
    })
    .map_err(Error::code)
}

/// `IntlChar::getPropertyName($property, $type)` (`Err` → `false`).
pub fn get_property_name(prop: i64, choice: i64) -> Result<String> {
    uchar::property_name(i32_of(prop), i32_of(choice))
        .ok_or_else(|| ill("Failed to get property name"))
}

/// `IntlChar::getPropertyEnum($alias)`.
pub fn get_property_enum(alias: &[u8]) -> i64 {
    i64::from(uchar::property_enum(alias))
}

/// `IntlChar::getPropertyValueName($property, $value, $type)`.
pub fn get_property_value_name(prop: i64, value: i64, choice: i64) -> Result<String> {
    uchar::property_value_name(i32_of(prop), i32_of(value), i32_of(choice))
        .ok_or_else(|| ill("Failed to get property name"))
}

/// `IntlChar::getPropertyValueEnum($property, $name)`.
pub fn get_property_value_enum(prop: i64, name: &[u8]) -> i64 {
    i64::from(uchar::property_value_enum(i32_of(prop), name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert_eq!(chr(CodePoint::Int(0x1F600)).unwrap(), "😀".as_bytes());
        assert_eq!(ord(CodePoint::Str("é".as_bytes())).unwrap(), 0xe9);
        assert!(ord(CodePoint::Str(b"ab")).is_err());
        assert!(ord(CodePoint::Int(0x11_0000)).is_err());
        assert!(bool_method("isalpha", CodePoint::Str(b"a")).unwrap());
        assert_eq!(
            char_method("toupper", CodePoint::Str(b"a")).unwrap(),
            CharOrInt::Str(b"A".to_vec())
        );
        assert_eq!(
            char_method("toupper", CodePoint::Int(97)).unwrap(),
            CharOrInt::Int(65)
        );
        assert_eq!(
            char_name(CodePoint::Int(65), 0).unwrap(),
            b"LATIN CAPITAL LETTER A"
        );
        assert_eq!(char_from_name(b"LATIN CAPITAL LETTER A", 0).unwrap(), 65);
        assert_eq!(get_property_name(0, 1).unwrap(), "Alphabetic");
        let mut n = 0;
        enum_char_types(&mut |_, _, _| {
            n += 1;
            n < 5
        });
        assert_eq!(n, 5);
        let mut names = Vec::new();
        enum_char_names(
            CodePoint::Int(65),
            CodePoint::Int(68),
            0,
            &mut |c, _, nm| {
                names.push((c, String::from_utf8_lossy(nm).into_owned()));
                true
            },
        )
        .unwrap();
        assert_eq!(names.len(), 3);
    }
}
