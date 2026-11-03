//! PHP's formatted-print engine and float formatting.
//!
//! * [`double`] — `zend_gcvt()`, `php_conv_fp()` and [`format_double`].

pub mod double;

pub use double::{format_double, php_conv_fp, zend_gcvt, DoubleMode};
