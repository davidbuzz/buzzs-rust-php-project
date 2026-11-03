//! `IntlRelativeDateTimeFormatter` and its enums (php-src
//! `ext/intl/reldateformatter`, PHP 8.5).
//!
//! The PHP object keeps a reference to the `NumberFormatter` it was built
//! with; the VM passes that object to every formatting call so a changed
//! formatter (its `configuration_version`) is picked up, as in php-src.
//! Constructor ICU failures always throw (`IntlError::always_throw`);
//! other errors are recorded on the object and globally.

use crate::common::{default_locale, validate_locale_arg, INTL_MAX_LOCALE_LEN};
use crate::error::{Error, IntlError, Result};
use crate::numfmt::NumberFormatter;
use crate::sys::ureldatefmt::{self, RelDateFormatter};
use crate::ustr;

/// `IntlRelativeDateTimeFormatterStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    /// `Long` (`UDAT_STYLE_LONG`)
    #[default]
    Long,
    /// `Short`
    Short,
    /// `Narrow`
    Narrow,
}

/// `IntlRelativeDateTimeFormatterCapitalization`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Capitalization {
    /// `None`
    #[default]
    None,
    /// `MiddleOfSentence`
    MiddleOfSentence,
    /// `BeginningOfSentence`
    BeginningOfSentence,
    /// `UiListAndMenu`
    UiListAndMenu,
    /// `Standalone`
    Standalone,
}

/// `IntlRelativeDateTimeFormatterUnit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// `Year`
    Year,
    /// `Quarter`
    Quarter,
    /// `Month`
    Month,
    /// `Week`
    Week,
    /// `Day`
    Day,
    /// `Hour`
    Hour,
    /// `Minute`
    Minute,
    /// `Second`
    Second,
    /// `Sunday`
    Sunday,
    /// `Monday`
    Monday,
    /// `Tuesday`
    Tuesday,
    /// `Wednesday`
    Wednesday,
    /// `Thursday`
    Thursday,
    /// `Friday`
    Friday,
    /// `Saturday`
    Saturday,
}

/// Case names of `IntlRelativeDateTimeFormatterStyle`, in declaration order.
pub const STYLE_CASES: [&str; 3] = ["Long", "Short", "Narrow"];
/// Case names of `IntlRelativeDateTimeFormatterCapitalization`.
pub const CAPITALIZATION_CASES: [&str; 5] = [
    "None",
    "MiddleOfSentence",
    "BeginningOfSentence",
    "UiListAndMenu",
    "Standalone",
];
/// Case names of `IntlRelativeDateTimeFormatterUnit`.
pub const UNIT_CASES: [&str; 15] = [
    "Year",
    "Quarter",
    "Month",
    "Week",
    "Day",
    "Hour",
    "Minute",
    "Second",
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

impl Style {
    /// From an enum case name.
    #[must_use]
    pub fn from_case(name: &str) -> Option<Self> {
        Some(match name {
            "Long" => Self::Long,
            "Short" => Self::Short,
            "Narrow" => Self::Narrow,
            _ => return None,
        })
    }

    fn icu(self) -> i32 {
        match self {
            Self::Long => 0,
            Self::Short => 1,
            Self::Narrow => 2,
        }
    }
}

impl Capitalization {
    /// From an enum case name.
    #[must_use]
    pub fn from_case(name: &str) -> Option<Self> {
        Some(match name {
            "None" => Self::None,
            "MiddleOfSentence" => Self::MiddleOfSentence,
            "BeginningOfSentence" => Self::BeginningOfSentence,
            "UiListAndMenu" => Self::UiListAndMenu,
            "Standalone" => Self::Standalone,
            _ => return None,
        })
    }

    /// `UDisplayContext` value.
    fn icu(self) -> i32 {
        match self {
            Self::None => 0x100,
            Self::MiddleOfSentence => 0x101,
            Self::BeginningOfSentence => 0x102,
            Self::UiListAndMenu => 0x103,
            Self::Standalone => 0x104,
        }
    }
}

impl Unit {
    /// From an enum case name.
    #[must_use]
    pub fn from_case(name: &str) -> Option<Self> {
        UNIT_CASES
            .iter()
            .position(|c| *c == name)
            .and_then(Self::from_index)
    }

    fn from_index(i: usize) -> Option<Self> {
        use Unit::{
            Day, Friday, Hour, Minute, Monday, Month, Quarter, Saturday, Second, Sunday, Thursday,
            Tuesday, Wednesday, Week, Year,
        };
        [
            Year, Quarter, Month, Week, Day, Hour, Minute, Second, Sunday, Monday, Tuesday,
            Wednesday, Thursday, Friday, Saturday,
        ]
        .get(i)
        .copied()
    }

    /// `URelativeDateTimeUnit` value (declaration order).
    fn icu(self) -> i32 {
        self as i32
    }
}

/// Message of the `Error` thrown by a second `__construct()` call.
pub const ALREADY_CONSTRUCTED: &str = "IntlRelativeDateTimeFormatter object is already constructed";
/// Message of the `Error` thrown for an unconstructed `NumberFormatter`
/// argument.
pub const UNCONSTRUCTED_NUMBER_FORMATTER: &str = "Found unconstructed NumberFormatter";
/// Message of the `Error` thrown when using an unconstructed object.
pub const UNCONSTRUCTED: &str = "Found unconstructed IntlRelativeDateTimeFormatter";

/// A PHP `IntlRelativeDateTimeFormatter` object (not cloneable).
#[derive(Debug)]
pub struct IntlRelativeDateTimeFormatter {
    fmt: RelDateFormatter,
    locale: String,
    style: Style,
    capitalization: Capitalization,
    /// `configuration_version` of the bound `NumberFormatter`.
    nf_version: Option<u64>,
}

/// `reldateformatter_open()`.
fn open(
    locale: &str,
    style: Style,
    capitalization: Capitalization,
    nf: Option<&NumberFormatter>,
) -> std::result::Result<RelDateFormatter, crate::UErrorCode> {
    let mut ctx = capitalization;
    if ctx == Capitalization::BeginningOfSentence {
        // php-src titlecases the result itself with a sentence iterator.
        ureldatefmt::check_sentence_iterator(locale)?;
        ctx = Capitalization::None;
    }
    let nf = nf.map(|n| n.raw().try_clone()).transpose()?;
    RelDateFormatter::open(locale, nf, style.icu(), ctx.icu())
}

impl IntlRelativeDateTimeFormatter {
    /// `new IntlRelativeDateTimeFormatter($locale, $style,
    /// $capitalizationContext, $numberFormatter)`.  The VM throws
    /// [`UNCONSTRUCTED_NUMBER_FORMATTER`] itself for an unconstructed
    /// formatter object.
    pub fn new(
        locale: Option<&str>,
        style: Style,
        capitalization: Capitalization,
        number_formatter: Option<&NumberFormatter>,
    ) -> Result<Self> {
        let locale = locale.unwrap_or_default();
        if locale.len() > INTL_MAX_LOCALE_LEN {
            return Err(Error::arg_value(
                1,
                format!("must be less than or equal to {INTL_MAX_LOCALE_LEN} characters"),
            ));
        }
        let locale = if locale.is_empty() {
            default_locale()
        } else {
            locale.split('\0').next().unwrap_or_default().to_owned()
        };
        validate_locale_arg(&locale, 1)?;
        let fmt = open(&locale, style, capitalization, number_formatter).map_err(|c| {
            Error::Intl(
                IntlError::new(c, "Failed to create IntlRelativeDateTimeFormatter").throwing(),
            )
        })?;
        Ok(Self {
            fmt,
            locale,
            style,
            capitalization,
            nf_version: number_formatter.map(|n| n.configuration_version),
        })
    }

    /// The locale the formatter was created for.
    #[must_use]
    pub fn locale(&self) -> &str {
        &self.locale
    }

    /// `reldateformatter_refresh_number_formatter()`.
    fn refresh(&mut self, nf: Option<&NumberFormatter>) -> Result<()> {
        let (Some(nf), Some(v)) = (nf, self.nf_version) else {
            return Ok(());
        };
        if nf.configuration_version == v {
            return Ok(());
        }
        self.fmt = open(&self.locale, self.style, self.capitalization, Some(nf)).map_err(|c| {
            Error::intl(
                c,
                "Failed to update IntlRelativeDateTimeFormatter from NumberFormatter",
            )
        })?;
        self.nf_version = Some(nf.configuration_version);
        Ok(())
    }

    fn finish(&self, out: Vec<u16>, capitalize: bool) -> Result<Vec<u8>> {
        let mut out = out;
        if capitalize
            && self.capitalization == Capitalization::BeginningOfSentence
            && !out.is_empty()
        {
            let first = char::decode_utf16(out.iter().copied())
                .next()
                .and_then(std::result::Result::ok)
                .map_or(u32::from(out[0]), u32::from);
            if ureldatefmt::is_lower(first) {
                out = ureldatefmt::title_sentences(&out, &self.locale)
                    .map_err(|c| Error::intl(c, "Failed to convert result to UTF-8"))?;
            }
        }
        ustr::from_u16(&out)
            .map(String::into_bytes)
            .map_err(|c| Error::intl(c, "Failed to convert result to UTF-8"))
    }

    /// `format($offset, $unit)`; `number_formatter` is the bound
    /// `NumberFormatter` object (if any).
    pub fn format(
        &mut self,
        offset: f64,
        unit: Unit,
        number_formatter: Option<&NumberFormatter>,
    ) -> Result<Vec<u8>> {
        self.refresh(number_formatter)?;
        let out = self
            .fmt
            .format(offset, unit.icu())
            .map_err(|c| Error::intl(c, "Failed to format relative date/time"))?;
        self.finish(out, true)
    }

    /// `formatNumeric($offset, $unit)`.
    pub fn format_numeric(
        &mut self,
        offset: f64,
        unit: Unit,
        number_formatter: Option<&NumberFormatter>,
    ) -> Result<Vec<u8>> {
        self.refresh(number_formatter)?;
        let out = self
            .fmt
            .format_numeric(offset, unit.icu())
            .map_err(|c| Error::intl(c, "Failed to format relative date/time numerically"))?;
        self.finish(out, true)
    }

    /// `combineDateAndTime($relativeDate, $time)`.
    pub fn combine_date_and_time(&self, relative_date: &[u8], time: &[u8]) -> Result<Vec<u8>> {
        let d = ustr::to_u16(relative_date)
            .map_err(|c| Error::intl(c, "Failed to convert relative date to UTF-16"))?;
        let t =
            ustr::to_u16(time).map_err(|c| Error::intl(c, "Failed to convert time to UTF-16"))?;
        let out = self
            .fmt
            .combine(&d, &t)
            .map_err(|c| Error::intl(c, "Failed to combine relative date and time"))?;
        self.finish(out, false)
    }
}
