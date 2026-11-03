//! `ext/date` for the phprust VM: binds the `php-date` timelib port through
//! the `php_vm::ext` extension API.
//!
//! * [`functions`] — `date()`, `strtotime()`, `mktime()`, `date_parse()`,
//!   `strftime()`, the timezone listings and the sun functions.
//! * [`datetime`] — `DateTimeInterface`, `DateTime`, `DateTimeImmutable` and
//!   the procedural `date_*()` aliases.
//! * [`timezone`] — `DateTimeZone` and `timezone_*()`.
//! * [`interval`] — `DateInterval` with its property handlers.
//! * [`period`] — `DatePeriod`, its overloaded constructor and iteration.
//! * [`duration`] — `Time\Duration` (PHP 8.6).
//!
//! Native state lives in the objects (see [`objects`]); the per-request
//! globals (`DATEG(timezone)`, `DATEG(last_errors)`) in [`common`].

// Native handlers and object hooks have signatures fixed by the VM (they
// return `VmResult` even when they cannot fail), and the code keeps
// timelib's one-letter field names (y, m, d, h, i, s).
#![allow(clippy::unnecessary_wraps, clippy::many_single_char_names)]

mod common;
mod datetime;
mod duration;
mod functions;
mod interval;
mod objects;
mod period;
mod timezone;

use php_date::api;
use php_runtime::Value;
use php_support::ini::{Access, IniDirective, ModifyHandler};
use php_vm::ext::{ClassBuilder, Extension, ModuleBuilder};

/// The `date` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Date;

/// The exception hierarchy added in PHP 8.3 (`parent`, `class`).
const EXCEPTIONS: [(&str, &str); 9] = [
    ("Error", "DateError"),
    ("DateError", "DateObjectError"),
    ("DateError", "DateRangeError"),
    ("Exception", "DateException"),
    ("DateException", "DateInvalidTimeZoneException"),
    ("DateException", "DateInvalidOperationException"),
    ("DateException", "DateMalformedStringException"),
    ("DateException", "DateMalformedIntervalStringException"),
    ("DateException", "DateMalformedPeriodStringException"),
];

impl Extension for Date {
    fn name(&self) -> &'static str {
        "date"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        for (name, default) in [
            ("date.default_latitude", "31.7667"),
            ("date.default_longitude", "35.2333"),
            ("date.sunset_zenith", "90.833333"),
            ("date.sunrise_zenith", "90.833333"),
        ] {
            m.ini(IniDirective::new(
                name,
                Some(default),
                Access::ALL,
                ModifyHandler::None,
                "date",
            ));
        }
        m.constant(
            "SUNFUNCS_RET_TIMESTAMP",
            Value::Int(api::SUNFUNCS_RET_TIMESTAMP),
        )
        .constant("SUNFUNCS_RET_STRING", Value::Int(api::SUNFUNCS_RET_STRING))
        .constant("SUNFUNCS_RET_DOUBLE", Value::Int(api::SUNFUNCS_RET_DOUBLE));

        {
            let vm = m.vm();
            for (parent, name) in EXCEPTIONS {
                ClassBuilder::new(name).extends(vm, parent).register(vm);
            }
        }

        datetime::register(m);
        timezone::register(m);
        interval::register(m);
        period::register(m);
        duration::register(m);

        functions::register(m);
        datetime::register_functions(m);

        m.on_request_shutdown(|_| {
            common::reset_request_state();
            duration::reset_cache();
        });
    }
}
