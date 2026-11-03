//! PHP's formatted-print engine and float formatting.
//!
//! * [`double`] — `zend_dtoa()` digit generation, `zend_gcvt()`,
//!   `php_conv_fp()` and [`format_double`] (float → string).
//! * [`printf`] — the `sprintf()` family (`ext/standard/formatted_print.c`).
//! * [`number`] — `round()` and `number_format()` (`ext/standard/math.c`).

pub mod double;
pub mod number;
pub mod printf;

pub use double::{format_double, php_conv_fp, zend_gcvt, DoubleMode};
pub use number::{
    number_format, number_format_double, number_format_long, php_round, Number, RoundingMode,
};
pub use printf::{
    php_formatted_print, sprintf, vsprintf, ArgSource, FormatArg, FormatError, FormatOptions,
};
