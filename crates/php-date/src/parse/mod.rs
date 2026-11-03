//! Date/time string parsers (ports of timelib's `parse_date.re` and
//! `parse_iso_intervals.re`).
//!
//! * [`strtotime`] — the free-form grammar behind `strtotime()`,
//!   `date_parse()`, `new DateTime($str)` and `DateTime::modify()`.
//! * [`parse_from_format`] — `DateTime::createFromFormat()` /
//!   `date_parse_from_format()`.
//! * [`parse_iso_interval`] — ISO 8601 durations and periods
//!   (`new DateInterval('P1D')`, `new DatePeriod('R4/2012-07-01T00:00:00Z/P7D')`).

mod from_format;
mod iso_interval;
mod regex;
mod strtotime;
mod zone;

pub use from_format::parse_from_format;
pub use iso_interval::{parse_iso_interval, IsoInterval};
pub use strtotime::strtotime;
pub use zone::{parse_timezone_spec, TimezoneSpec};

/// timelib error/warning codes (`TIMELIB_ERR_*` / `TIMELIB_WARN_*`).
#[allow(missing_docs)]
pub mod code {
    pub const WARN_DOUBLE_TZ: u32 = 0x101;
    pub const WARN_INVALID_TIME: u32 = 0x102;
    pub const WARN_INVALID_DATE: u32 = 0x103;
    pub const WARN_TRAILING_DATA: u32 = 0x11a;
    pub const ERR_DOUBLE_TZ: u32 = 0x201;
    pub const ERR_TZID_NOT_FOUND: u32 = 0x202;
    pub const ERR_DOUBLE_TIME: u32 = 0x203;
    pub const ERR_DOUBLE_DATE: u32 = 0x204;
    pub const ERR_UNEXPECTED_CHARACTER: u32 = 0x205;
    pub const ERR_EMPTY_STRING: u32 = 0x206;
    pub const ERR_UNEXPECTED_DATA: u32 = 0x207;
    pub const ERR_NO_TEXTUAL_DAY: u32 = 0x208;
    pub const ERR_NO_TWO_DIGIT_DAY: u32 = 0x209;
    pub const ERR_NO_THREE_DIGIT_DAY_OF_YEAR: u32 = 0x20a;
    pub const ERR_NO_TWO_DIGIT_MONTH: u32 = 0x20b;
    pub const ERR_NO_TEXTUAL_MONTH: u32 = 0x20c;
    pub const ERR_NO_TWO_DIGIT_YEAR: u32 = 0x20d;
    pub const ERR_NO_FOUR_DIGIT_YEAR: u32 = 0x20e;
    pub const ERR_NO_TWO_DIGIT_HOUR: u32 = 0x20f;
    pub const ERR_HOUR_LARGER_THAN_12: u32 = 0x210;
    pub const ERR_MERIDIAN_BEFORE_HOUR: u32 = 0x211;
    pub const ERR_NO_MERIDIAN: u32 = 0x212;
    pub const ERR_NO_TWO_DIGIT_MINUTE: u32 = 0x213;
    pub const ERR_NO_TWO_DIGIT_SECOND: u32 = 0x214;
    pub const ERR_NO_SIX_DIGIT_MICROSECOND: u32 = 0x215;
    pub const ERR_NO_SEP_SYMBOL: u32 = 0x216;
    pub const ERR_EXPECTED_ESCAPE_CHAR: u32 = 0x217;
    pub const ERR_NO_ESCAPED_CHAR: u32 = 0x218;
    pub const ERR_WRONG_FORMAT_SEP: u32 = 0x219;
    pub const ERR_TRAILING_DATA: u32 = 0x21a;
    pub const ERR_DATA_MISSING: u32 = 0x21b;
    pub const ERR_NO_THREE_DIGIT_MILLISECOND: u32 = 0x21c;
    pub const ERR_NO_FOUR_DIGIT_YEAR_ISO: u32 = 0x21d;
    pub const ERR_NO_TWO_DIGIT_WEEK: u32 = 0x21e;
    pub const ERR_INVALID_WEEK: u32 = 0x21f;
    pub const ERR_NO_DAY_OF_WEEK: u32 = 0x220;
    pub const ERR_INVALID_DAY_OF_WEEK: u32 = 0x221;
    pub const ERR_INVALID_SPECIFIER: u32 = 0x222;
    pub const ERR_INVALID_TZ_OFFSET: u32 = 0x223;
    pub const ERR_FORMAT_LITERAL_MISMATCH: u32 = 0x224;
    pub const ERR_MIX_ISO_WITH_NATURAL: u32 = 0x225;
    pub const ERR_NUMBER_OUT_OF_RANGE: u32 = 0x226;
}

const MAX_ERRORS: usize = 1000;

/// One parse error or warning (`timelib_error_message`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// timelib error code (see [`code`]).
    pub code: u32,
    /// Byte position in the (whitespace-trimmed) input.
    pub position: usize,
    /// The byte at that position (0 past the end).
    pub character: u8,
    /// The message text, e.g. `"Unexpected character"`.
    pub message: &'static str,
}

/// Errors and warnings from a parse (`timelib_error_container`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Errors {
    /// Warnings, in order.
    pub warnings: Vec<Message>,
    /// Errors, in order.
    pub errors: Vec<Message>,
}

impl Errors {
    pub(crate) fn add_error(&mut self, code: u32, position: usize, character: u8, message: &'static str) {
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(Message {
                code,
                position,
                character,
                message,
            });
        }
    }

    pub(crate) fn add_warning(&mut self, code: u32, position: usize, character: u8, message: &'static str) {
        if self.warnings.len() < MAX_ERRORS {
            self.warnings.push(Message {
                code,
                position,
                character,
                message,
            });
        }
    }

    /// Whether there are neither errors nor warnings.
    pub fn is_empty(&self) -> bool {
        self.warnings.is_empty() && self.errors.is_empty()
    }

    /// The `"at position %d (%c): %s"` tail PHP uses when reporting the
    /// first error, e.g. `at position 0 (f): The timezone could not be found
    /// in the database`. Bytes, because `%c` may be any byte.
    pub fn first_error_description(&self) -> Option<Vec<u8>> {
        self.errors.first().map(|e| {
            let c = if e.character == 0 { b' ' } else { e.character };
            let mut out = format!("at position {} (", e.position).into_bytes();
            out.push(c);
            out.extend_from_slice(b"): ");
            out.extend_from_slice(e.message.as_bytes());
            out
        })
    }
}
