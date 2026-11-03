//! A faithful Rust port of PHP's `ext/date` engine (timelib).
//!
//! This crate knows nothing about PHP values; it exposes plain Rust types
//! that the VM wraps into `DateTime`, `DateTimeImmutable`, `DateTimeZone`,
//! `DateInterval` and `DatePeriod`, plus the procedural `date()`,
//! `strtotime()`, `mktime()`, `date_parse()` … functions.
//!
//! * [`tz`] — the bundled timezone database (identifiers, transitions,
//!   abbreviations, POSIX footer rules).
//! * [`parse`] — `strtotime()` grammar (`parse_date.re`), `createFromFormat`
//!   (`timelib_parse_from_format`) and ISO 8601 intervals/periods.
//! * [`time`] — broken-down time ↔ timestamps, relative time application.
//! * [`interval`] — `diff()`, `add()`, `sub()`.
//! * [`format`] — `date()`/`DateTime::format()` and `DateInterval::format()`.
//! * [`astro`] — sunrise/sunset/twilight (`date_sun_info()`).
//! * [`api`] — PHP-function-shaped helpers built on the above.

pub mod api;
pub mod astro;
pub mod dow;
pub mod format;
pub mod interval;
pub mod parse;
pub mod time;
pub mod tz;

/// timelib's `TIMELIB_UNSET` marker for fields that were not set.
pub const UNSET: i64 = -9_999_999;

pub use time::{RelTime, Time, ZoneType};
pub use tz::TzInfo;
