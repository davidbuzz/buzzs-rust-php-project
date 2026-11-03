//! `u_*` character property functions.

use crate::ffi;

/// `u_hasBinaryProperty()`.
pub fn has_binary_property(c: u32, prop: i32) -> bool {
    #[allow(clippy::cast_possible_wrap)]
    // SAFETY: any code point value is accepted.
    unsafe {
        ffi::u_hasBinaryProperty(c as i32, prop) != 0
    }
}

/// `UCHAR_PATTERN_SYNTAX`.
pub const PATTERN_SYNTAX: i32 = 42;
/// `UCHAR_PATTERN_WHITE_SPACE`.
pub const PATTERN_WHITE_SPACE: i32 = 43;

/// `u_isspace()`.
pub fn is_space(c: u32) -> bool {
    #[allow(clippy::cast_possible_wrap)]
    // SAFETY: any code point value is accepted.
    unsafe {
        ffi::u_isspace(c as i32) != 0
    }
}
