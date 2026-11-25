// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `NumberFormatter` / `numfmt_*` (php-src `ext/intl/formatter`).

use crate::common;
use crate::error::{Error, Result, StatusExt, UErrorCode};
use crate::sys::unum::NumberFormat;
use crate::ustr;

/// `UNumberFormatStyle` values used by php-src's constructor switch.
pub mod style {
    /// `UNUM_PATTERN_DECIMAL` (= `NumberFormatter::PATTERN_DECIMAL`, `IGNORE`)
    pub const PATTERN_DECIMAL: i64 = 0;
    /// `UNUM_DECIMAL`
    pub const DECIMAL: i64 = 1;
    /// `UNUM_CURRENCY`
    pub const CURRENCY: i64 = 2;
    /// `UNUM_PERCENT`
    pub const PERCENT: i64 = 3;
    /// `UNUM_SCIENTIFIC`
    pub const SCIENTIFIC: i64 = 4;
    /// `UNUM_SPELLOUT`
    pub const SPELLOUT: i64 = 5;
    /// `UNUM_ORDINAL`
    pub const ORDINAL: i64 = 6;
    /// `UNUM_DURATION`
    pub const DURATION: i64 = 7;
    /// `UNUM_NUMBERING_SYSTEM`
    pub const NUMBERING_SYSTEM: i64 = 8;
    /// `UNUM_PATTERN_RULEBASED`
    pub const PATTERN_RULEBASED: i64 = 9;
    /// `UNUM_CURRENCY_ISO`
    pub const CURRENCY_ISO: i64 = 10;
    /// `UNUM_CURRENCY_PLURAL`
    pub const CURRENCY_PLURAL: i64 = 11;
    /// `UNUM_CURRENCY_ACCOUNTING`
    pub const CURRENCY_ACCOUNTING: i64 = 12;
    /// `UNUM_CASH_CURRENCY`
    pub const CASH_CURRENCY: i64 = 13;
    /// `UNUM_DECIMAL_COMPACT_SHORT`
    pub const DECIMAL_COMPACT_SHORT: i64 = 14;
    /// `UNUM_DECIMAL_COMPACT_LONG`
    pub const DECIMAL_COMPACT_LONG: i64 = 15;
    /// `UNUM_CURRENCY_STANDARD`
    pub const CURRENCY_STANDARD: i64 = 16;
    /// `UNUM_FORMAT_STYLE_COUNT`
    pub const COUNT: i64 = 17;
}

/// `NumberFormatter::TYPE_*` (php's `FORMAT_TYPE_*`).
pub mod format_type {
    /// `TYPE_DEFAULT`
    pub const DEFAULT: i64 = 0;
    /// `TYPE_INT32`
    pub const INT32: i64 = 1;
    /// `TYPE_INT64`
    pub const INT64: i64 = 2;
    /// `TYPE_DOUBLE`
    pub const DOUBLE: i64 = 3;
    /// `TYPE_CURRENCY`
    pub const CURRENCY: i64 = 4;
}

/// `UNUM_FORMAT_SYMBOL_COUNT`.
pub const SYMBOL_COUNT: i64 = 29;
/// `UNUM_ROUNDING_INCREMENT`.
const ATTR_ROUNDING_INCREMENT: i64 = 12;

/// A PHP `int|float` argument.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Number {
    /// `int`
    Int(i64),
    /// `float`
    Float(f64),
}

impl Number {
    /// `zval_get_long()`.
    pub fn to_long(self) -> i64 {
        match self {
            Self::Int(i) => i,
            Self::Float(f) => dval_to_lval(f),
        }
    }

    /// `zval_get_double()`.
    #[allow(clippy::cast_precision_loss)]
    pub fn to_double(self) -> f64 {
        match self {
            Self::Int(i) => i as f64,
            Self::Float(f) => f,
        }
    }
}

/// PHP's `zend_dval_to_lval()` (modular conversion for out-of-range
/// values, 0 for non-finite ones).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub fn dval_to_lval(d: f64) -> i64 {
    if !d.is_finite() {
        return 0;
    }
    if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&d) {
        return d as i64;
    }
    // zend_dval_to_lval_slow: modular arithmetic over 2^64.
    let two_pow_64 = 18_446_744_073_709_551_616.0_f64;
    let mut dmod = d % two_pow_64;
    if dmod < 0.0 {
        // we're going to make this number positive; call ourselves recursively
        // if the result is -zend_long_MIN, which can't be represented
        dmod += two_pow_64;
        if dmod >= two_pow_64 {
            return 0;
        }
    }
    if dmod > 9_223_372_036_854_775_807.0 {
        dmod -= two_pow_64;
    }
    dmod as i64
}

/// C's `(int64_t)double` on x86-64 (`cvttsd2si`): out-of-range and NaN give
/// `INT64_MIN`.
#[allow(clippy::cast_possible_truncation)]
pub fn c_double_to_i64(d: f64) -> i64 {
    if d.is_nan() || !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&d) {
        i64::MIN
    } else {
        d as i64
    }
}

/// C's `(int32_t)` truncation of a 64-bit integer.
#[allow(clippy::cast_possible_truncation)]
pub fn wrap_i32(v: i64) -> i32 {
    v as i32
}

/// The result of `NumberFormatter::parse()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Parsed {
    /// `int`
    Int(i64),
    /// `float`
    Float(f64),
}

/// A PHP `NumberFormatter` object's ICU state.
#[derive(Debug)]
pub struct NumberFormatter {
    fmt: NumberFormat,
    /// Bumped by every successful setter (used by
    /// `IntlRelativeDateTimeFormatter` to detect changes).
    pub configuration_version: u64,
}

impl NumberFormatter {
    /// `new NumberFormatter($locale, $style, $pattern)` /
    /// `numfmt_create()`.  Errors: the locale-length intl error,
    /// `ValueError` for an unknown locale, and
    /// `"number formatter creation failed"` (the constructor always
    /// throws it as an `IntlException`; the factory returns `null`).
    pub fn new(locale: &str, style: i64, pattern: Option<&[u8]>) -> Result<Self> {
        common::check_locale_len(locale)?;
        let upattern = match pattern {
            Some(p) if !p.is_empty() => {
                Some(ustr::to_u16(p).or_intl("error converting pattern to UTF-16")?)
            }
            _ => None,
        };
        let locale = common::locale_or_default(locale);
        common::validate_locale_arg(&locale, 1)?;
        let final_locale = common::canonicalize_locale_string(&locale).unwrap_or(locale);

        let fail = |c: UErrorCode| Error::intl(c, "number formatter creation failed");
        let mut st = style;
        if style == style::NUMBERING_SYSTEM
            && common::locale_keyword_value(&final_locale, "numbers").is_some()
        {
            st = style::DECIMAL;
        }
        if !(0..style::COUNT).contains(&st) {
            return Err(fail(UErrorCode::ILLEGAL_ARGUMENT_ERROR));
        }
        let empty: [u16; 0] = [];
        let pat: Option<&[u16]> = match st {
            style::PATTERN_DECIMAL | style::PATTERN_RULEBASED => {
                Some(upattern.as_deref().unwrap_or(&empty))
            }
            _ => None,
        };
        #[allow(clippy::cast_possible_truncation)]
        let fmt = NumberFormat::open(st as i32, pat, &final_locale).map_err(|(c, _)| fail(c))?;
        Ok(Self {
            fmt,
            configuration_version: 0,
        })
    }

    /// Clone (`clone $fmt`); failure is `Error("Failed to clone NumberFormatter")`.
    pub fn try_clone(&self) -> Result<Self> {
        let fmt = self
            .fmt
            .try_clone()
            .map_err(|_| Error::Thrown("Failed to clone NumberFormatter".into()))?;
        Ok(Self {
            fmt,
            configuration_version: self.configuration_version,
        })
    }

    /// The wrapped ICU formatter.
    pub fn raw(&self) -> &NumberFormat {
        &self.fmt
    }

    /// `NumberFormatter::format($num, $type)`.  `procedural` selects the
    /// `numfmt_format()` wording of the `TYPE_CURRENCY` `ValueError`
    /// (argument numbers are, as everywhere, in method form).
    pub fn format(&self, num: Number, ty: i64, procedural: bool) -> Result<String> {
        let mut ty = ty;
        if ty == format_type::DEFAULT {
            ty = match num {
                Number::Int(_) => format_type::INT64,
                Number::Float(_) => format_type::DOUBLE,
            };
        }
        let fail = |c| Error::intl(c, "Number formatting failed");
        let out = match ty {
            format_type::INT32 => self.fmt.format_i32(wrap_i32(num.to_long())).map_err(fail)?,
            format_type::INT64 => {
                let v = match num {
                    Number::Int(i) => i,
                    Number::Float(f) => c_double_to_i64(f),
                };
                self.fmt.format_i64(v).map_err(fail)?
            }
            format_type::DOUBLE => self.fmt.format_f64(num.to_double()).map_err(fail)?,
            format_type::CURRENCY => {
                return Err(if procedural {
                    Error::arg_value(
                        2,
                        "cannot be NumberFormatter::TYPE_CURRENCY constant, use numfmt_format_currency() function instead",
                    )
                } else {
                    Error::arg_value(
                        2,
                        "cannot be NumberFormatter::TYPE_CURRENCY constant, use NumberFormatter::formatCurrency() method instead",
                    )
                });
            }
            _ => {
                return Err(Error::arg_value(
                    2,
                    "must be a NumberFormatter::TYPE_* constant",
                ))
            }
        };
        ustr::from_u16(&out).or_intl("Error converting result to UTF-8")
    }

    /// `NumberFormatter::formatCurrency($amount, $currency)`.
    pub fn format_currency(&self, amount: f64, currency: &[u8]) -> Result<String> {
        let cur = ustr::to_u16(currency).or_intl("Currency conversion to UTF-16 failed")?;
        let out = self
            .fmt
            .format_currency(amount, &cur)
            .map_err(|c| Error::object_code(c, "Number formatting failed"))?;
        ustr::from_u16(&out).or_intl("Error converting result to UTF-8")
    }

    /// `NumberFormatter::parse($string, $type, &$offset)`.  `offset` is the
    /// by-reference value (`None` when not passed); on success the updated
    /// offset is returned alongside the value.
    pub fn parse(
        &self,
        s: &[u8],
        ty: i64,
        offset: Option<i64>,
        procedural: bool,
    ) -> Result<(Parsed, Option<i64>)> {
        let mut position = 0i32;
        if let Some(o) = offset {
            position = i32::try_from(o).map_err(|_| {
                Error::arg_value(3, format!("must be between {} and {}", i32::MIN, i32::MAX))
            })?;
        }
        let u = ustr::to_u16(s).or_intl("String conversion to UTF-16 failed")?;
        if offset.is_some() {
            position = ustr::utf8_offset_to_utf16(s, position)
                .map_err(|c| Error::intl(c, "Invalid UTF-8 offset"))?;
        }
        let failed = |c| Error::intl(c, "Number parsing failed");
        let value = match ty {
            format_type::INT32 => Parsed::Int(i64::from(
                self.fmt.parse_i32(&u, &mut position).map_err(failed)?,
            )),
            format_type::INT64 => {
                Parsed::Int(self.fmt.parse_i64(&u, &mut position).map_err(failed)?)
            }
            format_type::DOUBLE => {
                Parsed::Float(self.fmt.parse_f64(&u, &mut position).map_err(failed)?)
            }
            format_type::CURRENCY => {
                return Err(if procedural {
                    Error::arg_value(
                        2,
                        "cannot be NumberFormatter::TYPE_CURRENCY constant, use numfmt_parse_currency() function instead",
                    )
                } else {
                    Error::arg_value(
                        2,
                        "cannot be NumberFormatter::TYPE_CURRENCY constant, use NumberFormatter::parseCurrency() method instead",
                    )
                })
            }
            _ => {
                return Err(Error::arg_value(
                    2,
                    "must be a NumberFormatter::TYPE_* constant",
                ))
            }
        };
        let new_offset = offset.map(|_| i64::from(ustr::utf16_offset_to_utf8(&u, position)));
        Ok((value, new_offset))
    }

    /// `NumberFormatter::parseCurrency($string, &$currency, &$offset)` →
    /// `(value, currency, new offset)`.
    pub fn parse_currency(
        &self,
        s: &[u8],
        offset: Option<i64>,
    ) -> Result<(f64, String, Option<i64>)> {
        let u = ustr::to_u16(s).or_intl("String conversion to UTF-16 failed")?;
        let mut position = 0i32;
        if let Some(o) = offset {
            position = i32::try_from(o).map_err(|_| {
                Error::arg_value(3, format!("must be between {} and {}", i32::MIN, i32::MAX))
            })?;
            position = ustr::utf8_offset_to_utf16(s, position)
                .map_err(|c| Error::intl(c, "Invalid UTF-8 offset"))?;
        }
        let (v, cur) = self
            .fmt
            .parse_currency(&u, &mut position)
            .map_err(|_| Error::intl(UErrorCode::PARSE_ERROR, "Number parsing failed"))?;
        let new_offset = offset.map(|_| i64::from(ustr::utf16_offset_to_utf8(&u, position)));
        let cur = ustr::from_u16(&cur).or_intl("Currency conversion to UTF-8 failed")?;
        Ok((v, cur, new_offset))
    }

    fn is_int_attr(attr: i64) -> bool {
        matches!(attr, 0..=11 | 13..=19)
    }

    /// `NumberFormatter::getAttribute($attr)` (int or, for
    /// `ROUNDING_INCREMENT`, float).
    pub fn get_attribute(&self, attr: i64) -> Result<Number> {
        let unsupported = || {
            Error::intl(
                UErrorCode::UNSUPPORTED_ERROR,
                "Error getting attribute value",
            )
        };
        if Self::is_int_attr(attr) {
            #[allow(clippy::cast_possible_truncation)]
            let v = self.fmt.attribute(attr as i32);
            if v == -1 {
                return Err(unsupported());
            }
            return Ok(Number::Int(i64::from(v)));
        }
        if attr == ATTR_ROUNDING_INCREMENT {
            #[allow(clippy::cast_possible_truncation)]
            let v = self.fmt.double_attribute(attr as i32);
            #[allow(clippy::float_cmp)]
            if v == -1.0 {
                return Err(unsupported());
            }
            return Ok(Number::Float(v));
        }
        Err(unsupported())
    }

    /// `NumberFormatter::setAttribute($attr, $value)`.
    pub fn set_attribute(&mut self, attr: i64, value: Number) -> Result<()> {
        if Self::is_int_attr(attr) {
            #[allow(clippy::cast_possible_truncation)]
            self.fmt
                .set_attribute(attr as i32, wrap_i32(value.to_long()));
        } else if attr == ATTR_ROUNDING_INCREMENT {
            #[allow(clippy::cast_possible_truncation)]
            self.fmt
                .set_double_attribute(attr as i32, value.to_double());
        } else {
            return Err(Error::intl(
                UErrorCode::UNSUPPORTED_ERROR,
                "Error setting attribute value",
            ));
        }
        self.configuration_version += 1;
        Ok(())
    }

    /// `NumberFormatter::getTextAttribute($attr)`.
    pub fn get_text_attribute(&self, attr: i64) -> Result<String> {
        let v = self
            .fmt
            .text_attribute(attr_i32(attr))
            .or_intl("Error getting attribute value")?;
        ustr::from_u16(&v).or_intl("Error converting value to UTF-8")
    }

    /// `NumberFormatter::setTextAttribute($attr, $value)`.
    pub fn set_text_attribute(&mut self, attr: i64, value: &[u8]) -> Result<()> {
        let v = ustr::to_u16(value).or_intl("Error converting attribute value to UTF-16")?;
        self.fmt
            .set_text_attribute(attr_i32(attr), &v)
            .or_intl("Error setting text attribute")?;
        self.configuration_version += 1;
        Ok(())
    }

    fn check_symbol(symbol: i64) -> Result<i32> {
        if !(0..SYMBOL_COUNT).contains(&symbol) {
            return Err(Error::global(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                "invalid symbol value",
            ));
        }
        Ok(attr_i32(symbol))
    }

    /// `NumberFormatter::getSymbol($symbol)`.  The `"invalid symbol value"`
    /// error is global-only (raised before the object is fetched).
    pub fn get_symbol(&self, symbol: i64) -> Result<String> {
        let s = Self::check_symbol(symbol)?;
        let v = self.fmt.symbol(s).or_intl("Error getting symbol value")?;
        ustr::from_u16(&v).or_intl("Error converting value to UTF-8")
    }

    /// `NumberFormatter::setSymbol($symbol, $value)`.
    pub fn set_symbol(&mut self, symbol: i64, value: &[u8]) -> Result<()> {
        let s = Self::check_symbol(symbol)?;
        let v = ustr::to_u16(value).or_intl("Error converting symbol value to UTF-16")?;
        self.fmt
            .set_symbol(s, &v)
            .or_intl("Error setting symbol value")?;
        self.configuration_version += 1;
        Ok(())
    }

    /// `NumberFormatter::getPattern()`.
    pub fn get_pattern(&self) -> Result<String> {
        let v = self
            .fmt
            .to_pattern()
            .or_intl("Error getting formatter pattern")?;
        ustr::from_u16(&v).or_intl("Error converting value to UTF-8")
    }

    /// `NumberFormatter::setPattern($pattern)`.
    pub fn set_pattern(&mut self, pattern: &[u8]) -> Result<()> {
        let v = ustr::to_u16(pattern).or_intl("Error converting pattern to UTF-16")?;
        self.fmt.apply_pattern(&v).map_err(|(c, pe)| {
            Error::object_code(
                c,
                format!(
                    "Error setting pattern value at line {}, offset {}",
                    pe.line, pe.offset
                ),
            )
        })?;
        self.configuration_version += 1;
        Ok(())
    }

    /// `NumberFormatter::getLocale($type)`.
    pub fn get_locale(&self, ty: i64) -> Result<String> {
        self.fmt
            .locale_by_type(attr_i32(ty))
            .or_intl("Error getting locale")
    }
}

#[allow(clippy::cast_possible_truncation)]
fn attr_i32(v: i64) -> i32 {
    v as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nf(loc: &str, st: i64) -> NumberFormatter {
        NumberFormatter::new(loc, st, None).unwrap()
    }

    /// `numfmt_create('en', 99999)`: php-src master builds the formatter
    /// with ICU's C++ `NumberFormat::createInstance()`, which rejects an
    /// out-of-range style with `U_ILLEGAL_ARGUMENT_ERROR` (1, measured
    /// against the system ICU).  PHP 8.5 still called `unum_open()`,
    /// whose answer is `U_UNSUPPORTED_ERROR` (16); master wins.
    #[test]
    fn unknown_style_is_an_illegal_argument() {
        let Err(e) = NumberFormatter::new("en", 99999, None) else {
            panic!("style 99999 must fail");
        };
        assert_eq!(e.intl_code(), UErrorCode::ILLEGAL_ARGUMENT_ERROR);
    }

    #[test]
    fn basic_format() {
        let f = nf("en_US", style::DECIMAL);
        assert_eq!(
            f.format(Number::Float(1_234_567.891), 0, false).unwrap(),
            "1,234,567.891"
        );
        assert_eq!(
            f.format(Number::Int(-1_234_567), 0, false).unwrap(),
            "-1,234,567"
        );
        let f = nf("de_DE", style::CURRENCY);
        assert_eq!(
            f.format_currency(1234.5, b"EUR").unwrap(),
            "1.234,50\u{a0}€"
        );
        let f = nf("en_US", style::SPELLOUT);
        assert_eq!(f.format(Number::Int(42), 0, false).unwrap(), "forty-two");
    }

    #[test]
    fn creation_errors() {
        let e = NumberFormatter::new("whatever", 1, None).unwrap_err();
        assert_eq!(e, Error::arg_value(1, "\"whatever\" is invalid"));
        let e = NumberFormatter::new("en", -1, None).unwrap_err();
        assert_eq!(e.intl_code(), UErrorCode::ILLEGAL_ARGUMENT_ERROR);
        let e = NumberFormatter::new("en_US", style::PATTERN_RULEBASED, None).unwrap_err();
        assert_eq!(e.intl_code(), UErrorCode::MEMORY_ALLOCATION_ERROR);
    }

    #[test]
    fn parse_with_offset() {
        let f = nf("en_US", style::DECIMAL);
        let (v, off) = f
            .parse(b"123,456.75 abc", format_type::DOUBLE, Some(0), false)
            .unwrap();
        assert_eq!(v, Parsed::Float(123_456.75));
        assert_eq!(off, Some(10));
        let e = f
            .parse(b"abc", format_type::DOUBLE, None, false)
            .unwrap_err();
        assert_eq!(e.intl_code(), UErrorCode::PARSE_ERROR);
    }

    #[test]
    fn dval() {
        assert_eq!(dval_to_lval(1e19), -8_446_744_073_709_551_616);
        assert_eq!(dval_to_lval(f64::NAN), 0);
        assert_eq!(c_double_to_i64(1e30), i64::MIN);
    }
}
