//! `u_*` character property functions.

use std::ffi::{c_char, c_void};

use crate::error::UErrorCode;
use crate::ffi;

/// `UCHAR_PATTERN_SYNTAX`.
pub const PATTERN_SYNTAX: i32 = 42;
/// `UCHAR_PATTERN_WHITE_SPACE`.
pub const PATTERN_WHITE_SPACE: i32 = 43;

#[allow(clippy::cast_possible_wrap)]
fn cp(c: u32) -> i32 {
    c as i32
}

/// `u_hasBinaryProperty()`.
pub fn has_binary_property(c: u32, prop: i32) -> bool {
    // SAFETY: any code point / property value is accepted.
    unsafe { ffi::u_hasBinaryProperty(cp(c), prop) != 0 }
}

/// `u_isspace()`.
pub fn is_space(c: u32) -> bool {
    // SAFETY: any code point value is accepted.
    unsafe { ffi::u_isspace(cp(c)) != 0 }
}

macro_rules! bool_fns {
    ($($name:ident => $f:ident;)*) => {
        /// The boolean `u_is*` predicates by PHP method name.
        pub fn bool_property(name: &str, c: i32) -> Option<bool> {
            // SAFETY: the predicates accept any code point value.
            Some(unsafe {
                match name {
                    $(stringify!($name) => ffi::$f(c) != 0,)*
                    _ => return None,
                }
            })
        }
    };
}

bool_fns! {
    isUAlphabetic => u_isUAlphabetic;
    isULowercase => u_isULowercase;
    isUUppercase => u_isUUppercase;
    isUWhiteSpace => u_isUWhiteSpace;
    islower => u_islower;
    isupper => u_isupper;
    istitle => u_istitle;
    isdigit => u_isdigit;
    isalpha => u_isalpha;
    isalnum => u_isalnum;
    isxdigit => u_isxdigit;
    ispunct => u_ispunct;
    isgraph => u_isgraph;
    isblank => u_isblank;
    isdefined => u_isdefined;
    isspace => u_isspace;
    isJavaSpaceChar => u_isJavaSpaceChar;
    isWhitespace => u_isWhitespace;
    iscntrl => u_iscntrl;
    isISOControl => u_isISOControl;
    isprint => u_isprint;
    isbase => u_isbase;
    isMirrored => u_isMirrored;
    isIDStart => u_isIDStart;
    isIDPart => u_isIDPart;
    isIDIgnorable => u_isIDIgnorable;
    isJavaIDStart => u_isJavaIDStart;
    isJavaIDPart => u_isJavaIDPart;
}

/// The integer-valued per-character functions by PHP method name.
pub fn int_property(name: &str, c: i32) -> Option<i64> {
    // SAFETY: all accept any code point value.
    Some(unsafe {
        match name {
            "charDirection" => i64::from(ffi::u_charDirection(c)),
            "charType" => i64::from(ffi::u_charType(c)),
            "getCombiningClass" => i64::from(ffi::u_getCombiningClass(c)),
            "charDigitValue" => i64::from(ffi::u_charDigitValue(c)),
            "getBlockCode" => i64::from(ffi::ublock_getCode(c)),
            _ => return None,
        }
    })
}

/// The code-point mapping functions by PHP method name.
pub fn char_mapping(name: &str, c: i32) -> Option<i32> {
    // SAFETY: all accept any code point value.
    Some(unsafe {
        match name {
            "charMirror" => ffi::u_charMirror(c),
            "tolower" => ffi::u_tolower(c),
            "toupper" => ffi::u_toupper(c),
            "totitle" => ffi::u_totitle(c),
            "getBidiPairedBracket" => ffi::u_getBidiPairedBracket(c),
            _ => return None,
        }
    })
}

/// `u_getIntPropertyValue()`.
pub fn int_property_value(c: i32, prop: i32) -> i32 {
    // SAFETY: no preconditions.
    unsafe { ffi::u_getIntPropertyValue(c, prop) }
}

/// `u_getIntPropertyMinValue()`.
pub fn int_property_min(prop: i32) -> i32 {
    // SAFETY: no preconditions.
    unsafe { ffi::u_getIntPropertyMinValue(prop) }
}

/// `u_getIntPropertyMaxValue()`.
pub fn int_property_max(prop: i32) -> i32 {
    // SAFETY: no preconditions.
    unsafe { ffi::u_getIntPropertyMaxValue(prop) }
}

/// `u_getNumericValue()`.
pub fn numeric_value(c: i32) -> f64 {
    // SAFETY: no preconditions.
    unsafe { ffi::u_getNumericValue(c) }
}

/// `u_foldCase()`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn fold_case(c: i32, options: i64) -> i32 {
    // SAFETY: no preconditions.
    unsafe { ffi::u_foldCase(c, options as u32) }
}

/// `u_digit()`.
pub fn digit(c: i32, radix: i8) -> i32 {
    // SAFETY: no preconditions.
    unsafe { ffi::u_digit(c, radix) }
}

/// `u_forDigit()`.
pub fn for_digit(d: i32, radix: i8) -> i32 {
    // SAFETY: no preconditions.
    unsafe { ffi::u_forDigit(d, radix) }
}

/// `u_charAge()`.
pub fn char_age(c: i32) -> [u8; 4] {
    let mut v = [0u8; 4];
    // SAFETY: v has U_MAX_VERSION_LENGTH bytes.
    unsafe { ffi::u_charAge(c, v.as_mut_ptr()) };
    v
}

/// `u_getUnicodeVersion()`.
pub fn unicode_version() -> [u8; 4] {
    let mut v = [0u8; 4];
    // SAFETY: v has U_MAX_VERSION_LENGTH bytes.
    unsafe { ffi::u_getUnicodeVersion(v.as_mut_ptr()) };
    v
}

/// `u_charName()`.
pub fn char_name(c: i32, choice: i32) -> Result<Vec<u8>, UErrorCode> {
    // SAFETY: buffer contract handled by fill_c.
    ffi::fill_c(|b, cap, st| unsafe { ffi::u_charName(c, choice, b, cap, st) })
}

/// `u_charFromName()`.
pub fn char_from_name(choice: i32, name: &[u8]) -> Result<i32, UErrorCode> {
    let n = std::ffi::CString::new(name.to_vec()).map_err(|_| UErrorCode::INVALID_CHAR_FOUND)?;
    let mut st = UErrorCode::ZERO;
    // SAFETY: n is NUL-terminated.
    let r = unsafe { ffi::u_charFromName(choice, n.as_ptr(), &mut st) };
    st.check().map(|()| r)
}

/// `u_getPropertyName()`.
pub fn property_name(prop: i32, choice: i32) -> Option<String> {
    // SAFETY: returns a static string or null.
    unsafe { ffi::cstr_to_string(ffi::u_getPropertyName(prop, choice)) }
}

/// `u_getPropertyEnum()`.
pub fn property_enum(alias: &[u8]) -> i32 {
    let Ok(a) = std::ffi::CString::new(alias.to_vec()) else {
        return -1;
    };
    // SAFETY: a is NUL-terminated.
    unsafe { ffi::u_getPropertyEnum(a.as_ptr()) }
}

/// `u_getPropertyValueName()`.
pub fn property_value_name(prop: i32, value: i32, choice: i32) -> Option<String> {
    // SAFETY: returns a static string or null.
    unsafe { ffi::cstr_to_string(ffi::u_getPropertyValueName(prop, value, choice)) }
}

/// `u_getPropertyValueEnum()`.
pub fn property_value_enum(prop: i32, name: &[u8]) -> i32 {
    let Ok(a) = std::ffi::CString::new(name.to_vec()) else {
        return -1;
    };
    // SAFETY: a is NUL-terminated.
    unsafe { ffi::u_getPropertyValueEnum(prop, a.as_ptr()) }
}

/// `u_getFC_NFKC_Closure()`.
pub fn fc_nfkc_closure(c: i32) -> Result<Vec<u16>, UErrorCode> {
    let mut st = UErrorCode::ZERO;
    // SAFETY: preflight with a null buffer.
    let n = unsafe { ffi::u_getFC_NFKC_Closure(c, std::ptr::null_mut(), 0, &mut st) };
    if n <= 0 {
        return Ok(Vec::new());
    }
    let mut v = vec![0u16; usize::try_from(n).unwrap_or(0) + 1];
    let mut st = UErrorCode::ZERO;
    // SAFETY: v has n + 1 UChars.
    let m = unsafe { ffi::u_getFC_NFKC_Closure(c, v.as_mut_ptr(), n, &mut st) };
    if st.is_failure() && st != UErrorCode::STRING_NOT_TERMINATED_WARNING {
        return Err(st);
    }
    v.truncate(usize::try_from(m).unwrap_or(0));
    Ok(v)
}

unsafe extern "C" fn types_trampoline(
    context: *const c_void,
    start: i32,
    limit: i32,
    ty: i32,
) -> i8 {
    // SAFETY: context is the `&mut dyn FnMut` passed by enum_char_types,
    // valid for the duration of u_enumCharTypes.
    let f = unsafe {
        &mut *(context
            .cast_mut()
            .cast::<&mut dyn FnMut(i32, i32, i32) -> bool>())
    };
    i8::from(f(start, limit, ty))
}

/// `u_enumCharTypes()` calling `f(start, limit, type)`; return `false`
/// from `f` to stop.
pub fn enum_char_types(mut f: &mut dyn FnMut(i32, i32, i32) -> bool) {
    let ctx: *mut &mut dyn FnMut(i32, i32, i32) -> bool = &mut f;
    // SAFETY: the trampoline casts the context back to the same type; it
    // outlives the call.
    unsafe { ffi::u_enumCharTypes(Some(types_trampoline), ctx.cast_const().cast()) };
}

unsafe extern "C" fn names_trampoline(
    context: *mut c_void,
    code: i32,
    choice: i32,
    name: *const c_char,
    len: i32,
) -> i8 {
    // SAFETY: as in types_trampoline.
    let f = unsafe { &mut *(context.cast::<&mut dyn FnMut(i32, i32, &[u8]) -> bool>()) };
    // SAFETY: ICU passes `len` bytes at `name`.
    let s =
        unsafe { std::slice::from_raw_parts(name.cast::<u8>(), usize::try_from(len).unwrap_or(0)) };
    i8::from(f(code, choice, s))
}

/// `u_enumCharNames(start, limit, f, choice)`.
pub fn enum_char_names(
    start: i32,
    limit: i32,
    choice: i32,
    mut f: &mut dyn FnMut(i32, i32, &[u8]) -> bool,
) -> Result<(), UErrorCode> {
    let ctx: *mut &mut dyn FnMut(i32, i32, &[u8]) -> bool = &mut f;
    let mut st = UErrorCode::ZERO;
    // SAFETY: as in enum_char_types.
    unsafe {
        ffi::u_enumCharNames(
            start,
            limit,
            Some(names_trampoline),
            ctx.cast(),
            choice,
            &mut st,
        )
    };
    st.check()
}
