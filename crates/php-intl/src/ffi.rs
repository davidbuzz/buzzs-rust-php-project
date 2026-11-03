//! Raw bindings to the system ICU **C API** plus the small amount of glue
//! (buffer pre-flighting, enumeration draining, RAII handles) every service
//! module builds on.
//!
//! ICU exports every C symbol with a version suffix (`ucol_open_74` for
//! ICU 74.x).  `build.rs` detects the installed version via pkg-config and
//! exports the suffix as `PHP_INTL_ICU_SUFFIX`; the [`icu!`] macro below
//! attaches it to every declaration with `#[link_name]`.
//!
//! This module (together with the thin RAII wrappers in the service modules,
//! each of which documents its `unsafe` blocks) is the only place the crate
//! uses `unsafe`.
#![allow(non_camel_case_types, non_snake_case, clippy::upper_case_acronyms)]
// FFI out-parameters are passed as `&mut x` coerced to `*mut T` throughout.
#![allow(clippy::borrow_as_ptr)]
#![allow(unsafe_code)]

use std::ffi::{c_char, c_void, CStr};
use std::ptr;

use crate::error::UErrorCode;

/// `UChar` — one UTF-16 code unit.
pub type UChar = u16;
/// `UChar32` — a code point (or a negative sentinel).
pub type UChar32 = i32;
/// `UBool` — ICU's `int8_t` boolean.
pub type UBool = i8;
/// `UDate` — milliseconds since the epoch as a double.
pub type UDate = f64;

/// Declares opaque ICU handle types.
macro_rules! opaque {
    ($($name:ident),* $(,)?) => {
        $(
            #[repr(C)]
            #[derive(Debug)]
            pub struct $name {
                _private: [u8; 0],
            }
        )*
    };
}

opaque!(
    UNumberFormat,
    UDateFormat,
    UCollator,
    UEnumeration,
    UCalendar,
    UBreakIterator,
    UTransliterator,
    UResourceBundle,
    USpoofChecker,
    USpoofCheckResult,
    UConverter,
    UDateTimePatternGenerator,
    UListFormatter,
    UFormattedList,
    UIDNA,
    UNormalizer2,
    UPluralRules,
    UNumberFormatter,
    UFormattedNumber,
    UNumberRangeFormatter,
    UFormattedNumberRange,
    URelativeDateTimeFormatter,
    UStringSearch,
    USet,
    UFieldPositionIterator,
    ULocaleDisplayNames,
    UFormattedValue,
    UConstrainedFieldPosition,
    UCharsetDetector,
    UNumberingSystem,
    UText,
);

/// `UParseError`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UParseError {
    pub line: i32,
    pub offset: i32,
    pub preContext: [UChar; 16],
    pub postContext: [UChar; 16],
}

/// `UFieldPosition`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UFieldPosition {
    pub field: i32,
    pub beginIndex: i32,
    pub endIndex: i32,
}

/// `UIDNAInfo`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct UIDNAInfo {
    pub size: i16,
    pub isTransitionalDifferent: UBool,
    pub reservedB3: UBool,
    pub errors: u32,
    pub reservedI2: i32,
    pub reservedI3: i32,
}

impl Default for UIDNAInfo {
    fn default() -> Self {
        Self {
            size: i16::try_from(std::mem::size_of::<Self>()).unwrap_or(16),
            isTransitionalDifferent: 0,
            reservedB3: 0,
            errors: 0,
            reservedI2: 0,
            reservedI3: 0,
        }
    }
}

/// `UTransPosition`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UTransPosition {
    pub contextStart: i32,
    pub contextLimit: i32,
    pub start: i32,
    pub limit: i32,
}

/// `UConverterFromUnicodeArgs`.
#[repr(C)]
#[derive(Debug)]
pub struct UConverterFromUnicodeArgs {
    pub size: u16,
    pub flush: UBool,
    pub converter: *mut UConverter,
    pub source: *const UChar,
    pub sourceLimit: *const UChar,
    pub target: *mut c_char,
    pub targetLimit: *const c_char,
    pub offsets: *mut i32,
}

/// `UConverterToUnicodeArgs`.
#[repr(C)]
#[derive(Debug)]
pub struct UConverterToUnicodeArgs {
    pub size: u16,
    pub flush: UBool,
    pub converter: *mut UConverter,
    pub source: *const c_char,
    pub sourceLimit: *const c_char,
    pub target: *mut UChar,
    pub targetLimit: *const UChar,
    pub offsets: *mut i32,
}

/// `UConverterToUCallback`.
pub type UConverterToUCallback = Option<
    unsafe extern "C" fn(
        context: *const c_void,
        args: *mut UConverterToUnicodeArgs,
        codeUnits: *const c_char,
        length: i32,
        reason: i32,
        pErrorCode: *mut UErrorCode,
    ),
>;

/// `UConverterFromUCallback`.
pub type UConverterFromUCallback = Option<
    unsafe extern "C" fn(
        context: *const c_void,
        args: *mut UConverterFromUnicodeArgs,
        codeUnits: *const UChar,
        length: i32,
        codePoint: UChar32,
        reason: i32,
        pErrorCode: *mut UErrorCode,
    ),
>;

/// `UCharEnumTypeRange` callback (`u_enumCharTypes`).
pub type UCharEnumTypeRange = Option<
    unsafe extern "C" fn(context: *const c_void, start: UChar32, limit: UChar32, ty: i32) -> UBool,
>;

/// `UEnumCharNamesFn` callback (`u_enumCharNames`).
pub type UEnumCharNamesFn = Option<
    unsafe extern "C" fn(
        context: *mut c_void,
        code: UChar32,
        nameChoice: i32,
        name: *const c_char,
        length: i32,
    ) -> UBool,
>;

/// Declares versioned ICU C functions.
macro_rules! icu {
    ($(fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?;)*) => {
        extern "C" {
            $(
                #[link_name = concat!(stringify!($name), env!("PHP_INTL_ICU_SUFFIX"))]
                pub fn $name($($arg: $ty),*) $(-> $ret)?;
            )*
        }
    };
}

// DECLS-BEGIN
// Generated by tools/gen_ffi.py from the ICU 74 headers.
icu! {
    fn u_errorName(code: UErrorCode) -> *const c_char;
    fn u_versionFromString(versionArray: *mut u8, versionString: *const c_char);
    fn u_versionFromUString(versionArray: *mut u8, versionString: *const UChar);
    fn u_versionToString(versionArray: *mut u8, versionString: *mut c_char);
    fn u_getVersion(versionArray: *mut u8);
    fn uenum_close(en: *mut UEnumeration);
    fn uenum_count(en: *mut UEnumeration, status: *mut UErrorCode) -> i32;
    fn uenum_unext(en: *mut UEnumeration, resultLength: *mut i32, status: *mut UErrorCode) -> *const UChar;
    fn uenum_next(en: *mut UEnumeration, resultLength: *mut i32, status: *mut UErrorCode) -> *const c_char;
    fn uenum_reset(en: *mut UEnumeration, status: *mut UErrorCode);
    fn uloc_getDefault() -> *const c_char;
    fn uloc_setDefault(localeID: *const c_char, status: *mut UErrorCode);
    fn uloc_getLanguage(localeID: *const c_char, language: *mut c_char, languageCapacity: i32, err: *mut UErrorCode) -> i32;
    fn uloc_getScript(localeID: *const c_char, script: *mut c_char, scriptCapacity: i32, err: *mut UErrorCode) -> i32;
    fn uloc_getCountry(localeID: *const c_char, country: *mut c_char, countryCapacity: i32, err: *mut UErrorCode) -> i32;
    fn uloc_getVariant(localeID: *const c_char, variant: *mut c_char, variantCapacity: i32, err: *mut UErrorCode) -> i32;
    fn uloc_getName(localeID: *const c_char, name: *mut c_char, nameCapacity: i32, err: *mut UErrorCode) -> i32;
    fn uloc_canonicalize(localeID: *const c_char, name: *mut c_char, nameCapacity: i32, err: *mut UErrorCode) -> i32;
    fn uloc_getISO3Language(localeID: *const c_char) -> *const c_char;
    fn uloc_getISO3Country(localeID: *const c_char) -> *const c_char;
    fn uloc_getLCID(localeID: *const c_char) -> u32;
    fn uloc_getDisplayLanguage(locale: *const c_char, displayLocale: *const c_char, language: *mut UChar, languageCapacity: i32, status: *mut UErrorCode) -> i32;
    fn uloc_getDisplayScript(locale: *const c_char, displayLocale: *const c_char, script: *mut UChar, scriptCapacity: i32, status: *mut UErrorCode) -> i32;
    fn uloc_getDisplayCountry(locale: *const c_char, displayLocale: *const c_char, country: *mut UChar, countryCapacity: i32, status: *mut UErrorCode) -> i32;
    fn uloc_getDisplayVariant(locale: *const c_char, displayLocale: *const c_char, variant: *mut UChar, variantCapacity: i32, status: *mut UErrorCode) -> i32;
    fn uloc_getDisplayKeyword(keyword: *const c_char, displayLocale: *const c_char, dest: *mut UChar, destCapacity: i32, status: *mut UErrorCode) -> i32;
    fn uloc_getDisplayKeywordValue(locale: *const c_char, keyword: *const c_char, displayLocale: *const c_char, dest: *mut UChar, destCapacity: i32, status: *mut UErrorCode) -> i32;
    fn uloc_getDisplayName(localeID: *const c_char, inLocaleID: *const c_char, result: *mut UChar, maxResultSize: i32, err: *mut UErrorCode) -> i32;
    fn uloc_getAvailable(n: i32) -> *const c_char;
    fn uloc_countAvailable() -> i32;
    fn uloc_openAvailableByType(type_: i32, status: *mut UErrorCode) -> *mut UEnumeration;
    fn uloc_getISOLanguages() -> *const *const c_char;
    fn uloc_getISOCountries() -> *const *const c_char;
    fn uloc_getParent(localeID: *const c_char, parent: *mut c_char, parentCapacity: i32, err: *mut UErrorCode) -> i32;
    fn uloc_getBaseName(localeID: *const c_char, name: *mut c_char, nameCapacity: i32, err: *mut UErrorCode) -> i32;
    fn uloc_openKeywords(localeID: *const c_char, status: *mut UErrorCode) -> *mut UEnumeration;
    fn uloc_getKeywordValue(localeID: *const c_char, keywordName: *const c_char, buffer: *mut c_char, bufferCapacity: i32, status: *mut UErrorCode) -> i32;
    fn uloc_setKeywordValue(keywordName: *const c_char, keywordValue: *const c_char, buffer: *mut c_char, bufferCapacity: i32, status: *mut UErrorCode) -> i32;
    fn uloc_isRightToLeft(locale: *const c_char) -> UBool;
    fn uloc_getCharacterOrientation(localeId: *const c_char, status: *mut UErrorCode) -> i32;
    fn uloc_getLineOrientation(localeId: *const c_char, status: *mut UErrorCode) -> i32;
    fn uloc_acceptLanguageFromHTTP(result: *mut c_char, resultAvailable: i32, outResult: *mut i32, httpAcceptLanguage: *const c_char, availableLocales: *mut UEnumeration, status: *mut UErrorCode) -> i32;
    fn uloc_acceptLanguage(result: *mut c_char, resultAvailable: i32, outResult: *mut i32, acceptList: *mut *const c_char, acceptListCount: i32, availableLocales: *mut UEnumeration, status: *mut UErrorCode) -> i32;
    fn uloc_getLocaleForLCID(hostID: u32, locale: *mut c_char, localeCapacity: i32, status: *mut UErrorCode) -> i32;
    fn uloc_addLikelySubtags(localeID: *const c_char, maximizedLocaleID: *mut c_char, maximizedLocaleIDCapacity: i32, err: *mut UErrorCode) -> i32;
    fn uloc_minimizeSubtags(localeID: *const c_char, minimizedLocaleID: *mut c_char, minimizedLocaleIDCapacity: i32, err: *mut UErrorCode) -> i32;
    fn uloc_forLanguageTag(langtag: *const c_char, localeID: *mut c_char, localeIDCapacity: i32, parsedLength: *mut i32, err: *mut UErrorCode) -> i32;
    fn uloc_toLanguageTag(localeID: *const c_char, langtag: *mut c_char, langtagCapacity: i32, strict: UBool, err: *mut UErrorCode) -> i32;
    fn uloc_toUnicodeLocaleKey(keyword: *const c_char) -> *const c_char;
    fn uloc_toUnicodeLocaleType(keyword: *const c_char, value: *const c_char) -> *const c_char;
    fn uloc_toLegacyKey(keyword: *const c_char) -> *const c_char;
    fn uloc_toLegacyType(keyword: *const c_char, value: *const c_char) -> *const c_char;
    fn unum_open(style: i32, pattern: *const UChar, patternLength: i32, locale: *const c_char, parseErr: *mut UParseError, status: *mut UErrorCode) -> *mut UNumberFormat;
    fn unum_close(fmt: *mut UNumberFormat);
    fn unum_clone(fmt: *const UNumberFormat, status: *mut UErrorCode) -> *mut UNumberFormat;
    fn unum_format(fmt: *const UNumberFormat, number: i32, result: *mut UChar, resultLength: i32, pos: *mut UFieldPosition, status: *mut UErrorCode) -> i32;
    fn unum_formatInt64(fmt: *const UNumberFormat, number: i64, result: *mut UChar, resultLength: i32, pos: *mut UFieldPosition, status: *mut UErrorCode) -> i32;
    fn unum_formatDouble(fmt: *const UNumberFormat, number: f64, result: *mut UChar, resultLength: i32, pos: *mut UFieldPosition, status: *mut UErrorCode) -> i32;
    fn unum_formatDoubleForFields(format: *const UNumberFormat, number: f64, result: *mut UChar, resultLength: i32, fpositer: *mut UFieldPositionIterator, status: *mut UErrorCode) -> i32;
    fn unum_formatDecimal(fmt: *const UNumberFormat, number: *const c_char, length: i32, result: *mut UChar, resultLength: i32, pos: *mut UFieldPosition, status: *mut UErrorCode) -> i32;
    fn unum_formatDoubleCurrency(fmt: *const UNumberFormat, number: f64, currency: *mut UChar, result: *mut UChar, resultLength: i32, pos: *mut UFieldPosition, status: *mut UErrorCode) -> i32;
    fn unum_formatUFormattable(fmt: *const UNumberFormat, number: *const *mut c_void, result: *mut UChar, resultLength: i32, pos: *mut UFieldPosition, status: *mut UErrorCode) -> i32;
    fn unum_parse(fmt: *const UNumberFormat, text: *const UChar, textLength: i32, parsePos: *mut i32, status: *mut UErrorCode) -> i32;
    fn unum_parseInt64(fmt: *const UNumberFormat, text: *const UChar, textLength: i32, parsePos: *mut i32, status: *mut UErrorCode) -> i64;
    fn unum_parseDouble(fmt: *const UNumberFormat, text: *const UChar, textLength: i32, parsePos: *mut i32, status: *mut UErrorCode) -> f64;
    fn unum_parseDecimal(fmt: *const UNumberFormat, text: *const UChar, textLength: i32, parsePos: *mut i32, outBuf: *mut c_char, outBufLength: i32, status: *mut UErrorCode) -> i32;
    fn unum_parseDoubleCurrency(fmt: *const UNumberFormat, text: *const UChar, textLength: i32, parsePos: *mut i32, currency: *mut UChar, status: *mut UErrorCode) -> f64;
    fn unum_parseToUFormattable(fmt: *const UNumberFormat, result: *mut *mut c_void, text: *const UChar, textLength: i32, parsePos: *mut i32, status: *mut UErrorCode) -> *mut *mut c_void;
    fn unum_applyPattern(format: *mut UNumberFormat, localized: UBool, pattern: *const UChar, patternLength: i32, parseError: *mut UParseError, status: *mut UErrorCode);
    fn unum_getAvailable(localeIndex: i32) -> *const c_char;
    fn unum_countAvailable() -> i32;
    fn unum_hasAttribute(fmt: *const UNumberFormat, attr: i32) -> bool;
    fn unum_getAttribute(fmt: *const UNumberFormat, attr: i32) -> i32;
    fn unum_setAttribute(fmt: *mut UNumberFormat, attr: i32, newValue: i32);
    fn unum_getDoubleAttribute(fmt: *const UNumberFormat, attr: i32) -> f64;
    fn unum_setDoubleAttribute(fmt: *mut UNumberFormat, attr: i32, newValue: f64);
    fn unum_getTextAttribute(fmt: *const UNumberFormat, tag: i32, result: *mut UChar, resultLength: i32, status: *mut UErrorCode) -> i32;
    fn unum_setTextAttribute(fmt: *mut UNumberFormat, tag: i32, newValue: *const UChar, newValueLength: i32, status: *mut UErrorCode);
    fn unum_toPattern(fmt: *const UNumberFormat, isPatternLocalized: UBool, result: *mut UChar, resultLength: i32, status: *mut UErrorCode) -> i32;
    fn unum_getSymbol(fmt: *const UNumberFormat, symbol: i32, buffer: *mut UChar, size: i32, status: *mut UErrorCode) -> i32;
    fn unum_setSymbol(fmt: *mut UNumberFormat, symbol: i32, value: *const UChar, length: i32, status: *mut UErrorCode);
    fn unum_getLocaleByType(fmt: *const UNumberFormat, type_: i32, status: *mut UErrorCode) -> *const c_char;
    fn unum_setContext(fmt: *mut UNumberFormat, value: i32, status: *mut UErrorCode);
    fn unum_getContext(fmt: *const UNumberFormat, type_: i32, status: *mut UErrorCode) -> i32;
    fn udat_toCalendarDateField(field: i32) -> i32;
    fn udat_open(timeStyle: i32, dateStyle: i32, locale: *const c_char, tzID: *const UChar, tzIDLength: i32, pattern: *const UChar, patternLength: i32, status: *mut UErrorCode) -> *mut UDateFormat;
    fn udat_close(format: *mut UDateFormat);
    fn udat_getBooleanAttribute(fmt: *const UDateFormat, attr: i32, status: *mut UErrorCode) -> UBool;
    fn udat_setBooleanAttribute(fmt: *mut UDateFormat, attr: i32, newValue: UBool, status: *mut UErrorCode);
    fn udat_clone(fmt: *const UDateFormat, status: *mut UErrorCode) -> *mut UDateFormat;
    fn udat_format(format: *const UDateFormat, dateToFormat: UDate, result: *mut UChar, resultLength: i32, position: *mut UFieldPosition, status: *mut UErrorCode) -> i32;
    fn udat_formatCalendar(format: *const UDateFormat, calendar: *mut UCalendar, result: *mut UChar, capacity: i32, position: *mut UFieldPosition, status: *mut UErrorCode) -> i32;
    fn udat_formatForFields(format: *const UDateFormat, dateToFormat: UDate, result: *mut UChar, resultLength: i32, fpositer: *mut UFieldPositionIterator, status: *mut UErrorCode) -> i32;
    fn udat_formatCalendarForFields(format: *const UDateFormat, calendar: *mut UCalendar, result: *mut UChar, capacity: i32, fpositer: *mut UFieldPositionIterator, status: *mut UErrorCode) -> i32;
    fn udat_parse(format: *const UDateFormat, text: *const UChar, textLength: i32, parsePos: *mut i32, status: *mut UErrorCode) -> UDate;
    fn udat_parseCalendar(format: *const UDateFormat, calendar: *mut UCalendar, text: *const UChar, textLength: i32, parsePos: *mut i32, status: *mut UErrorCode);
    fn udat_isLenient(fmt: *const UDateFormat) -> UBool;
    fn udat_setLenient(fmt: *mut UDateFormat, isLenient: UBool);
    fn udat_getCalendar(fmt: *const UDateFormat) -> *const UCalendar;
    fn udat_setCalendar(fmt: *mut UDateFormat, calendarToSet: *const UCalendar);
    fn udat_getNumberFormat(fmt: *const UDateFormat) -> *const UNumberFormat;
    fn udat_getNumberFormatForField(fmt: *const UDateFormat, field: UChar) -> *const UNumberFormat;
    fn udat_adoptNumberFormatForFields(fmt: *mut UDateFormat, fields: *const UChar, numberFormatToSet: *mut UNumberFormat, status: *mut UErrorCode);
    fn udat_setNumberFormat(fmt: *mut UDateFormat, numberFormatToSet: *const UNumberFormat);
    fn udat_adoptNumberFormat(fmt: *mut UDateFormat, numberFormatToAdopt: *mut UNumberFormat);
    fn udat_getAvailable(localeIndex: i32) -> *const c_char;
    fn udat_countAvailable() -> i32;
    fn udat_get2DigitYearStart(fmt: *const UDateFormat, status: *mut UErrorCode) -> UDate;
    fn udat_set2DigitYearStart(fmt: *mut UDateFormat, d: UDate, status: *mut UErrorCode);
    fn udat_toPattern(fmt: *const UDateFormat, localized: UBool, result: *mut UChar, resultLength: i32, status: *mut UErrorCode) -> i32;
    fn udat_applyPattern(format: *mut UDateFormat, localized: UBool, pattern: *const UChar, patternLength: i32);
    fn udat_getSymbols(fmt: *const UDateFormat, type_: i32, symbolIndex: i32, result: *mut UChar, resultLength: i32, status: *mut UErrorCode) -> i32;
    fn udat_countSymbols(fmt: *const UDateFormat, type_: i32) -> i32;
    fn udat_setSymbols(format: *mut UDateFormat, type_: i32, symbolIndex: i32, value: *mut UChar, valueLength: i32, status: *mut UErrorCode);
    fn udat_getLocaleByType(fmt: *const UDateFormat, type_: i32, status: *mut UErrorCode) -> *const c_char;
    fn udat_setContext(fmt: *mut UDateFormat, value: i32, status: *mut UErrorCode);
    fn udat_getContext(fmt: *const UDateFormat, type_: i32, status: *mut UErrorCode) -> i32;
    fn udat_toPatternRelativeDate(fmt: *const UDateFormat, result: *mut UChar, resultLength: i32, status: *mut UErrorCode) -> i32;
    fn udat_toPatternRelativeTime(fmt: *const UDateFormat, result: *mut UChar, resultLength: i32, status: *mut UErrorCode) -> i32;
    fn udat_applyPatternRelative(format: *mut UDateFormat, datePattern: *const UChar, datePatternLength: i32, timePattern: *const UChar, timePatternLength: i32, status: *mut UErrorCode);
    fn ucal_openTimeZoneIDEnumeration(zoneType: i32, region: *const c_char, rawOffset: *const i32, ec: *mut UErrorCode) -> *mut UEnumeration;
    fn ucal_openTimeZones(ec: *mut UErrorCode) -> *mut UEnumeration;
    fn ucal_openCountryTimeZones(country: *const c_char, ec: *mut UErrorCode) -> *mut UEnumeration;
    fn ucal_getDefaultTimeZone(result: *mut UChar, resultCapacity: i32, ec: *mut UErrorCode) -> i32;
    fn ucal_setDefaultTimeZone(zoneID: *const UChar, ec: *mut UErrorCode);
    fn ucal_getHostTimeZone(result: *mut UChar, resultCapacity: i32, ec: *mut UErrorCode) -> i32;
    fn ucal_getDSTSavings(zoneID: *const UChar, ec: *mut UErrorCode) -> i32;
    fn ucal_getNow() -> UDate;
    fn ucal_open(zoneID: *const UChar, len: i32, locale: *const c_char, type_: i32, status: *mut UErrorCode) -> *mut UCalendar;
    fn ucal_close(cal: *mut UCalendar);
    fn ucal_clone(cal: *const UCalendar, status: *mut UErrorCode) -> *mut UCalendar;
    fn ucal_setTimeZone(cal: *mut UCalendar, zoneID: *const UChar, len: i32, status: *mut UErrorCode);
    fn ucal_getTimeZoneID(cal: *const UCalendar, result: *mut UChar, resultLength: i32, status: *mut UErrorCode) -> i32;
    fn ucal_getTimeZoneDisplayName(cal: *const UCalendar, type_: i32, locale: *const c_char, result: *mut UChar, resultLength: i32, status: *mut UErrorCode) -> i32;
    fn ucal_inDaylightTime(cal: *const UCalendar, status: *mut UErrorCode) -> UBool;
    fn ucal_setGregorianChange(cal: *mut UCalendar, date: UDate, pErrorCode: *mut UErrorCode);
    fn ucal_getGregorianChange(cal: *const UCalendar, pErrorCode: *mut UErrorCode) -> UDate;
    fn ucal_getAttribute(cal: *const UCalendar, attr: i32) -> i32;
    fn ucal_setAttribute(cal: *mut UCalendar, attr: i32, newValue: i32);
    fn ucal_getAvailable(localeIndex: i32) -> *const c_char;
    fn ucal_countAvailable() -> i32;
    fn ucal_getMillis(cal: *const UCalendar, status: *mut UErrorCode) -> UDate;
    fn ucal_setMillis(cal: *mut UCalendar, dateTime: UDate, status: *mut UErrorCode);
    fn ucal_setDate(cal: *mut UCalendar, year: i32, month: i32, date: i32, status: *mut UErrorCode);
    fn ucal_setDateTime(cal: *mut UCalendar, year: i32, month: i32, date: i32, hour: i32, minute: i32, second: i32, status: *mut UErrorCode);
    fn ucal_equivalentTo(cal1: *const UCalendar, cal2: *const UCalendar) -> UBool;
    fn ucal_add(cal: *mut UCalendar, field: i32, amount: i32, status: *mut UErrorCode);
    fn ucal_roll(cal: *mut UCalendar, field: i32, amount: i32, status: *mut UErrorCode);
    fn ucal_get(cal: *const UCalendar, field: i32, status: *mut UErrorCode) -> i32;
    fn ucal_set(cal: *mut UCalendar, field: i32, value: i32);
    fn ucal_isSet(cal: *const UCalendar, field: i32) -> UBool;
    fn ucal_clearField(cal: *mut UCalendar, field: i32);
    fn ucal_clear(calendar: *mut UCalendar);
    fn ucal_getLimit(cal: *const UCalendar, field: i32, type_: i32, status: *mut UErrorCode) -> i32;
    fn ucal_getLocaleByType(cal: *const UCalendar, type_: i32, status: *mut UErrorCode) -> *const c_char;
    fn ucal_getTZDataVersion(status: *mut UErrorCode) -> *const c_char;
    fn ucal_getCanonicalTimeZoneID(id: *const UChar, len: i32, result: *mut UChar, resultCapacity: i32, isSystemID: *mut UBool, status: *mut UErrorCode) -> i32;
    fn ucal_getIanaTimeZoneID(id: *const UChar, len: i32, result: *mut UChar, resultCapacity: i32, status: *mut UErrorCode) -> i32;
    fn ucal_getType(cal: *const UCalendar, status: *mut UErrorCode) -> *const c_char;
    fn ucal_getKeywordValuesForLocale(key: *const c_char, locale: *const c_char, commonlyUsed: UBool, status: *mut UErrorCode) -> *mut UEnumeration;
    fn ucal_getDayOfWeekType(cal: *const UCalendar, dayOfWeek: i32, status: *mut UErrorCode) -> i32;
    fn ucal_getWeekendTransition(cal: *const UCalendar, dayOfWeek: i32, status: *mut UErrorCode) -> i32;
    fn ucal_isWeekend(cal: *const UCalendar, date: UDate, status: *mut UErrorCode) -> UBool;
    fn ucal_getFieldDifference(cal: *mut UCalendar, target: UDate, field: i32, status: *mut UErrorCode) -> i32;
    fn ucal_getTimeZoneTransitionDate(cal: *const UCalendar, type_: i32, transition: *mut UDate, status: *mut UErrorCode) -> UBool;
    fn ucal_getWindowsTimeZoneID(id: *const UChar, len: i32, winid: *mut UChar, winidCapacity: i32, status: *mut UErrorCode) -> i32;
    fn ucal_getTimeZoneIDForWindowsID(winid: *const UChar, len: i32, region: *const c_char, id: *mut UChar, idCapacity: i32, status: *mut UErrorCode) -> i32;
    fn ucal_getTimeZoneOffsetFromLocal(cal: *const UCalendar, nonExistingTimeOpt: i32, duplicatedTimeOpt: i32, rawOffset: *mut i32, dstOffset: *mut i32, status: *mut UErrorCode);
    fn ucol_open(loc: *const c_char, status: *mut UErrorCode) -> *mut UCollator;
    fn ucol_openRules(rules: *const UChar, rulesLength: i32, normalizationMode: i32, strength: i32, parseError: *mut UParseError, status: *mut UErrorCode) -> *mut UCollator;
    fn ucol_getContractionsAndExpansions(coll: *const UCollator, contractions: *mut USet, expansions: *mut USet, addPrefixes: UBool, status: *mut UErrorCode);
    fn ucol_close(coll: *mut UCollator);
    fn ucol_strcoll(coll: *const UCollator, source: *const UChar, sourceLength: i32, target: *const UChar, targetLength: i32) -> i32;
    fn ucol_strcollUTF8(coll: *const UCollator, source: *const c_char, sourceLength: i32, target: *const c_char, targetLength: i32, status: *mut UErrorCode) -> i32;
    fn ucol_greater(coll: *const UCollator, source: *const UChar, sourceLength: i32, target: *const UChar, targetLength: i32) -> UBool;
    fn ucol_greaterOrEqual(coll: *const UCollator, source: *const UChar, sourceLength: i32, target: *const UChar, targetLength: i32) -> UBool;
    fn ucol_equal(coll: *const UCollator, source: *const UChar, sourceLength: i32, target: *const UChar, targetLength: i32) -> UBool;
    fn ucol_getStrength(coll: *const UCollator) -> i32;
    fn ucol_setStrength(coll: *mut UCollator, strength: i32);
    fn ucol_getReorderCodes(coll: *const UCollator, dest: *mut i32, destCapacity: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn ucol_setReorderCodes(coll: *mut UCollator, reorderCodes: *const i32, reorderCodesLength: i32, pErrorCode: *mut UErrorCode);
    fn ucol_getEquivalentReorderCodes(reorderCode: i32, dest: *mut i32, destCapacity: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn ucol_getDisplayName(objLoc: *const c_char, dispLoc: *const c_char, result: *mut UChar, resultLength: i32, status: *mut UErrorCode) -> i32;
    fn ucol_getAvailable(localeIndex: i32) -> *const c_char;
    fn ucol_countAvailable() -> i32;
    fn ucol_openAvailableLocales(status: *mut UErrorCode) -> *mut UEnumeration;
    fn ucol_getKeywords(status: *mut UErrorCode) -> *mut UEnumeration;
    fn ucol_getKeywordValues(keyword: *const c_char, status: *mut UErrorCode) -> *mut UEnumeration;
    fn ucol_getKeywordValuesForLocale(key: *const c_char, locale: *const c_char, commonlyUsed: UBool, status: *mut UErrorCode) -> *mut UEnumeration;
    fn ucol_getFunctionalEquivalent(result: *mut c_char, resultCapacity: i32, keyword: *const c_char, locale: *const c_char, isAvailable: *mut UBool, status: *mut UErrorCode) -> i32;
    fn ucol_getRules(coll: *const UCollator, length: *mut i32) -> *const UChar;
    fn ucol_getSortKey(coll: *const UCollator, source: *const UChar, sourceLength: i32, result: *mut u8, resultLength: i32) -> i32;
    fn ucol_getBound(source: *const u8, sourceLength: i32, boundType: i32, noOfLevels: u32, result: *mut u8, resultLength: i32, status: *mut UErrorCode) -> i32;
    fn ucol_getVersion(coll: *const UCollator, info: *mut u8);
    fn ucol_getUCAVersion(coll: *const UCollator, info: *mut u8);
    fn ucol_mergeSortkeys(src1: *const u8, src1Length: i32, src2: *const u8, src2Length: i32, dest: *mut u8, destCapacity: i32) -> i32;
    fn ucol_setAttribute(coll: *mut UCollator, attr: i32, value: i32, status: *mut UErrorCode);
    fn ucol_getAttribute(coll: *const UCollator, attr: i32, status: *mut UErrorCode) -> i32;
    fn ucol_setMaxVariable(coll: *mut UCollator, group: i32, pErrorCode: *mut UErrorCode);
    fn ucol_getMaxVariable(coll: *const UCollator) -> i32;
    fn ucol_getVariableTop(coll: *const UCollator, status: *mut UErrorCode) -> u32;
    fn ucol_clone(coll: *const UCollator, status: *mut UErrorCode) -> *mut UCollator;
    fn ucol_getRulesEx(coll: *const UCollator, delta: i32, buffer: *mut UChar, bufferLen: i32) -> i32;
    fn ucol_getLocaleByType(coll: *const UCollator, type_: i32, status: *mut UErrorCode) -> *const c_char;
    fn ucol_getTailoredSet(coll: *const UCollator, status: *mut UErrorCode) -> *mut USet;
    fn ucol_getUnsafeSet(coll: *const UCollator, unsafe_: *mut USet, status: *mut UErrorCode) -> i32;
    fn ucol_prepareShortStringOpen(definition: *const c_char, forceDefaults: UBool, parseError: *mut UParseError, status: *mut UErrorCode);
    fn ucol_cloneBinary(coll: *const UCollator, buffer: *mut u8, capacity: i32, status: *mut UErrorCode) -> i32;
    fn ucol_openBinary(bin: *const u8, length: i32, base: *const UCollator, status: *mut UErrorCode) -> *mut UCollator;
    fn unorm2_getNFCInstance(pErrorCode: *mut UErrorCode) -> *const UNormalizer2;
    fn unorm2_getNFDInstance(pErrorCode: *mut UErrorCode) -> *const UNormalizer2;
    fn unorm2_getNFKCInstance(pErrorCode: *mut UErrorCode) -> *const UNormalizer2;
    fn unorm2_getNFKDInstance(pErrorCode: *mut UErrorCode) -> *const UNormalizer2;
    fn unorm2_getNFKCCasefoldInstance(pErrorCode: *mut UErrorCode) -> *const UNormalizer2;
    fn unorm2_getNFKCSimpleCasefoldInstance(pErrorCode: *mut UErrorCode) -> *const UNormalizer2;
    fn unorm2_getInstance(packageName: *const c_char, name: *const c_char, mode: i32, pErrorCode: *mut UErrorCode) -> *const UNormalizer2;
    fn unorm2_openFiltered(norm2: *const UNormalizer2, filterSet: *const USet, pErrorCode: *mut UErrorCode) -> *mut UNormalizer2;
    fn unorm2_close(norm2: *mut UNormalizer2);
    fn unorm2_normalize(norm2: *const UNormalizer2, src: *const UChar, length: i32, dest: *mut UChar, capacity: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn unorm2_normalizeSecondAndAppend(norm2: *const UNormalizer2, first: *mut UChar, firstLength: i32, firstCapacity: i32, second: *const UChar, secondLength: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn unorm2_append(norm2: *const UNormalizer2, first: *mut UChar, firstLength: i32, firstCapacity: i32, second: *const UChar, secondLength: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn unorm2_getDecomposition(norm2: *const UNormalizer2, c: UChar32, decomposition: *mut UChar, capacity: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn unorm2_getRawDecomposition(norm2: *const UNormalizer2, c: UChar32, decomposition: *mut UChar, capacity: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn unorm2_composePair(norm2: *const UNormalizer2, a: UChar32, b: UChar32) -> UChar32;
    fn unorm2_getCombiningClass(norm2: *const UNormalizer2, c: UChar32) -> u8;
    fn unorm2_isNormalized(norm2: *const UNormalizer2, s: *const UChar, length: i32, pErrorCode: *mut UErrorCode) -> UBool;
    fn unorm2_quickCheck(norm2: *const UNormalizer2, s: *const UChar, length: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn unorm2_spanQuickCheckYes(norm2: *const UNormalizer2, s: *const UChar, length: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn unorm2_hasBoundaryBefore(norm2: *const UNormalizer2, c: UChar32) -> UBool;
    fn unorm2_hasBoundaryAfter(norm2: *const UNormalizer2, c: UChar32) -> UBool;
    fn unorm2_isInert(norm2: *const UNormalizer2, c: UChar32) -> UBool;
    fn unorm_compare(s1: *const UChar, length1: i32, s2: *const UChar, length2: i32, options: u32, pErrorCode: *mut UErrorCode) -> i32;
    fn uidna_openUTS46(options: u32, pErrorCode: *mut UErrorCode) -> *mut UIDNA;
    fn uidna_close(idna: *mut UIDNA);
    fn uidna_labelToASCII(idna: *const UIDNA, label: *const UChar, length: i32, dest: *mut UChar, capacity: i32, pInfo: *mut UIDNAInfo, pErrorCode: *mut UErrorCode) -> i32;
    fn uidna_labelToUnicode(idna: *const UIDNA, label: *const UChar, length: i32, dest: *mut UChar, capacity: i32, pInfo: *mut UIDNAInfo, pErrorCode: *mut UErrorCode) -> i32;
    fn uidna_nameToASCII(idna: *const UIDNA, name: *const UChar, length: i32, dest: *mut UChar, capacity: i32, pInfo: *mut UIDNAInfo, pErrorCode: *mut UErrorCode) -> i32;
    fn uidna_nameToUnicode(idna: *const UIDNA, name: *const UChar, length: i32, dest: *mut UChar, capacity: i32, pInfo: *mut UIDNAInfo, pErrorCode: *mut UErrorCode) -> i32;
    fn uidna_labelToASCII_UTF8(idna: *const UIDNA, label: *const c_char, length: i32, dest: *mut c_char, capacity: i32, pInfo: *mut UIDNAInfo, pErrorCode: *mut UErrorCode) -> i32;
    fn uidna_labelToUnicodeUTF8(idna: *const UIDNA, label: *const c_char, length: i32, dest: *mut c_char, capacity: i32, pInfo: *mut UIDNAInfo, pErrorCode: *mut UErrorCode) -> i32;
    fn uidna_nameToASCII_UTF8(idna: *const UIDNA, name: *const c_char, length: i32, dest: *mut c_char, capacity: i32, pInfo: *mut UIDNAInfo, pErrorCode: *mut UErrorCode) -> i32;
    fn uidna_nameToUnicodeUTF8(idna: *const UIDNA, name: *const c_char, length: i32, dest: *mut c_char, capacity: i32, pInfo: *mut UIDNAInfo, pErrorCode: *mut UErrorCode) -> i32;
    fn ubrk_open(type_: i32, locale: *const c_char, text: *const UChar, textLength: i32, status: *mut UErrorCode) -> *mut UBreakIterator;
    fn ubrk_openRules(rules: *const UChar, rulesLength: i32, text: *const UChar, textLength: i32, parseErr: *mut UParseError, status: *mut UErrorCode) -> *mut UBreakIterator;
    fn ubrk_openBinaryRules(binaryRules: *const u8, rulesLength: i32, text: *const UChar, textLength: i32, status: *mut UErrorCode) -> *mut UBreakIterator;
    fn ubrk_clone(bi: *const UBreakIterator, status: *mut UErrorCode) -> *mut UBreakIterator;
    fn ubrk_close(bi: *mut UBreakIterator);
    fn ubrk_setText(bi: *mut UBreakIterator, text: *const UChar, textLength: i32, status: *mut UErrorCode);
    fn ubrk_setUText(bi: *mut UBreakIterator, text: *mut UText, status: *mut UErrorCode);
    fn ubrk_current(bi: *const UBreakIterator) -> i32;
    fn ubrk_next(bi: *mut UBreakIterator) -> i32;
    fn ubrk_previous(bi: *mut UBreakIterator) -> i32;
    fn ubrk_first(bi: *mut UBreakIterator) -> i32;
    fn ubrk_last(bi: *mut UBreakIterator) -> i32;
    fn ubrk_preceding(bi: *mut UBreakIterator, offset: i32) -> i32;
    fn ubrk_following(bi: *mut UBreakIterator, offset: i32) -> i32;
    fn ubrk_getAvailable(index: i32) -> *const c_char;
    fn ubrk_countAvailable() -> i32;
    fn ubrk_isBoundary(bi: *mut UBreakIterator, offset: i32) -> UBool;
    fn ubrk_getRuleStatus(bi: *mut UBreakIterator) -> i32;
    fn ubrk_getRuleStatusVec(bi: *mut UBreakIterator, fillInVec: *mut i32, capacity: i32, status: *mut UErrorCode) -> i32;
    fn ubrk_getLocaleByType(bi: *const UBreakIterator, type_: i32, status: *mut UErrorCode) -> *const c_char;
    fn ubrk_refreshUText(bi: *mut UBreakIterator, text: *mut UText, status: *mut UErrorCode);
    fn ubrk_getBinaryRules(bi: *mut UBreakIterator, binaryRules: *mut u8, rulesCapacity: i32, status: *mut UErrorCode) -> i32;
    fn utrans_openU(id: *const UChar, idLength: i32, dir: i32, rules: *const UChar, rulesLength: i32, parseError: *mut UParseError, pErrorCode: *mut UErrorCode) -> *mut UTransliterator;
    fn utrans_openInverse(trans: *const UTransliterator, status: *mut UErrorCode) -> *mut UTransliterator;
    fn utrans_clone(trans: *const UTransliterator, status: *mut UErrorCode) -> *mut UTransliterator;
    fn utrans_close(trans: *mut UTransliterator);
    fn utrans_getUnicodeID(trans: *const UTransliterator, resultLength: *mut i32) -> *const UChar;
    fn utrans_register(adoptedTrans: *mut UTransliterator, status: *mut UErrorCode);
    fn utrans_unregisterID(id: *const UChar, idLength: i32);
    fn utrans_setFilter(trans: *mut UTransliterator, filterPattern: *const UChar, filterPatternLen: i32, status: *mut UErrorCode);
    fn utrans_countAvailableIDs() -> i32;
    fn utrans_openIDs(pErrorCode: *mut UErrorCode) -> *mut UEnumeration;
    fn utrans_transUChars(trans: *const UTransliterator, text: *mut UChar, textLength: *mut i32, textCapacity: i32, start: i32, limit: *mut i32, status: *mut UErrorCode);
    fn utrans_transIncrementalUChars(trans: *const UTransliterator, text: *mut UChar, textLength: *mut i32, textCapacity: i32, pos: *mut UTransPosition, status: *mut UErrorCode);
    fn utrans_toRules(trans: *const UTransliterator, escapeUnprintable: UBool, result: *mut UChar, resultLength: i32, status: *mut UErrorCode) -> i32;
    fn utrans_getSourceSet(trans: *const UTransliterator, ignoreFilter: UBool, fillIn: *mut USet, status: *mut UErrorCode) -> *mut USet;
    fn ures_open(packageName: *const c_char, locale: *const c_char, status: *mut UErrorCode) -> *mut UResourceBundle;
    fn ures_openDirect(packageName: *const c_char, locale: *const c_char, status: *mut UErrorCode) -> *mut UResourceBundle;
    fn ures_openU(packageName: *const UChar, locale: *const c_char, status: *mut UErrorCode) -> *mut UResourceBundle;
    fn ures_close(resourceBundle: *mut UResourceBundle);
    fn ures_getVersion(resB: *const UResourceBundle, versionInfo: *mut u8);
    fn ures_getLocaleByType(resourceBundle: *const UResourceBundle, type_: i32, status: *mut UErrorCode) -> *const c_char;
    fn ures_openFillIn(r: *mut UResourceBundle, packageName: *const c_char, localeID: *const c_char, status: *mut UErrorCode);
    fn ures_getString(resourceBundle: *const UResourceBundle, len: *mut i32, status: *mut UErrorCode) -> *const UChar;
    fn ures_getUTF8String(resB: *const UResourceBundle, dest: *mut c_char, length: *mut i32, forceCopy: UBool, status: *mut UErrorCode) -> *const c_char;
    fn ures_getBinary(resourceBundle: *const UResourceBundle, len: *mut i32, status: *mut UErrorCode) -> *const u8;
    fn ures_getIntVector(resourceBundle: *const UResourceBundle, len: *mut i32, status: *mut UErrorCode) -> *const i32;
    fn ures_getUInt(resourceBundle: *const UResourceBundle, status: *mut UErrorCode) -> u32;
    fn ures_getInt(resourceBundle: *const UResourceBundle, status: *mut UErrorCode) -> i32;
    fn ures_getSize(resourceBundle: *const UResourceBundle) -> i32;
    fn ures_getType(resourceBundle: *const UResourceBundle) -> i32;
    fn ures_getKey(resourceBundle: *const UResourceBundle) -> *const c_char;
    fn ures_resetIterator(resourceBundle: *mut UResourceBundle);
    fn ures_hasNext(resourceBundle: *const UResourceBundle) -> UBool;
    fn ures_getNextResource(resourceBundle: *mut UResourceBundle, fillIn: *mut UResourceBundle, status: *mut UErrorCode) -> *mut UResourceBundle;
    fn ures_getNextString(resourceBundle: *mut UResourceBundle, len: *mut i32, key: *mut *const c_char, status: *mut UErrorCode) -> *const UChar;
    fn ures_getByIndex(resourceBundle: *const UResourceBundle, indexR: i32, fillIn: *mut UResourceBundle, status: *mut UErrorCode) -> *mut UResourceBundle;
    fn ures_getStringByIndex(resourceBundle: *const UResourceBundle, indexS: i32, len: *mut i32, status: *mut UErrorCode) -> *const UChar;
    fn ures_getUTF8StringByIndex(resB: *const UResourceBundle, stringIndex: i32, dest: *mut c_char, pLength: *mut i32, forceCopy: UBool, status: *mut UErrorCode) -> *const c_char;
    fn ures_getByKey(resourceBundle: *const UResourceBundle, key: *const c_char, fillIn: *mut UResourceBundle, status: *mut UErrorCode) -> *mut UResourceBundle;
    fn ures_getStringByKey(resB: *const UResourceBundle, key: *const c_char, len: *mut i32, status: *mut UErrorCode) -> *const UChar;
    fn ures_getUTF8StringByKey(resB: *const UResourceBundle, key: *const c_char, dest: *mut c_char, pLength: *mut i32, forceCopy: UBool, status: *mut UErrorCode) -> *const c_char;
    fn ures_openAvailableLocales(packageName: *const c_char, status: *mut UErrorCode) -> *mut UEnumeration;
    fn uspoof_open(status: *mut UErrorCode) -> *mut USpoofChecker;
    fn uspoof_openFromSerialized(data: *const c_void, length: i32, pActualLength: *mut i32, pErrorCode: *mut UErrorCode) -> *mut USpoofChecker;
    fn uspoof_openFromSource(confusables: *const c_char, confusablesLen: i32, confusablesWholeScript: *const c_char, confusablesWholeScriptLen: i32, errType: *mut i32, pe: *mut UParseError, status: *mut UErrorCode) -> *mut USpoofChecker;
    fn uspoof_close(sc: *mut USpoofChecker);
    fn uspoof_clone(sc: *const USpoofChecker, status: *mut UErrorCode) -> *mut USpoofChecker;
    fn uspoof_setChecks(sc: *mut USpoofChecker, checks: i32, status: *mut UErrorCode);
    fn uspoof_getChecks(sc: *const USpoofChecker, status: *mut UErrorCode) -> i32;
    fn uspoof_setRestrictionLevel(sc: *mut USpoofChecker, restrictionLevel: i32);
    fn uspoof_getRestrictionLevel(sc: *const USpoofChecker) -> i32;
    fn uspoof_setAllowedLocales(sc: *mut USpoofChecker, localesList: *const c_char, status: *mut UErrorCode);
    fn uspoof_getAllowedLocales(sc: *mut USpoofChecker, status: *mut UErrorCode) -> *const c_char;
    fn uspoof_setAllowedChars(sc: *mut USpoofChecker, chars: *const USet, status: *mut UErrorCode);
    fn uspoof_getAllowedChars(sc: *const USpoofChecker, status: *mut UErrorCode) -> *const USet;
    fn uspoof_check(sc: *const USpoofChecker, id: *const UChar, length: i32, position: *mut i32, status: *mut UErrorCode) -> i32;
    fn uspoof_checkUTF8(sc: *const USpoofChecker, id: *const c_char, length: i32, position: *mut i32, status: *mut UErrorCode) -> i32;
    fn uspoof_check2(sc: *const USpoofChecker, id: *const UChar, length: i32, checkResult: *mut USpoofCheckResult, status: *mut UErrorCode) -> i32;
    fn uspoof_check2UTF8(sc: *const USpoofChecker, id: *const c_char, length: i32, checkResult: *mut USpoofCheckResult, status: *mut UErrorCode) -> i32;
    fn uspoof_openCheckResult(status: *mut UErrorCode) -> *mut USpoofCheckResult;
    fn uspoof_closeCheckResult(checkResult: *mut USpoofCheckResult);
    fn uspoof_getCheckResultChecks(checkResult: *const USpoofCheckResult, status: *mut UErrorCode) -> i32;
    fn uspoof_getCheckResultRestrictionLevel(checkResult: *const USpoofCheckResult, status: *mut UErrorCode) -> i32;
    fn uspoof_getCheckResultNumerics(checkResult: *const USpoofCheckResult, status: *mut UErrorCode) -> *const USet;
    fn uspoof_areConfusable(sc: *const USpoofChecker, id1: *const UChar, length1: i32, id2: *const UChar, length2: i32, status: *mut UErrorCode) -> i32;
    fn uspoof_areBidiConfusable(sc: *const USpoofChecker, direction: i32, id1: *const UChar, length1: i32, id2: *const UChar, length2: i32, status: *mut UErrorCode) -> u32;
    fn uspoof_areConfusableUTF8(sc: *const USpoofChecker, id1: *const c_char, length1: i32, id2: *const c_char, length2: i32, status: *mut UErrorCode) -> i32;
    fn uspoof_areBidiConfusableUTF8(sc: *const USpoofChecker, direction: i32, id1: *const c_char, length1: i32, id2: *const c_char, length2: i32, status: *mut UErrorCode) -> u32;
    fn uspoof_getSkeleton(sc: *const USpoofChecker, type_: u32, id: *const UChar, length: i32, dest: *mut UChar, destCapacity: i32, status: *mut UErrorCode) -> i32;
    fn uspoof_getBidiSkeleton(sc: *const USpoofChecker, direction: i32, id: *const UChar, length: i32, dest: *mut UChar, destCapacity: i32, status: *mut UErrorCode) -> i32;
    fn uspoof_getSkeletonUTF8(sc: *const USpoofChecker, type_: u32, id: *const c_char, length: i32, dest: *mut c_char, destCapacity: i32, status: *mut UErrorCode) -> i32;
    fn uspoof_getBidiSkeletonUTF8(sc: *const USpoofChecker, direction: i32, id: *const c_char, length: i32, dest: *mut c_char, destCapacity: i32, status: *mut UErrorCode) -> i32;
    fn uspoof_getInclusionSet(status: *mut UErrorCode) -> *const USet;
    fn uspoof_getRecommendedSet(status: *mut UErrorCode) -> *const USet;
    fn uspoof_serialize(sc: *mut USpoofChecker, data: *mut c_void, capacity: i32, status: *mut UErrorCode) -> i32;
    fn ucnv_compareNames(name1: *const c_char, name2: *const c_char) -> i32;
    fn ucnv_open(converterName: *const c_char, err: *mut UErrorCode) -> *mut UConverter;
    fn ucnv_openU(name: *const UChar, err: *mut UErrorCode) -> *mut UConverter;
    fn ucnv_openCCSID(codepage: i32, platform: i32, err: *mut UErrorCode) -> *mut UConverter;
    fn ucnv_openPackage(packageName: *const c_char, converterName: *const c_char, err: *mut UErrorCode) -> *mut UConverter;
    fn ucnv_clone(cnv: *const UConverter, status: *mut UErrorCode) -> *mut UConverter;
    fn ucnv_close(converter: *mut UConverter);
    fn ucnv_getSubstChars(converter: *const UConverter, subChars: *mut c_char, len: *mut i8, err: *mut UErrorCode);
    fn ucnv_setSubstChars(converter: *mut UConverter, subChars: *const c_char, len: i8, err: *mut UErrorCode);
    fn ucnv_setSubstString(cnv: *mut UConverter, s: *const UChar, length: i32, err: *mut UErrorCode);
    fn ucnv_getInvalidChars(converter: *const UConverter, errBytes: *mut c_char, len: *mut i8, err: *mut UErrorCode);
    fn ucnv_getInvalidUChars(converter: *const UConverter, errUChars: *mut UChar, len: *mut i8, err: *mut UErrorCode);
    fn ucnv_reset(converter: *mut UConverter);
    fn ucnv_resetToUnicode(converter: *mut UConverter);
    fn ucnv_resetFromUnicode(converter: *mut UConverter);
    fn ucnv_getMaxCharSize(converter: *const UConverter) -> i8;
    fn ucnv_getMinCharSize(converter: *const UConverter) -> i8;
    fn ucnv_getDisplayName(converter: *const UConverter, displayLocale: *const c_char, displayName: *mut UChar, displayNameCapacity: i32, err: *mut UErrorCode) -> i32;
    fn ucnv_getName(converter: *const UConverter, err: *mut UErrorCode) -> *const c_char;
    fn ucnv_getCCSID(converter: *const UConverter, err: *mut UErrorCode) -> i32;
    fn ucnv_getPlatform(converter: *const UConverter, err: *mut UErrorCode) -> i32;
    fn ucnv_getType(converter: *const UConverter) -> i32;
    fn ucnv_getUnicodeSet(cnv: *const UConverter, setFillIn: *mut USet, whichSet: i32, pErrorCode: *mut UErrorCode);
    fn ucnv_getToUCallBack(converter: *const UConverter, action: *mut UConverterToUCallback, context: *mut *const c_void);
    fn ucnv_getFromUCallBack(converter: *const UConverter, action: *mut UConverterFromUCallback, context: *mut *const c_void);
    fn ucnv_setToUCallBack(converter: *mut UConverter, newAction: UConverterToUCallback, newContext: *const c_void, oldAction: *mut UConverterToUCallback, oldContext: *mut *const c_void, err: *mut UErrorCode);
    fn ucnv_setFromUCallBack(converter: *mut UConverter, newAction: UConverterFromUCallback, newContext: *const c_void, oldAction: *mut UConverterFromUCallback, oldContext: *mut *const c_void, err: *mut UErrorCode);
    fn ucnv_fromUnicode(converter: *mut UConverter, target: *mut *mut c_char, targetLimit: *const c_char, source: *mut *const UChar, sourceLimit: *const UChar, offsets: *mut i32, flush: UBool, err: *mut UErrorCode);
    fn ucnv_toUnicode(converter: *mut UConverter, target: *mut *mut UChar, targetLimit: *const UChar, source: *mut *const c_char, sourceLimit: *const c_char, offsets: *mut i32, flush: UBool, err: *mut UErrorCode);
    fn ucnv_fromUChars(cnv: *mut UConverter, dest: *mut c_char, destCapacity: i32, src: *const UChar, srcLength: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn ucnv_toUChars(cnv: *mut UConverter, dest: *mut UChar, destCapacity: i32, src: *const c_char, srcLength: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn ucnv_getNextUChar(converter: *mut UConverter, source: *mut *const c_char, sourceLimit: *const c_char, err: *mut UErrorCode) -> UChar32;
    fn ucnv_convertEx(targetCnv: *mut UConverter, sourceCnv: *mut UConverter, target: *mut *mut c_char, targetLimit: *const c_char, source: *mut *const c_char, sourceLimit: *const c_char, pivotStart: *mut UChar, pivotSource: *mut *mut UChar, pivotTarget: *mut *mut UChar, pivotLimit: *const UChar, reset: UBool, flush: UBool, pErrorCode: *mut UErrorCode);
    fn ucnv_convert(toConverterName: *const c_char, fromConverterName: *const c_char, target: *mut c_char, targetCapacity: i32, source: *const c_char, sourceLength: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn ucnv_toAlgorithmic(algorithmicType: i32, cnv: *mut UConverter, target: *mut c_char, targetCapacity: i32, source: *const c_char, sourceLength: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn ucnv_fromAlgorithmic(cnv: *mut UConverter, algorithmicType: i32, target: *mut c_char, targetCapacity: i32, source: *const c_char, sourceLength: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn ucnv_flushCache() -> i32;
    fn ucnv_countAvailable() -> i32;
    fn ucnv_getAvailableName(n: i32) -> *const c_char;
    fn ucnv_openAllNames(pErrorCode: *mut UErrorCode) -> *mut UEnumeration;
    fn ucnv_countAliases(alias: *const c_char, pErrorCode: *mut UErrorCode) -> u16;
    fn ucnv_getAlias(alias: *const c_char, n: u16, pErrorCode: *mut UErrorCode) -> *const c_char;
    fn ucnv_getAliases(alias: *const c_char, aliases: *mut *const c_char, pErrorCode: *mut UErrorCode);
    fn ucnv_openStandardNames(convName: *const c_char, standard: *const c_char, pErrorCode: *mut UErrorCode) -> *mut UEnumeration;
    fn ucnv_countStandards() -> u16;
    fn ucnv_getStandard(n: u16, pErrorCode: *mut UErrorCode) -> *const c_char;
    fn ucnv_getStandardName(name: *const c_char, standard: *const c_char, pErrorCode: *mut UErrorCode) -> *const c_char;
    fn ucnv_getCanonicalName(alias: *const c_char, standard: *const c_char, pErrorCode: *mut UErrorCode) -> *const c_char;
    fn ucnv_getDefaultName() -> *const c_char;
    fn ucnv_setDefaultName(name: *const c_char);
    fn ucnv_fixFileSeparator(cnv: *const UConverter, source: *mut UChar, sourceLen: i32);
    fn ucnv_isAmbiguous(cnv: *const UConverter) -> UBool;
    fn ucnv_setFallback(cnv: *mut UConverter, usesFallback: UBool);
    fn ucnv_usesFallback(cnv: *const UConverter) -> UBool;
    fn ucnv_detectUnicodeSignature(source: *const c_char, sourceLength: i32, signatureLength: *mut i32, pErrorCode: *mut UErrorCode) -> *const c_char;
    fn ucnv_fromUCountPending(cnv: *const UConverter, status: *mut UErrorCode) -> i32;
    fn ucnv_toUCountPending(cnv: *const UConverter, status: *mut UErrorCode) -> i32;
    fn ucnv_isFixedWidth(cnv: *mut UConverter, status: *mut UErrorCode) -> UBool;
    fn ucnv_cbFromUWriteBytes(args: *mut UConverterFromUnicodeArgs, source: *const c_char, length: i32, offsetIndex: i32, err: *mut UErrorCode);
    fn ucnv_cbFromUWriteSub(args: *mut UConverterFromUnicodeArgs, offsetIndex: i32, err: *mut UErrorCode);
    fn ucnv_cbFromUWriteUChars(args: *mut UConverterFromUnicodeArgs, source: *mut *const UChar, sourceLimit: *const UChar, offsetIndex: i32, err: *mut UErrorCode);
    fn ucnv_cbToUWriteUChars(args: *mut UConverterToUnicodeArgs, source: *const UChar, length: i32, offsetIndex: i32, err: *mut UErrorCode);
    fn ucnv_cbToUWriteSub(args: *mut UConverterToUnicodeArgs, offsetIndex: i32, err: *mut UErrorCode);
    fn udatpg_open(locale: *const c_char, pErrorCode: *mut UErrorCode) -> *mut UDateTimePatternGenerator;
    fn udatpg_openEmpty(pErrorCode: *mut UErrorCode) -> *mut UDateTimePatternGenerator;
    fn udatpg_close(dtpg: *mut UDateTimePatternGenerator);
    fn udatpg_clone(dtpg: *const UDateTimePatternGenerator, pErrorCode: *mut UErrorCode) -> *mut UDateTimePatternGenerator;
    fn udatpg_getBestPattern(dtpg: *mut UDateTimePatternGenerator, skeleton: *const UChar, length: i32, bestPattern: *mut UChar, capacity: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn udatpg_getBestPatternWithOptions(dtpg: *mut UDateTimePatternGenerator, skeleton: *const UChar, length: i32, options: i32, bestPattern: *mut UChar, capacity: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn udatpg_getSkeleton(unusedDtpg: *mut UDateTimePatternGenerator, pattern: *const UChar, length: i32, skeleton: *mut UChar, capacity: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn udatpg_getBaseSkeleton(unusedDtpg: *mut UDateTimePatternGenerator, pattern: *const UChar, length: i32, baseSkeleton: *mut UChar, capacity: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn udatpg_addPattern(dtpg: *mut UDateTimePatternGenerator, pattern: *const UChar, patternLength: i32, override_: UBool, conflictingPattern: *mut UChar, capacity: i32, pLength: *mut i32, pErrorCode: *mut UErrorCode) -> i32;
    fn udatpg_setAppendItemFormat(dtpg: *mut UDateTimePatternGenerator, field: i32, value: *const UChar, length: i32);
    fn udatpg_getAppendItemFormat(dtpg: *const UDateTimePatternGenerator, field: i32, pLength: *mut i32) -> *const UChar;
    fn udatpg_setAppendItemName(dtpg: *mut UDateTimePatternGenerator, field: i32, value: *const UChar, length: i32);
    fn udatpg_getAppendItemName(dtpg: *const UDateTimePatternGenerator, field: i32, pLength: *mut i32) -> *const UChar;
    fn udatpg_getFieldDisplayName(dtpg: *const UDateTimePatternGenerator, field: i32, width: i32, fieldName: *mut UChar, capacity: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn udatpg_setDateTimeFormat(dtpg: *const UDateTimePatternGenerator, dtFormat: *const UChar, length: i32);
    fn udatpg_getDateTimeFormat(dtpg: *const UDateTimePatternGenerator, pLength: *mut i32) -> *const UChar;
    fn udatpg_setDateTimeFormatForStyle(udtpg: *mut UDateTimePatternGenerator, style: i32, dateTimeFormat: *const UChar, length: i32, pErrorCode: *mut UErrorCode);
    fn udatpg_getDateTimeFormatForStyle(udtpg: *const UDateTimePatternGenerator, style: i32, pLength: *mut i32, pErrorCode: *mut UErrorCode) -> *const UChar;
    fn udatpg_setDecimal(dtpg: *mut UDateTimePatternGenerator, decimal: *const UChar, length: i32);
    fn udatpg_getDecimal(dtpg: *const UDateTimePatternGenerator, pLength: *mut i32) -> *const UChar;
    fn udatpg_replaceFieldTypes(dtpg: *mut UDateTimePatternGenerator, pattern: *const UChar, patternLength: i32, skeleton: *const UChar, skeletonLength: i32, dest: *mut UChar, destCapacity: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn udatpg_replaceFieldTypesWithOptions(dtpg: *mut UDateTimePatternGenerator, pattern: *const UChar, patternLength: i32, skeleton: *const UChar, skeletonLength: i32, options: i32, dest: *mut UChar, destCapacity: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn udatpg_openSkeletons(dtpg: *const UDateTimePatternGenerator, pErrorCode: *mut UErrorCode) -> *mut UEnumeration;
    fn udatpg_openBaseSkeletons(dtpg: *const UDateTimePatternGenerator, pErrorCode: *mut UErrorCode) -> *mut UEnumeration;
    fn udatpg_getPatternForSkeleton(dtpg: *const UDateTimePatternGenerator, skeleton: *const UChar, skeletonLength: i32, pLength: *mut i32) -> *const UChar;
    fn udatpg_getDefaultHourCycle(dtpg: *const UDateTimePatternGenerator, pErrorCode: *mut UErrorCode) -> i32;
    fn ulistfmt_open(locale: *const c_char, status: *mut UErrorCode) -> *mut UListFormatter;
    fn ulistfmt_openForType(locale: *const c_char, type_: i32, width: i32, status: *mut UErrorCode) -> *mut UListFormatter;
    fn ulistfmt_close(listfmt: *mut UListFormatter);
    fn ulistfmt_openResult(ec: *mut UErrorCode) -> *mut UFormattedList;
    fn ulistfmt_resultAsValue(uresult: *const UFormattedList, ec: *mut UErrorCode) -> *const UFormattedValue;
    fn ulistfmt_closeResult(uresult: *mut UFormattedList);
    fn u_hasBinaryProperty(c: UChar32, which: i32) -> UBool;
    fn u_stringHasBinaryProperty(s: *const UChar, length: i32, which: i32) -> UBool;
    fn u_getBinaryPropertySet(property: i32, pErrorCode: *mut UErrorCode) -> *const USet;
    fn u_isUAlphabetic(c: UChar32) -> UBool;
    fn u_isULowercase(c: UChar32) -> UBool;
    fn u_isUUppercase(c: UChar32) -> UBool;
    fn u_isUWhiteSpace(c: UChar32) -> UBool;
    fn u_getIntPropertyValue(c: UChar32, which: i32) -> i32;
    fn u_getIntPropertyMinValue(which: i32) -> i32;
    fn u_getIntPropertyMaxValue(which: i32) -> i32;
    fn u_getNumericValue(c: UChar32) -> f64;
    fn u_islower(c: UChar32) -> UBool;
    fn u_isupper(c: UChar32) -> UBool;
    fn u_istitle(c: UChar32) -> UBool;
    fn u_isdigit(c: UChar32) -> UBool;
    fn u_isalpha(c: UChar32) -> UBool;
    fn u_isalnum(c: UChar32) -> UBool;
    fn u_isxdigit(c: UChar32) -> UBool;
    fn u_ispunct(c: UChar32) -> UBool;
    fn u_isgraph(c: UChar32) -> UBool;
    fn u_isblank(c: UChar32) -> UBool;
    fn u_isdefined(c: UChar32) -> UBool;
    fn u_isspace(c: UChar32) -> UBool;
    fn u_isJavaSpaceChar(c: UChar32) -> UBool;
    fn u_isWhitespace(c: UChar32) -> UBool;
    fn u_iscntrl(c: UChar32) -> UBool;
    fn u_isISOControl(c: UChar32) -> UBool;
    fn u_isprint(c: UChar32) -> UBool;
    fn u_isbase(c: UChar32) -> UBool;
    fn u_charDirection(c: UChar32) -> i32;
    fn u_isMirrored(c: UChar32) -> UBool;
    fn u_charMirror(c: UChar32) -> UChar32;
    fn u_getBidiPairedBracket(c: UChar32) -> UChar32;
    fn u_charType(c: UChar32) -> i8;
    fn u_enumCharTypes(enumRange: UCharEnumTypeRange, context: *const c_void);
    fn u_getCombiningClass(c: UChar32) -> u8;
    fn u_charDigitValue(c: UChar32) -> i32;
    fn ublock_getCode(c: UChar32) -> i32;
    fn u_charName(code: UChar32, nameChoice: i32, buffer: *mut c_char, bufferLength: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn u_charFromName(nameChoice: i32, name: *const c_char, pErrorCode: *mut UErrorCode) -> UChar32;
    fn u_enumCharNames(start: UChar32, limit: UChar32, fn_: UEnumCharNamesFn, context: *mut c_void, nameChoice: i32, pErrorCode: *mut UErrorCode);
    fn u_getPropertyName(property: i32, nameChoice: i32) -> *const c_char;
    fn u_getPropertyEnum(alias: *const c_char) -> i32;
    fn u_getPropertyValueName(property: i32, value: i32, nameChoice: i32) -> *const c_char;
    fn u_getPropertyValueEnum(property: i32, alias: *const c_char) -> i32;
    fn u_isIDStart(c: UChar32) -> UBool;
    fn u_isIDPart(c: UChar32) -> UBool;
    fn u_isIDIgnorable(c: UChar32) -> UBool;
    fn u_isJavaIDStart(c: UChar32) -> UBool;
    fn u_isJavaIDPart(c: UChar32) -> UBool;
    fn u_tolower(c: UChar32) -> UChar32;
    fn u_toupper(c: UChar32) -> UChar32;
    fn u_totitle(c: UChar32) -> UChar32;
    fn u_foldCase(c: UChar32, options: u32) -> UChar32;
    fn u_digit(ch: UChar32, radix: i8) -> i32;
    fn u_forDigit(digit: i32, radix: i8) -> UChar32;
    fn u_charAge(c: UChar32, versionArray: *mut u8);
    fn u_getUnicodeVersion(versionArray: *mut u8);
    fn u_getFC_NFKC_Closure(c: UChar32, dest: *mut UChar, destCapacity: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn u_strlen(s: *const UChar) -> i32;
    fn u_countChar32(s: *const UChar, length: i32) -> i32;
    fn u_strHasMoreChar32Than(s: *const UChar, length: i32, number: i32) -> UBool;
    fn u_strcat(dst: *mut UChar, src: *const UChar) -> *mut UChar;
    fn u_strncat(dst: *mut UChar, src: *const UChar, n: i32) -> *mut UChar;
    fn u_strstr(s: *const UChar, substring: *const UChar) -> *mut UChar;
    fn u_strFindFirst(s: *const UChar, length: i32, substring: *const UChar, subLength: i32) -> *mut UChar;
    fn u_strchr(s: *const UChar, c: UChar) -> *mut UChar;
    fn u_strchr32(s: *const UChar, c: UChar32) -> *mut UChar;
    fn u_strrstr(s: *const UChar, substring: *const UChar) -> *mut UChar;
    fn u_strFindLast(s: *const UChar, length: i32, substring: *const UChar, subLength: i32) -> *mut UChar;
    fn u_strrchr(s: *const UChar, c: UChar) -> *mut UChar;
    fn u_strrchr32(s: *const UChar, c: UChar32) -> *mut UChar;
    fn u_strpbrk(string: *const UChar, matchSet: *const UChar) -> *mut UChar;
    fn u_strcspn(string: *const UChar, matchSet: *const UChar) -> i32;
    fn u_strspn(string: *const UChar, matchSet: *const UChar) -> i32;
    fn u_strtok_r(src: *mut UChar, delim: *const UChar, saveState: *mut *mut UChar) -> *mut UChar;
    fn u_strcmp(s1: *const UChar, s2: *const UChar) -> i32;
    fn u_strcmpCodePointOrder(s1: *const UChar, s2: *const UChar) -> i32;
    fn u_strCompare(s1: *const UChar, length1: i32, s2: *const UChar, length2: i32, codePointOrder: UBool) -> i32;
    fn u_strCaseCompare(s1: *const UChar, length1: i32, s2: *const UChar, length2: i32, options: u32, pErrorCode: *mut UErrorCode) -> i32;
    fn u_strncmp(ucs1: *const UChar, ucs2: *const UChar, n: i32) -> i32;
    fn u_strncmpCodePointOrder(s1: *const UChar, s2: *const UChar, n: i32) -> i32;
    fn u_strcasecmp(s1: *const UChar, s2: *const UChar, options: u32) -> i32;
    fn u_strncasecmp(s1: *const UChar, s2: *const UChar, n: i32, options: u32) -> i32;
    fn u_memcasecmp(s1: *const UChar, s2: *const UChar, length: i32, options: u32) -> i32;
    fn u_strcpy(dst: *mut UChar, src: *const UChar) -> *mut UChar;
    fn u_strncpy(dst: *mut UChar, src: *const UChar, n: i32) -> *mut UChar;
    fn u_uastrcpy(dst: *mut UChar, src: *const c_char) -> *mut UChar;
    fn u_uastrncpy(dst: *mut UChar, src: *const c_char, n: i32) -> *mut UChar;
    fn u_austrcpy(dst: *mut c_char, src: *const UChar) -> *mut c_char;
    fn u_austrncpy(dst: *mut c_char, src: *const UChar, n: i32) -> *mut c_char;
    fn u_memcpy(dest: *mut UChar, src: *const UChar, count: i32) -> *mut UChar;
    fn u_memmove(dest: *mut UChar, src: *const UChar, count: i32) -> *mut UChar;
    fn u_memset(dest: *mut UChar, c: UChar, count: i32) -> *mut UChar;
    fn u_memcmp(buf1: *const UChar, buf2: *const UChar, count: i32) -> i32;
    fn u_memcmpCodePointOrder(s1: *const UChar, s2: *const UChar, count: i32) -> i32;
    fn u_memchr(s: *const UChar, c: UChar, count: i32) -> *mut UChar;
    fn u_memchr32(s: *const UChar, c: UChar32, count: i32) -> *mut UChar;
    fn u_memrchr(s: *const UChar, c: UChar, count: i32) -> *mut UChar;
    fn u_memrchr32(s: *const UChar, c: UChar32, count: i32) -> *mut UChar;
    fn u_unescape(src: *const c_char, dest: *mut UChar, destCapacity: i32) -> i32;
    fn u_unescapeAt(charAt: i32, offset: *mut i32, length: i32, context: *mut c_void) -> UChar32;
    fn u_strToUpper(dest: *mut UChar, destCapacity: i32, src: *const UChar, srcLength: i32, locale: *const c_char, pErrorCode: *mut UErrorCode) -> i32;
    fn u_strToLower(dest: *mut UChar, destCapacity: i32, src: *const UChar, srcLength: i32, locale: *const c_char, pErrorCode: *mut UErrorCode) -> i32;
    fn u_strToTitle(dest: *mut UChar, destCapacity: i32, src: *const UChar, srcLength: i32, titleIter: *mut UBreakIterator, locale: *const c_char, pErrorCode: *mut UErrorCode) -> i32;
    fn u_strFoldCase(dest: *mut UChar, destCapacity: i32, src: *const UChar, srcLength: i32, options: u32, pErrorCode: *mut UErrorCode) -> i32;
    fn u_strToUTF8(dest: *mut c_char, destCapacity: i32, pDestLength: *mut i32, src: *const UChar, srcLength: i32, pErrorCode: *mut UErrorCode) -> *mut c_char;
    fn u_strFromUTF8(dest: *mut UChar, destCapacity: i32, pDestLength: *mut i32, src: *const c_char, srcLength: i32, pErrorCode: *mut UErrorCode) -> *mut UChar;
    fn u_strToUTF8WithSub(dest: *mut c_char, destCapacity: i32, pDestLength: *mut i32, src: *const UChar, srcLength: i32, subchar: UChar32, pNumSubstitutions: *mut i32, pErrorCode: *mut UErrorCode) -> *mut c_char;
    fn u_strFromUTF8WithSub(dest: *mut UChar, destCapacity: i32, pDestLength: *mut i32, src: *const c_char, srcLength: i32, subchar: UChar32, pNumSubstitutions: *mut i32, pErrorCode: *mut UErrorCode) -> *mut UChar;
    fn u_strFromUTF8Lenient(dest: *mut UChar, destCapacity: i32, pDestLength: *mut i32, src: *const c_char, srcLength: i32, pErrorCode: *mut UErrorCode) -> *mut UChar;
    fn u_strToUTF32(dest: *mut UChar32, destCapacity: i32, pDestLength: *mut i32, src: *const UChar, srcLength: i32, pErrorCode: *mut UErrorCode) -> *mut UChar32;
    fn u_strFromUTF32(dest: *mut UChar, destCapacity: i32, pDestLength: *mut i32, src: *const UChar32, srcLength: i32, pErrorCode: *mut UErrorCode) -> *mut UChar;
    fn u_strToUTF32WithSub(dest: *mut UChar32, destCapacity: i32, pDestLength: *mut i32, src: *const UChar, srcLength: i32, subchar: UChar32, pNumSubstitutions: *mut i32, pErrorCode: *mut UErrorCode) -> *mut UChar32;
    fn u_strFromUTF32WithSub(dest: *mut UChar, destCapacity: i32, pDestLength: *mut i32, src: *const UChar32, srcLength: i32, subchar: UChar32, pNumSubstitutions: *mut i32, pErrorCode: *mut UErrorCode) -> *mut UChar;
    fn u_strToJavaModifiedUTF8(dest: *mut c_char, destCapacity: i32, pDestLength: *mut i32, src: *const UChar, srcLength: i32, pErrorCode: *mut UErrorCode) -> *mut c_char;
    fn u_strFromJavaModifiedUTF8WithSub(dest: *mut UChar, destCapacity: i32, pDestLength: *mut i32, src: *const c_char, srcLength: i32, subchar: UChar32, pNumSubstitutions: *mut i32, pErrorCode: *mut UErrorCode) -> *mut UChar;
    fn uplrules_open(locale: *const c_char, status: *mut UErrorCode) -> *mut UPluralRules;
    fn uplrules_openForType(locale: *const c_char, type_: i32, status: *mut UErrorCode) -> *mut UPluralRules;
    fn uplrules_close(uplrules: *mut UPluralRules);
    fn uplrules_select(uplrules: *const UPluralRules, number: f64, keyword: *mut UChar, capacity: i32, status: *mut UErrorCode) -> i32;
    fn uplrules_selectFormatted(uplrules: *const UPluralRules, number: *const UFormattedNumber, keyword: *mut UChar, capacity: i32, status: *mut UErrorCode) -> i32;
    fn uplrules_selectForRange(uplrules: *const UPluralRules, urange: *const UFormattedNumberRange, keyword: *mut UChar, capacity: i32, status: *mut UErrorCode) -> i32;
    fn uplrules_selectWithFormat(uplrules: *const UPluralRules, number: f64, fmt: *const UNumberFormat, keyword: *mut UChar, capacity: i32, status: *mut UErrorCode) -> i32;
    fn uplrules_getKeywords(uplrules: *const UPluralRules, status: *mut UErrorCode) -> *mut UEnumeration;
    fn unumf_openForSkeletonAndLocale(skeleton: *const UChar, skeletonLen: i32, locale: *const c_char, ec: *mut UErrorCode) -> *mut UNumberFormatter;
    fn unumf_openForSkeletonAndLocaleWithError(skeleton: *const UChar, skeletonLen: i32, locale: *const c_char, perror: *mut UParseError, ec: *mut UErrorCode) -> *mut UNumberFormatter;
    fn unumf_formatInt(uformatter: *const UNumberFormatter, value: i64, uresult: *mut UFormattedNumber, ec: *mut UErrorCode);
    fn unumf_formatDouble(uformatter: *const UNumberFormatter, value: f64, uresult: *mut UFormattedNumber, ec: *mut UErrorCode);
    fn unumf_formatDecimal(uformatter: *const UNumberFormatter, value: *const c_char, valueLen: i32, uresult: *mut UFormattedNumber, ec: *mut UErrorCode);
    fn unumf_close(uformatter: *mut UNumberFormatter);
    fn unumrf_openForSkeletonWithCollapseAndIdentityFallback(skeleton: *const UChar, skeletonLen: i32, collapse: i32, identityFallback: i32, locale: *const c_char, perror: *mut UParseError, ec: *mut UErrorCode) -> *mut UNumberRangeFormatter;
    fn unumrf_openResult(ec: *mut UErrorCode) -> *mut UFormattedNumberRange;
    fn unumrf_formatDoubleRange(uformatter: *const UNumberRangeFormatter, first: f64, second: f64, uresult: *mut UFormattedNumberRange, ec: *mut UErrorCode);
    fn unumrf_formatDecimalRange(uformatter: *const UNumberRangeFormatter, first: *const c_char, firstLen: i32, second: *const c_char, secondLen: i32, uresult: *mut UFormattedNumberRange, ec: *mut UErrorCode);
    fn unumrf_resultAsValue(uresult: *const UFormattedNumberRange, ec: *mut UErrorCode) -> *const UFormattedValue;
    fn unumrf_resultGetIdentityResult(uresult: *const UFormattedNumberRange, ec: *mut UErrorCode) -> i32;
    fn unumrf_resultGetFirstDecimalNumber(uresult: *const UFormattedNumberRange, dest: *mut c_char, destCapacity: i32, ec: *mut UErrorCode) -> i32;
    fn unumrf_resultGetSecondDecimalNumber(uresult: *const UFormattedNumberRange, dest: *mut c_char, destCapacity: i32, ec: *mut UErrorCode) -> i32;
    fn unumrf_close(uformatter: *mut UNumberRangeFormatter);
    fn unumrf_closeResult(uresult: *mut UFormattedNumberRange);
    fn ureldatefmt_open(locale: *const c_char, nfToAdopt: *mut UNumberFormat, width: i32, capitalizationContext: i32, status: *mut UErrorCode) -> *mut URelativeDateTimeFormatter;
    fn ureldatefmt_close(reldatefmt: *mut URelativeDateTimeFormatter);
    fn ureldatefmt_openResult(ec: *mut UErrorCode) -> *mut i32;
    fn ureldatefmt_resultAsValue(ufrdt: *const i32, ec: *mut UErrorCode) -> *const UFormattedValue;
    fn ureldatefmt_closeResult(ufrdt: *mut i32);
    fn ureldatefmt_formatNumeric(reldatefmt: *const URelativeDateTimeFormatter, offset: f64, unit: i32, result: *mut UChar, resultCapacity: i32, status: *mut UErrorCode) -> i32;
    fn ureldatefmt_formatNumericToResult(reldatefmt: *const URelativeDateTimeFormatter, offset: f64, unit: i32, result: *mut i32, status: *mut UErrorCode);
    fn ureldatefmt_format(reldatefmt: *const URelativeDateTimeFormatter, offset: f64, unit: i32, result: *mut UChar, resultCapacity: i32, status: *mut UErrorCode) -> i32;
    fn ureldatefmt_formatToResult(reldatefmt: *const URelativeDateTimeFormatter, offset: f64, unit: i32, result: *mut i32, status: *mut UErrorCode);
    fn ureldatefmt_combineDateAndTime(reldatefmt: *const URelativeDateTimeFormatter, relativeDateString: *const UChar, relativeDateStringLen: i32, timeString: *const UChar, timeStringLen: i32, result: *mut UChar, resultCapacity: i32, status: *mut UErrorCode) -> i32;
    fn usearch_open(pattern: *const UChar, patternlength: i32, text: *const UChar, textlength: i32, locale: *const c_char, breakiter: *mut UBreakIterator, status: *mut UErrorCode) -> *mut UStringSearch;
    fn usearch_openFromCollator(pattern: *const UChar, patternlength: i32, text: *const UChar, textlength: i32, collator: *const UCollator, breakiter: *mut UBreakIterator, status: *mut UErrorCode) -> *mut UStringSearch;
    fn usearch_close(searchiter: *mut UStringSearch);
    fn usearch_setOffset(strsrch: *mut UStringSearch, position: i32, status: *mut UErrorCode);
    fn usearch_getOffset(strsrch: *const UStringSearch) -> i32;
    fn usearch_setAttribute(strsrch: *mut UStringSearch, attribute: i32, value: i32, status: *mut UErrorCode);
    fn usearch_getAttribute(strsrch: *const UStringSearch, attribute: i32) -> i32;
    fn usearch_getMatchedStart(strsrch: *const UStringSearch) -> i32;
    fn usearch_getMatchedLength(strsrch: *const UStringSearch) -> i32;
    fn usearch_getMatchedText(strsrch: *const UStringSearch, result: *mut UChar, resultCapacity: i32, status: *mut UErrorCode) -> i32;
    fn usearch_setBreakIterator(strsrch: *mut UStringSearch, breakiter: *mut UBreakIterator, status: *mut UErrorCode);
    fn usearch_getBreakIterator(strsrch: *const UStringSearch) -> *const UBreakIterator;
    fn usearch_setText(strsrch: *mut UStringSearch, text: *const UChar, textlength: i32, status: *mut UErrorCode);
    fn usearch_getText(strsrch: *const UStringSearch, length: *mut i32) -> *const UChar;
    fn usearch_getCollator(strsrch: *const UStringSearch) -> *mut UCollator;
    fn usearch_setCollator(strsrch: *mut UStringSearch, collator: *const UCollator, status: *mut UErrorCode);
    fn usearch_setPattern(strsrch: *mut UStringSearch, pattern: *const UChar, patternlength: i32, status: *mut UErrorCode);
    fn usearch_getPattern(strsrch: *const UStringSearch, length: *mut i32) -> *const UChar;
    fn usearch_first(strsrch: *mut UStringSearch, status: *mut UErrorCode) -> i32;
    fn usearch_following(strsrch: *mut UStringSearch, position: i32, status: *mut UErrorCode) -> i32;
    fn usearch_last(strsrch: *mut UStringSearch, status: *mut UErrorCode) -> i32;
    fn usearch_preceding(strsrch: *mut UStringSearch, position: i32, status: *mut UErrorCode) -> i32;
    fn usearch_next(strsrch: *mut UStringSearch, status: *mut UErrorCode) -> i32;
    fn usearch_previous(strsrch: *mut UStringSearch, status: *mut UErrorCode) -> i32;
    fn usearch_reset(strsrch: *mut UStringSearch);
    fn usearch_search(strsrch: *mut UStringSearch, startIdx: i32, matchStart: *mut i32, matchLimit: *mut i32, status: *mut UErrorCode) -> UBool;
    fn usearch_searchBackwards(strsrch: *mut UStringSearch, startIdx: i32, matchStart: *mut i32, matchLimit: *mut i32, status: *mut UErrorCode) -> UBool;
    fn uset_openEmpty() -> *mut USet;
    fn uset_open(start: UChar32, end: UChar32) -> *mut USet;
    fn uset_openPattern(pattern: *const UChar, patternLength: i32, ec: *mut UErrorCode) -> *mut USet;
    fn uset_openPatternOptions(pattern: *const UChar, patternLength: i32, options: u32, ec: *mut UErrorCode) -> *mut USet;
    fn uset_close(set: *mut USet);
    fn uset_clone(set: *const USet) -> *mut USet;
    fn uset_isFrozen(set: *const USet) -> UBool;
    fn uset_freeze(set: *mut USet);
    fn uset_cloneAsThawed(set: *const USet) -> *mut USet;
    fn uset_set(set: *mut USet, start: UChar32, end: UChar32);
    fn uset_applyPattern(set: *mut USet, pattern: *const UChar, patternLength: i32, options: u32, status: *mut UErrorCode) -> i32;
    fn uset_applyIntPropertyValue(set: *mut USet, prop: i32, value: i32, ec: *mut UErrorCode);
    fn uset_applyPropertyAlias(set: *mut USet, prop: *const UChar, propLength: i32, value: *const UChar, valueLength: i32, ec: *mut UErrorCode);
    fn uset_resemblesPattern(pattern: *const UChar, patternLength: i32, pos: i32) -> UBool;
    fn uset_toPattern(set: *const USet, result: *mut UChar, resultCapacity: i32, escapeUnprintable: UBool, ec: *mut UErrorCode) -> i32;
    fn uset_add(set: *mut USet, c: UChar32);
    fn uset_addAll(set: *mut USet, additionalSet: *const USet);
    fn uset_addRange(set: *mut USet, start: UChar32, end: UChar32);
    fn uset_addString(set: *mut USet, str_: *const UChar, strLen: i32);
    fn uset_addAllCodePoints(set: *mut USet, str_: *const UChar, strLen: i32);
    fn uset_remove(set: *mut USet, c: UChar32);
    fn uset_removeRange(set: *mut USet, start: UChar32, end: UChar32);
    fn uset_removeString(set: *mut USet, str_: *const UChar, strLen: i32);
    fn uset_removeAllCodePoints(set: *mut USet, str_: *const UChar, length: i32);
    fn uset_removeAll(set: *mut USet, removeSet: *const USet);
    fn uset_retain(set: *mut USet, start: UChar32, end: UChar32);
    fn uset_retainString(set: *mut USet, str_: *const UChar, length: i32);
    fn uset_retainAllCodePoints(set: *mut USet, str_: *const UChar, length: i32);
    fn uset_retainAll(set: *mut USet, retain: *const USet);
    fn uset_compact(set: *mut USet);
    fn uset_complement(set: *mut USet);
    fn uset_complementRange(set: *mut USet, start: UChar32, end: UChar32);
    fn uset_complementString(set: *mut USet, str_: *const UChar, length: i32);
    fn uset_complementAllCodePoints(set: *mut USet, str_: *const UChar, length: i32);
    fn uset_complementAll(set: *mut USet, complement: *const USet);
    fn uset_clear(set: *mut USet);
    fn uset_closeOver(set: *mut USet, attributes: i32);
    fn uset_removeAllStrings(set: *mut USet);
    fn uset_isEmpty(set: *const USet) -> UBool;
    fn uset_hasStrings(set: *const USet) -> UBool;
    fn uset_contains(set: *const USet, c: UChar32) -> UBool;
    fn uset_containsRange(set: *const USet, start: UChar32, end: UChar32) -> UBool;
    fn uset_containsString(set: *const USet, str_: *const UChar, strLen: i32) -> UBool;
    fn uset_indexOf(set: *const USet, c: UChar32) -> i32;
    fn uset_charAt(set: *const USet, charIndex: i32) -> UChar32;
    fn uset_size(set: *const USet) -> i32;
    fn uset_getRangeCount(set: *const USet) -> i32;
    fn uset_getItemCount(set: *const USet) -> i32;
    fn uset_getItem(set: *const USet, itemIndex: i32, start: *mut UChar32, end: *mut UChar32, str_: *mut UChar, strCapacity: i32, ec: *mut UErrorCode) -> i32;
    fn uset_containsAll(set1: *const USet, set2: *const USet) -> UBool;
    fn uset_containsAllCodePoints(set: *const USet, str_: *const UChar, strLen: i32) -> UBool;
    fn uset_containsNone(set1: *const USet, set2: *const USet) -> UBool;
    fn uset_containsSome(set1: *const USet, set2: *const USet) -> UBool;
    fn uset_span(set: *const USet, s: *const UChar, length: i32, spanCondition: i32) -> i32;
    fn uset_spanBack(set: *const USet, s: *const UChar, length: i32, spanCondition: i32) -> i32;
    fn uset_spanUTF8(set: *const USet, s: *const c_char, length: i32, spanCondition: i32) -> i32;
    fn uset_spanBackUTF8(set: *const USet, s: *const c_char, length: i32, spanCondition: i32) -> i32;
    fn uset_equals(set1: *const USet, set2: *const USet) -> UBool;
    fn uset_serialize(set: *const USet, dest: *mut u16, destCapacity: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn uscript_getCode(nameOrAbbrOrLocale: *const c_char, fillIn: *mut i32, capacity: i32, err: *mut UErrorCode) -> i32;
    fn uscript_getName(scriptCode: i32) -> *const c_char;
    fn uscript_getShortName(scriptCode: i32) -> *const c_char;
    fn uscript_getScript(codepoint: UChar32, err: *mut UErrorCode) -> i32;
    fn uscript_hasScript(c: UChar32, sc: i32) -> UBool;
    fn uscript_getScriptExtensions(c: UChar32, scripts: *mut i32, capacity: i32, errorCode: *mut UErrorCode) -> i32;
    fn uscript_getSampleString(script: i32, dest: *mut UChar, capacity: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn uscript_getUsage(script: i32) -> i32;
    fn uscript_isRightToLeft(script: i32) -> UBool;
    fn uscript_breaksBetweenLetters(script: i32) -> UBool;
    fn uscript_isCased(script: i32) -> UBool;
    fn ucurr_forLocale(locale: *const c_char, buff: *mut UChar, buffCapacity: i32, ec: *mut UErrorCode) -> i32;
    fn ucurr_register(isoCode: *const UChar, locale: *const c_char, status: *mut UErrorCode) -> i32;
    fn ucurr_unregister(key: i32, status: *mut UErrorCode) -> UBool;
    fn ucurr_getName(currency: *const UChar, locale: *const c_char, nameStyle: i32, isChoiceFormat: *mut UBool, len: *mut i32, ec: *mut UErrorCode) -> *const UChar;
    fn ucurr_getPluralName(currency: *const UChar, locale: *const c_char, isChoiceFormat: *mut UBool, pluralCount: *const c_char, len: *mut i32, ec: *mut UErrorCode) -> *const UChar;
    fn ucurr_getDefaultFractionDigits(currency: *const UChar, ec: *mut UErrorCode) -> i32;
    fn ucurr_getDefaultFractionDigitsForUsage(currency: *const UChar, usage: i32, ec: *mut UErrorCode) -> i32;
    fn ucurr_getRoundingIncrement(currency: *const UChar, ec: *mut UErrorCode) -> f64;
    fn ucurr_getRoundingIncrementForUsage(currency: *const UChar, usage: i32, ec: *mut UErrorCode) -> f64;
    fn ucurr_openISOCurrencies(currType: u32, pErrorCode: *mut UErrorCode) -> *mut UEnumeration;
    fn ucurr_isAvailable(isoCode: *const UChar, from: UDate, to: UDate, errorCode: *mut UErrorCode) -> UBool;
    fn ucurr_countCurrencies(locale: *const c_char, date: UDate, ec: *mut UErrorCode) -> i32;
    fn ucurr_forLocaleAndDate(locale: *const c_char, date: UDate, index: i32, buff: *mut UChar, buffCapacity: i32, ec: *mut UErrorCode) -> i32;
    fn ucurr_getKeywordValuesForLocale(key: *const c_char, locale: *const c_char, commonlyUsed: UBool, status: *mut UErrorCode) -> *mut UEnumeration;
    fn ucurr_getNumericCode(currency: *const UChar) -> i32;
    fn ufieldpositer_open(status: *mut UErrorCode) -> *mut UFieldPositionIterator;
    fn ufieldpositer_close(fpositer: *mut UFieldPositionIterator);
    fn ufieldpositer_next(fpositer: *mut UFieldPositionIterator, beginIndex: *mut i32, endIndex: *mut i32) -> i32;
    fn ufmtval_getString(ufmtval: *const UFormattedValue, pLength: *mut i32, ec: *mut UErrorCode) -> *const UChar;
    fn uldn_open(locale: *const c_char, dialectHandling: i32, pErrorCode: *mut UErrorCode) -> *mut ULocaleDisplayNames;
    fn uldn_close(ldn: *mut ULocaleDisplayNames);
    fn uldn_getLocale(ldn: *const ULocaleDisplayNames) -> *const c_char;
    fn uldn_getDialectHandling(ldn: *const ULocaleDisplayNames) -> i32;
    fn uldn_localeDisplayName(ldn: *const ULocaleDisplayNames, locale: *const c_char, result: *mut UChar, maxResultSize: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn uldn_languageDisplayName(ldn: *const ULocaleDisplayNames, lang: *const c_char, result: *mut UChar, maxResultSize: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn uldn_scriptDisplayName(ldn: *const ULocaleDisplayNames, script: *const c_char, result: *mut UChar, maxResultSize: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn uldn_scriptCodeDisplayName(ldn: *const ULocaleDisplayNames, scriptCode: i32, result: *mut UChar, maxResultSize: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn uldn_regionDisplayName(ldn: *const ULocaleDisplayNames, region: *const c_char, result: *mut UChar, maxResultSize: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn uldn_variantDisplayName(ldn: *const ULocaleDisplayNames, variant: *const c_char, result: *mut UChar, maxResultSize: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn uldn_keyDisplayName(ldn: *const ULocaleDisplayNames, key: *const c_char, result: *mut UChar, maxResultSize: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn uldn_keyValueDisplayName(ldn: *const ULocaleDisplayNames, key: *const c_char, value: *const c_char, result: *mut UChar, maxResultSize: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn uldn_openForContext(locale: *const c_char, contexts: *mut i32, length: i32, pErrorCode: *mut UErrorCode) -> *mut ULocaleDisplayNames;
    fn uldn_getContext(ldn: *const ULocaleDisplayNames, type_: i32, pErrorCode: *mut UErrorCode) -> i32;
    fn unumsys_open(locale: *const c_char, status: *mut UErrorCode) -> *mut UNumberingSystem;
    fn unumsys_openByName(name: *const c_char, status: *mut UErrorCode) -> *mut UNumberingSystem;
    fn unumsys_close(unumsys: *mut UNumberingSystem);
    fn unumsys_openAvailableNames(status: *mut UErrorCode) -> *mut UEnumeration;
    fn unumsys_getName(unumsys: *const UNumberingSystem) -> *const c_char;
    fn unumsys_isAlgorithmic(unumsys: *const UNumberingSystem) -> UBool;
    fn unumsys_getRadix(unumsys: *const UNumberingSystem) -> i32;
    fn unumsys_getDescription(unumsys: *const UNumberingSystem, result: *mut UChar, resultLength: i32, status: *mut UErrorCode) -> i32;
    fn umsg_open(pattern: *const UChar, patternLength: i32, locale: *const c_char, parseError: *mut UParseError, status: *mut UErrorCode) -> *mut *mut c_void;
    fn umsg_close(format: *mut *mut c_void);
    fn umsg_clone(fmt: *const *mut c_void, status: *mut UErrorCode) -> *mut c_void;
    fn umsg_setLocale(fmt: *mut *mut c_void, locale: *const c_char);
    fn umsg_getLocale(fmt: *const *mut c_void) -> *const c_char;
    fn umsg_applyPattern(fmt: *mut *mut c_void, pattern: *const UChar, patternLength: i32, parseError: *mut UParseError, status: *mut UErrorCode);
    fn umsg_toPattern(fmt: *const *mut c_void, result: *mut UChar, resultLength: i32, status: *mut UErrorCode) -> i32;
    fn umsg_autoQuoteApostrophe(pattern: *const UChar, patternLength: i32, dest: *mut UChar, destCapacity: i32, ec: *mut UErrorCode) -> i32;
    fn ucol_keyHashCode(key: *const u8, length: i32) -> i32;
    fn ucol_primaryOrder(order: i32) -> i32;
    fn ucol_secondaryOrder(order: i32) -> i32;
    fn ucol_tertiaryOrder(order: i32) -> i32;
    fn u_getDataDirectory() -> *const c_char;
    fn u_setDataDirectory(directory: *const c_char);
    fn u_getTimeZoneFilesDirectory(status: *mut UErrorCode) -> *const c_char;
    fn u_setTimeZoneFilesDirectory(path: *const c_char, status: *mut UErrorCode);
    fn u_charsToUChars(cs: *const c_char, us: *mut UChar, length: i32);
    fn u_UCharsToChars(us: *const UChar, cs: *mut c_char, length: i32);
    fn u_init(status: *mut UErrorCode);
    fn u_cleanup();
    fn u_getDataVersion(dataVersionFillin: *mut u8, status: *mut UErrorCode);
    fn ulocdata_getMeasurementSystem(localeID: *const c_char, status: *mut UErrorCode) -> i32;
    fn ulocdata_getPaperSize(localeID: *const c_char, height: *mut i32, width: *mut i32, status: *mut UErrorCode);
    fn ulocdata_getCLDRVersion(versionArray: *mut u8, status: *mut UErrorCode);
    fn unumf_openResult(ec: *mut UErrorCode) -> *mut UFormattedNumber;
    fn unumf_resultAsValue(uresult: *const UFormattedNumber, ec: *mut UErrorCode) -> *const UFormattedValue;
    fn unumf_resultToString(uresult: *const UFormattedNumber, buffer: *mut UChar, bufferCapacity: i32, ec: *mut UErrorCode) -> i32;
    fn unumf_resultNextFieldPosition(uresult: *const UFormattedNumber, ufpos: *mut UFieldPosition, ec: *mut UErrorCode) -> UBool;
    fn unumf_resultGetAllFieldPositions(uresult: *const UFormattedNumber, ufpositer: *mut UFieldPositionIterator, ec: *mut UErrorCode);
    fn unumf_resultToDecimalNumber(uresult: *const UFormattedNumber, dest: *mut c_char, destCapacity: i32, ec: *mut UErrorCode) -> i32;
    fn unumf_closeResult(uresult: *mut UFormattedNumber);
    fn ufmt_open(status: *mut UErrorCode) -> *mut *mut c_void;
    fn ufmt_close(fmt: *mut *mut c_void);
    fn ufmt_getType(fmt: *const *mut c_void, status: *mut UErrorCode) -> i32;
    fn ufmt_isNumeric(fmt: *const *mut c_void) -> UBool;
    fn ufmt_getDate(fmt: *const *mut c_void, status: *mut UErrorCode) -> UDate;
    fn ufmt_getDouble(fmt: *mut *mut c_void, status: *mut UErrorCode) -> f64;
    fn ufmt_getLong(fmt: *mut *mut c_void, status: *mut UErrorCode) -> i32;
    fn ufmt_getInt64(fmt: *mut *mut c_void, status: *mut UErrorCode) -> i64;
    fn ufmt_getObject(fmt: *const *mut c_void, status: *mut UErrorCode) -> *const c_void;
    fn ufmt_getUChars(fmt: *mut *mut c_void, len: *mut i32, status: *mut UErrorCode) -> *const UChar;
    fn ufmt_getArrayLength(fmt: *const *mut c_void, status: *mut UErrorCode) -> i32;
    fn ufmt_getArrayItemByIndex(fmt: *mut *mut c_void, n: i32, status: *mut UErrorCode) -> *mut *mut c_void;
    fn ufmt_getDecNumChars(fmt: *mut *mut c_void, len: *mut i32, status: *mut UErrorCode) -> *const c_char;
    fn utext_close(ut: *mut UText) -> *mut UText;
    fn utext_openUTF8(ut: *mut UText, s: *const c_char, length: i64, status: *mut UErrorCode) -> *mut UText;
    fn utext_openUChars(ut: *mut UText, s: *const UChar, length: i64, status: *mut UErrorCode) -> *mut UText;
    fn utext_clone(dest: *mut UText, src: *const UText, deep: UBool, readOnly: UBool, status: *mut UErrorCode) -> *mut UText;
    fn utext_equals(a: *const UText, b: *const UText) -> UBool;
    fn utext_nativeLength(ut: *mut UText) -> i64;
    fn utext_isLengthExpensive(ut: *const UText) -> UBool;
    fn utext_char32At(ut: *mut UText, nativeIndex: i64) -> UChar32;
    fn utext_current32(ut: *mut UText) -> UChar32;
    fn utext_next32(ut: *mut UText) -> UChar32;
    fn utext_previous32(ut: *mut UText) -> UChar32;
    fn utext_next32From(ut: *mut UText, nativeIndex: i64) -> UChar32;
    fn utext_previous32From(ut: *mut UText, nativeIndex: i64) -> UChar32;
    fn utext_getNativeIndex(ut: *const UText) -> i64;
    fn utext_setNativeIndex(ut: *mut UText, nativeIndex: i64);
    fn utext_moveIndex32(ut: *mut UText, delta: i32) -> UBool;
    fn utext_getPreviousNativeIndex(ut: *mut UText) -> i64;
    fn utext_extract(ut: *mut UText, nativeStart: i64, nativeLimit: i64, dest: *mut UChar, destCapacity: i32, status: *mut UErrorCode) -> i32;
    fn utext_isWritable(ut: *const UText) -> UBool;
    fn utext_hasMetaData(ut: *const UText) -> UBool;
    fn utext_replace(ut: *mut UText, nativeStart: i64, nativeLimit: i64, replacementText: *const UChar, replacementLength: i32, status: *mut UErrorCode) -> i32;
    fn utext_copy(ut: *mut UText, nativeStart: i64, nativeLimit: i64, destIndex: i64, move_: UBool, status: *mut UErrorCode);
    fn utext_freeze(ut: *mut UText);
    fn utext_setup(ut: *mut UText, extraSpace: i32, status: *mut UErrorCode) -> *mut UText;
}
// DECLS-END

// Declarations the generator cannot express (array parameters).
icu! {
    fn uenum_openCharStringsEnumeration(strings: *const *const c_char, count: i32, ec: *mut UErrorCode) -> *mut UEnumeration;
    fn ulistfmt_format(listfmt: *const UListFormatter, strings: *const *const UChar, stringLengths: *const i32, stringCount: i32, result: *mut UChar, resultCapacity: i32, status: *mut UErrorCode) -> i32;
}

// ---------------------------------------------------------------------------
// Buffer helpers
// ---------------------------------------------------------------------------

/// Runs an ICU "fill this `UChar` buffer" function with pre-flighting: the
/// call is retried with an exactly-sized buffer on `U_BUFFER_OVERFLOW_ERROR`.
/// The closure receives `(buffer, capacity, status)` and returns the length.
pub fn fill_u16(
    mut f: impl FnMut(*mut UChar, i32, &mut UErrorCode) -> i32,
) -> Result<Vec<UChar>, UErrorCode> {
    let mut buf: Vec<UChar> = vec![0; 128];
    let mut status = UErrorCode::ZERO;
    let cap = i32::try_from(buf.len()).unwrap_or(i32::MAX);
    let len = f(buf.as_mut_ptr(), cap, &mut status);
    if status == UErrorCode::BUFFER_OVERFLOW_ERROR {
        let need = usize::try_from(len).unwrap_or(0);
        buf = vec![0; need + 1];
        status = UErrorCode::ZERO;
        let cap = i32::try_from(buf.len()).unwrap_or(i32::MAX);
        let len = f(buf.as_mut_ptr(), cap, &mut status);
        if status.is_failure() {
            return Err(status);
        }
        buf.truncate(usize::try_from(len).unwrap_or(0));
        return Ok(buf);
    }
    if status.is_failure() {
        return Err(status);
    }
    buf.truncate(usize::try_from(len).unwrap_or(0).min(buf.len()));
    Ok(buf)
}

/// Like [`fill_u16`] but also returns the final (possibly warning) status.
pub fn fill_u16_status(
    mut f: impl FnMut(*mut UChar, i32, &mut UErrorCode) -> i32,
) -> (Vec<UChar>, UErrorCode) {
    let mut last = UErrorCode::ZERO;
    let res = fill_u16(|b, c, s| {
        let r = f(b, c, s);
        last = *s;
        r
    });
    match res {
        Ok(v) => (v, last),
        Err(e) => (Vec::new(), e),
    }
}

/// Pre-flighting helper for functions filling a `char` buffer.
pub fn fill_c(
    mut f: impl FnMut(*mut c_char, i32, &mut UErrorCode) -> i32,
) -> Result<Vec<u8>, UErrorCode> {
    let mut buf: Vec<u8> = vec![0; 160];
    let mut status = UErrorCode::ZERO;
    let cap = i32::try_from(buf.len()).unwrap_or(i32::MAX);
    let len = f(buf.as_mut_ptr().cast(), cap, &mut status);
    if status == UErrorCode::BUFFER_OVERFLOW_ERROR {
        let need = usize::try_from(len).unwrap_or(0);
        buf = vec![0; need + 1];
        status = UErrorCode::ZERO;
        let cap = i32::try_from(buf.len()).unwrap_or(i32::MAX);
        let len = f(buf.as_mut_ptr().cast(), cap, &mut status);
        if status.is_failure() {
            return Err(status);
        }
        buf.truncate(usize::try_from(len).unwrap_or(0));
        return Ok(buf);
    }
    if status.is_failure() {
        return Err(status);
    }
    buf.truncate(usize::try_from(len).unwrap_or(0).min(buf.len()));
    Ok(buf)
}

/// Converts a C string returned by ICU (static or owned by an ICU object)
/// into an owned `String`.  A null pointer yields `None`.
///
/// # Safety
/// `p` must be null or point to a NUL-terminated string that stays valid
/// for the duration of the call.
pub unsafe fn cstr_to_string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller; the bytes are copied immediately.
    let s = unsafe { CStr::from_ptr(p) };
    Some(String::from_utf8_lossy(s.to_bytes()).into_owned())
}

/// Length of a buffer as the `int32_t` ICU expects (saturating; callers
/// reject inputs larger than `i32::MAX` before reaching ICU).
pub fn len32<T>(s: &[T]) -> i32 {
    i32::try_from(s.len()).unwrap_or(i32::MAX)
}

/// A NUL-terminated copy of `s` for passing as `const char*`.
pub fn cstring(s: &str) -> std::ffi::CString {
    std::ffi::CString::new(s.replace('\0', "")).unwrap_or_default()
}

/// A NUL-terminated copy of UTF-16 text.
pub fn u16z(s: &[UChar]) -> Vec<UChar> {
    let mut v = Vec::with_capacity(s.len() + 1);
    v.extend_from_slice(s);
    v.push(0);
    v
}

/// Owned `UEnumeration*`.
#[derive(Debug)]
pub struct Enumeration(*mut UEnumeration);

impl Enumeration {
    /// Wraps a pointer returned by an ICU `*_open*` enumeration function.
    ///
    /// # Safety
    /// `p` must be null or an enumeration returned by ICU that nobody else
    /// closes.
    pub unsafe fn from_raw(p: *mut UEnumeration, status: UErrorCode) -> Result<Self, UErrorCode> {
        if status.is_failure() {
            if !p.is_null() {
                // SAFETY: p was returned by ICU and is closed exactly once.
                unsafe { uenum_close(p) };
            }
            return Err(status);
        }
        if p.is_null() {
            return Err(UErrorCode::MEMORY_ALLOCATION_ERROR);
        }
        Ok(Self(p))
    }

    /// The raw pointer (still owned by `self`).
    pub fn as_ptr(&self) -> *mut UEnumeration {
        self.0
    }

    /// Number of elements (`uenum_count`).
    pub fn count(&self) -> Result<i32, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is a live enumeration.
        let n = unsafe { uenum_count(self.0, &mut st) };
        if st.is_failure() {
            Err(st)
        } else {
            Ok(n)
        }
    }

    /// Next element as UTF-8 via `uenum_next` (char form).
    pub fn next_str(&mut self) -> Result<Option<String>, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        let mut len = 0i32;
        // SAFETY: self.0 is a live enumeration; the returned pointer is valid
        // until the next call and is copied immediately.
        let p = unsafe { uenum_next(self.0, &mut len, &mut st) };
        if st.is_failure() {
            return Err(st);
        }
        if p.is_null() {
            return Ok(None);
        }
        let n = usize::try_from(len).unwrap_or(0);
        // SAFETY: ICU guarantees `len` valid bytes at p.
        let bytes = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), n) };
        Ok(Some(String::from_utf8_lossy(bytes).into_owned()))
    }

    /// Next element as UTF-16 via `uenum_unext`.
    pub fn next_u16(&mut self) -> Result<Option<Vec<UChar>>, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        let mut len = 0i32;
        // SAFETY: as in next_str.
        let p = unsafe { uenum_unext(self.0, &mut len, &mut st) };
        if st.is_failure() {
            return Err(st);
        }
        if p.is_null() {
            return Ok(None);
        }
        let n = usize::try_from(len).unwrap_or(0);
        // SAFETY: ICU guarantees `len` valid code units at p.
        Ok(Some(unsafe { std::slice::from_raw_parts(p, n) }.to_vec()))
    }

    /// Resets the enumeration to its start.
    pub fn reset(&mut self) -> Result<(), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live.
        unsafe { uenum_reset(self.0, &mut st) };
        if st.is_failure() {
            Err(st)
        } else {
            Ok(())
        }
    }

    /// Drains all remaining elements as UTF-8 strings.
    pub fn collect_strings(mut self) -> Result<Vec<String>, UErrorCode> {
        let mut out = Vec::new();
        while let Some(s) = self.next_str()? {
            out.push(s);
        }
        Ok(out)
    }
}

impl Drop for Enumeration {
    fn drop(&mut self) {
        // SAFETY: self.0 is a live enumeration owned by us.
        unsafe { uenum_close(self.0) };
    }
}

/// `u_errorName` for any code (unknown codes yield `[BOGUS UErrorCode]`).
pub fn error_name(code: i32) -> String {
    // SAFETY: u_errorName accepts any int and returns a static string.
    let p = unsafe { u_errorName(UErrorCode(code)) };
    // SAFETY: the static string returned above is NUL-terminated.
    unsafe { cstr_to_string(p) }.unwrap_or_default()
}

/// Null `UParseError*`-compatible pointer helper.
pub fn null_mut<T>() -> *mut T {
    ptr::null_mut()
}
