//! `MessageFormatter` / `msgfmt_*` (php-src `ext/intl/msgformat`).
//!
//! Patterns are validated by ICU (`umsg_open`), so syntax errors, type
//! conflicts and sub-format creation failures are reported exactly as ICU
//! does.  Formatting and parsing follow ICU's `MessageFormat` algorithm
//! (ported in Rust on top of [`crate::msgpattern`]) with ICU's number, date,
//! RBNF and plural-rule services doing the actual work, and php-src's
//! argument coercions (`umsg_format_helper`) in front.

#![allow(clippy::similar_names, clippy::match_same_arms)]

use std::collections::HashMap;

use crate::calendar::{IntlCalendar, PhpDateTime};
use crate::common;
use crate::dateformat::{self, DateArg};
use crate::error::{Error, IntlError, Result, UErrorCode};
use crate::ffi::UParseError;
use crate::msgpattern::{ArgType, MessagePattern, PartType};
use crate::sys::udat::DateFormat;
use crate::sys::udatpg::PatternGenerator;
use crate::sys::umsg;
use crate::sys::unum::{NumberFormat, Parsed as NumParsed};
use crate::sys::unumf::SkeletonFormatter;
use crate::sys::uplrules::PluralRules;
use crate::ustr;

/// `Formattable::Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FType {
    Date,
    Double,
    Long,
    String,
    Int64,
    Object,
}

/// A `Formattable` value.
#[derive(Debug, Clone, PartialEq)]
enum Fmt {
    Date(f64),
    Double(f64),
    Long(i32),
    Int64(i64),
    Str(Vec<u16>),
}

impl Fmt {
    fn is_numeric(&self) -> bool {
        matches!(self, Self::Double(_) | Self::Long(_) | Self::Int64(_))
    }

    #[allow(clippy::cast_precision_loss)]
    fn as_double(&self) -> Option<f64> {
        match self {
            Self::Double(d) => Some(*d),
            Self::Long(l) => Some(f64::from(*l)),
            Self::Int64(l) => Some(*l as f64),
            _ => None,
        }
    }
}

/// A cached sub-format of a simple argument.
#[derive(Debug)]
enum SubFormat {
    Number(NumberFormat),
    Skeleton(SkeletonFormatter),
    Date(DateFormat),
}

/// Argument identifiers in a pattern.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ArgId {
    Num(i64),
    Name(Vec<u16>),
}

/// A PHP array key of the `$values` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgKey<'a> {
    /// An integer key.
    Index(i64),
    /// A string key (raw bytes).
    Name(&'a [u8]),
}

/// A PHP value passed to `format()`.
#[derive(Debug, Clone, Copy)]
pub enum MsgValue<'a> {
    /// `null`
    Null,
    /// `bool`
    Bool(bool),
    /// `int`
    Int(i64),
    /// `float`
    Float(f64),
    /// `string`
    Str(&'a [u8]),
    /// A `DateTimeInterface`.
    DateTime(&'a PhpDateTime),
    /// An `IntlCalendar`.
    Calendar(&'a IntlCalendar),
    /// Any other object: its class name and, when it has `__toString`,
    /// the string conversion.
    Object {
        /// Class name.
        class: &'a str,
        /// `__toString()` result.
        as_string: Option<&'a [u8]>,
    },
    /// An array.
    Array {
        /// Whether it has elements (for numeric conversions).
        non_empty: bool,
    },
    /// A resource or anything else.
    Other,
}

impl MsgValue<'_> {
    /// `zval_get_string()` (`None` = conversion throws).
    fn to_php_string(self) -> std::result::Result<Vec<u8>, Error> {
        Ok(match self {
            Self::Null | Self::Bool(false) => Vec::new(),
            Self::Bool(true) => b"1".to_vec(),
            Self::Int(i) => i.to_string().into_bytes(),
            Self::Float(f) => common::php_float_to_string(f).into_bytes(),
            Self::Str(s) => s.to_vec(),
            Self::Array { .. } => b"Array".to_vec(),
            Self::Other => b"Resource id #0".to_vec(),
            Self::Object {
                as_string: Some(s), ..
            } => s.to_vec(),
            Self::Object {
                class,
                as_string: None,
            } => {
                return Err(Error::Thrown(format!(
                    "Object of class {class} could not be converted to string"
                )))
            }
            Self::DateTime(_) => {
                return Err(Error::Thrown(
                    "Object of class DateTime could not be converted to string".into(),
                ))
            }
            Self::Calendar(_) => {
                return Err(Error::Thrown(
                    "Object of class IntlCalendar could not be converted to string".into(),
                ))
            }
        })
    }

    /// `zval_get_double()`.
    #[allow(clippy::cast_precision_loss)]
    fn to_double(self) -> f64 {
        match self {
            Self::Null | Self::Bool(false) | Self::Other => 0.0,
            Self::Bool(true) => 1.0,
            Self::Int(i) => i as f64,
            Self::Float(f) => f,
            Self::Str(s) => common::php_strtod(s),
            Self::Array { non_empty } => f64::from(u8::from(non_empty)),
            Self::Object { .. } | Self::DateTime(_) | Self::Calendar(_) => 1.0,
        }
    }

    /// `zval_get_long()`.
    fn to_long(self) -> i64 {
        match self {
            Self::Null | Self::Bool(false) | Self::Other => 0,
            Self::Bool(true) => 1,
            Self::Int(i) => i,
            Self::Float(f) => crate::numfmt::dval_to_lval(f),
            Self::Str(s) => common::php_str_to_long(s),
            Self::Array { non_empty } => i64::from(non_empty),
            Self::Object { .. } | Self::DateTime(_) | Self::Calendar(_) => 1,
        }
    }
}

/// A value returned by `MessageFormatter::parse()`.
#[derive(Debug, Clone, PartialEq)]
pub enum MsgParsed {
    /// `int`
    Int(i64),
    /// `float`
    Float(f64),
    /// `string`
    Str(String),
}

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn is_white(c: u16) -> bool {
    crate::sys::uchar::has_binary_property(u32::from(c), crate::sys::uchar::PATTERN_WHITE_SPACE)
}

fn trim(s: &[u16]) -> &[u16] {
    let a = s.iter().position(|&c| !is_white(c)).unwrap_or(s.len());
    let b = s.iter().rposition(|&c| !is_white(c)).map_or(a, |e| e + 1);
    &s[a..b]
}

/// `findKeyword()`: trimmed, ASCII-lowercased comparison.
fn find_keyword(s: &[u16], list: &[&str]) -> Option<usize> {
    let t: String = String::from_utf16_lossy(trim(s)).to_lowercase();
    list.iter().position(|k| *k == t)
}

/// The compiled pattern and its sub-formats.
#[derive(Debug)]
struct Compiled {
    pat: MessagePattern,
    formats: HashMap<usize, SubFormat>,
    /// `getArgTypeList()` (numbered arguments).
    arg_types: Vec<FType>,
    /// Top-level `ARG_START` part indexes of date/time sub-formats.
    top_level_dates: Vec<usize>,
}

impl Compiled {
    fn new(pattern: &[u16], locale: &str) -> std::result::Result<Self, UErrorCode> {
        let pat = MessagePattern::parse(pattern)?;
        let mut formats = HashMap::new();
        let mut arg_types: Vec<FType> = Vec::new();
        let parts = pat.parts().to_vec();
        let mut i = 1;
        while i < parts.len() {
            let part = parts[i];
            if part.ty != PartType::ArgStart {
                i += 1;
                continue;
            }
            let arg_number = if parts[i + 1].ty == PartType::ArgNumber {
                Some(parts[i + 1].value)
            } else {
                None
            };
            let ty = match part.arg_type {
                ArgType::None | ArgType::Select => FType::String,
                ArgType::Choice | ArgType::Plural | ArgType::SelectOrdinal => FType::Double,
                ArgType::Simple => {
                    let ty_part = parts[i + 2];
                    let style = if parts[i + 3].ty == PartType::ArgStyle {
                        Some(pat.substring(&parts[i + 3]))
                    } else {
                        None
                    };
                    let (f, t) = create_format(pat.substring(&ty_part), style, locale)?;
                    formats.insert(i, f);
                    t
                }
            };
            if let Some(n) = arg_number.and_then(|n| usize::try_from(n).ok()) {
                if arg_types.len() <= n {
                    arg_types.resize(n + 1, FType::Object);
                }
                arg_types[n] = ty;
            }
            i += 1;
        }
        let mut top_level_dates = Vec::new();
        let mut i = 1;
        while i < parts.len() {
            if parts[i].ty == PartType::ArgStart {
                if matches!(formats.get(&i), Some(SubFormat::Date(_))) {
                    top_level_dates.push(i);
                }
                i = pat.limit_part_index(i);
            }
            i += 1;
        }
        Ok(Self {
            pat,
            formats,
            arg_types,
            top_level_dates,
        })
    }
}

/// `MessageFormat::createAppropriateFormat()`.
fn create_format(
    ty: &[u16],
    style: Option<&[u16]>,
    locale: &str,
) -> std::result::Result<(SubFormat, FType), UErrorCode> {
    let empty: [u16; 0] = [];
    let style = style.unwrap_or(&empty);
    let skel = |s: &[u16]| -> Option<Vec<u16>> {
        let start = s.iter().position(|&c| !is_white(c)).unwrap_or(s.len());
        let rest = &s[start..];
        (rest.len() >= 2 && rest[0] == u16::from(b':') && rest[1] == u16::from(b':'))
            .then(|| rest[2..].to_vec())
    };
    match find_keyword(
        ty,
        &["number", "date", "time", "spellout", "ordinal", "duration"],
    ) {
        Some(0) => {
            let kw = find_keyword(style, &["", "currency", "percent", "integer"]);
            Ok(match kw {
                Some(0) => (
                    SubFormat::Number(NumberFormat::open(1, None, locale).map_err(|e| e.0)?),
                    FType::Double,
                ),
                Some(1) => (
                    SubFormat::Number(NumberFormat::open(2, None, locale).map_err(|e| e.0)?),
                    FType::Double,
                ),
                Some(2) => (
                    SubFormat::Number(NumberFormat::open(3, None, locale).map_err(|e| e.0)?),
                    FType::Double,
                ),
                Some(_) => {
                    let mut nf = NumberFormat::open(1, None, locale).map_err(|e| e.0)?;
                    nf.set_attribute(6, 0); // UNUM_MAX_FRACTION_DIGITS
                    nf.set_attribute(2, 0); // UNUM_DECIMAL_ALWAYS_SHOWN
                    nf.set_attribute(0, 1); // UNUM_PARSE_INT_ONLY
                    (SubFormat::Number(nf), FType::Long)
                }
                None => {
                    if let Some(sk) = skel(style) {
                        (
                            SubFormat::Skeleton(
                                SkeletonFormatter::open(&sk, locale).map_err(|e| e.0)?,
                            ),
                            FType::Double,
                        )
                    } else {
                        let mut nf = NumberFormat::open(1, None, locale).map_err(|e| e.0)?;
                        nf.apply_pattern(style).map_err(|e| e.0)?;
                        (SubFormat::Number(nf), FType::Double)
                    }
                }
            })
        }
        Some(t @ (1 | 2)) => {
            // DATE_STYLES: default(medium), short, medium, long, full
            let id = find_keyword(style, &["", "short", "medium", "long", "full"]);
            let st = match id {
                Some(1) => 3,
                Some(3) => 1,
                Some(4) => 0,
                _ => 2,
            };
            let mut df = if t == 1 {
                DateFormat::open(-1, st, Some(locale), None, None)?
            } else {
                DateFormat::open(st, -1, Some(locale), None, None)?
            };
            if id.is_none() {
                if let Some(sk) = skel(style) {
                    let mut gen = PatternGenerator::open(locale)?;
                    let p = gen.best_pattern(&sk)?;
                    df = DateFormat::open(-2, -2, Some(locale), None, Some(&p))?;
                } else {
                    df.apply_pattern(false, style);
                }
            }
            Ok((SubFormat::Date(df), FType::Date))
        }
        Some(k) => {
            let st = match k {
                3 => 5,
                4 => 6,
                _ => 7,
            };
            let mut nf = NumberFormat::open(st, None, locale).map_err(|e| e.0)?;
            let rs = trim(style);
            if !rs.is_empty() {
                let _ = nf.set_text_attribute(6, rs);
            }
            Ok((SubFormat::Number(nf), FType::Double))
        }
        None => Err(UErrorCode::ILLEGAL_ARGUMENT_ERROR),
    }
}

/// A PHP `MessageFormatter` object.
#[derive(Debug)]
pub struct MessageFormatter {
    icu: umsg::MessageFormat,
    locale: String,
    orig: Vec<u8>,
    compiled: Compiled,
    /// php-src's cached `arg_types` (named mode; `None` until computed).
    named_types: Option<HashMap<ArgId, FType>>,
    tz_set: bool,
    default_nf: Option<NumberFormat>,
    default_df: Option<DateFormat>,
    cardinal: Option<PluralRules>,
    ordinal: Option<PluralRules>,
}

fn syntax_error(pe: &UParseError) -> String {
    format!(
        "pattern syntax error ({})",
        crate::error::parse_error_to_string(pe)
    )
}

impl MessageFormatter {
    fn build(
        locale: &str,
        pattern: &[u8],
    ) -> std::result::Result<Self, (UErrorCode, UParseError, bool)> {
        let upat = ustr::to_u16(pattern).map_err(|c| (c, UParseError::default(), true))?;
        let locale = common::locale_or_default(locale);
        let icu = umsg::MessageFormat::open(&upat, &locale).map_err(|(c, pe)| (c, pe, false))?;
        let compiled =
            Compiled::new(&upat, &locale).map_err(|c| (c, UParseError::default(), false))?;
        Ok(Self {
            icu,
            locale,
            orig: pattern.to_vec(),
            compiled,
            named_types: None,
            tz_set: false,
            default_nf: None,
            default_df: None,
            cardinal: None,
            ordinal: None,
        })
    }

    /// `new MessageFormatter($locale, $pattern)` / `msgfmt_create()`.
    pub fn new(locale: &str, pattern: &[u8]) -> Result<Self> {
        common::check_locale_len(locale)
            .map_err(|e| crate::timezone::rescope(e, crate::Scope::Global))?;
        Self::build(locale, pattern).map_err(|(c, pe, conv)| {
            if conv {
                Error::intl(c, "error converting pattern to UTF-16")
            } else if c == UErrorCode::PATTERN_SYNTAX_ERROR {
                Error::intl(c, syntax_error(&pe))
            } else {
                Error::intl(c, "message formatter creation failed")
            }
        })
    }

    /// Clone (`clone $fmt`).
    pub fn try_clone(&self) -> Result<Self> {
        let upat = ustr::to_u16(&self.orig).unwrap_or_default();
        let icu = self
            .icu
            .try_clone()
            .map_err(|_| Error::Thrown("Failed to clone MessageFormatter".into()))?;
        let compiled = Compiled::new(self.compiled.pat.pattern(), &self.locale)
            .or_else(|_| Compiled::new(&upat, &self.locale))
            .map_err(|_| Error::Thrown("Failed to clone MessageFormatter".into()))?;
        Ok(Self {
            icu,
            locale: self.locale.clone(),
            orig: self.orig.clone(),
            compiled,
            named_types: None,
            tz_set: false,
            default_nf: None,
            default_df: None,
            cardinal: None,
            ordinal: None,
        })
    }

    /// `getPattern()`.
    pub fn pattern(&self) -> &[u8] {
        &self.orig
    }

    /// `getLocale()` (`umsg_getLocale`).
    pub fn locale(&self) -> String {
        self.icu.locale()
    }

    /// `setPattern($pattern)`.
    pub fn set_pattern(&mut self, pattern: &[u8]) -> Result<()> {
        let upat = ustr::to_u16(pattern)
            .map_err(|c| Error::intl(c, "Error converting pattern to UTF-16"))?;
        if let Err((c, pe)) = self.icu.apply_pattern(&upat) {
            // ICU resets the formatter to an empty pattern on failure.
            if let Ok(c2) = Compiled::new(&[], &self.locale) {
                self.compiled = c2;
            }
            return Err(Error::object_code(
                c,
                format!(
                    "Error setting symbol value at line {}, offset {}",
                    pe.line, pe.offset
                ),
            ));
        }
        self.compiled = Compiled::new(&upat, &self.locale)
            .map_err(|c| Error::intl(c, "Error setting pattern"))?;
        self.orig = pattern.to_vec();
        // php-src keeps `tz_set`, so new date sub-formats keep ICU's
        // default time zone.
        self.named_types = None;
        Ok(())
    }

    /// php-src's `umsg_parse_format()` type table for named patterns.
    fn named_type_table(&mut self) -> Result<&HashMap<ArgId, FType>> {
        if self.named_types.is_none() {
            let pat = &self.compiled.pat;
            let parts = pat.parts();
            let mut ret: HashMap<ArgId, FType> = HashMap::new();
            let n = parts.len();
            let mut i = 0;
            while i + 2 < n {
                if parts[i].ty != PartType::ArgStart {
                    i += 1;
                    continue;
                }
                let arg_type = parts[i].arg_type;
                i += 1;
                let name_part = parts[i];
                let id = match name_part.ty {
                    PartType::ArgName => ArgId::Name(pat.substring(&name_part).to_vec()),
                    PartType::ArgNumber => ArgId::Num(i64::from(name_part.value)),
                    _ => {
                        return Err(Error::intl(
                            UErrorCode::INVALID_FORMAT_ERROR,
                            "Invalid part type encountered",
                        ))
                    }
                };
                let ty = match arg_type {
                    ArgType::None | ArgType::Select => FType::String,
                    ArgType::Plural | ArgType::Choice | ArgType::SelectOrdinal => FType::Double,
                    ArgType::Simple => {
                        i += 1;
                        let tp = parts[i];
                        let ts = String::from_utf16_lossy(pat.substring(&tp));
                        match ts.as_str() {
                            "number" => {
                                let sp = parts[i + 1];
                                if sp.ty == PartType::ArgStyle
                                    && pat.substring(&sp) == u("integer").as_slice()
                                {
                                    FType::Int64
                                } else {
                                    FType::Double
                                }
                            }
                            "date" | "time" => FType::Date,
                            "spellout" | "ordinal" | "duration" => FType::Double,
                            _ => FType::String,
                        }
                    }
                };
                let stored = ret.entry(id).or_insert(FType::Object);
                if *stored != FType::Object && *stored != ty {
                    return Err(Error::intl(
                        UErrorCode::ARGUMENT_TYPE_MISMATCH,
                        "Inconsistent types declared for an argument",
                    ));
                }
                *stored = ty;
                i += 1;
            }
            self.named_types = Some(ret);
        }
        Ok(self.named_types.as_ref().expect("computed above"))
    }

    fn arg_type_for(&mut self, key: &ArgId) -> Result<FType> {
        if self.compiled.pat.has_named_arguments() {
            Ok(self
                .named_type_table()?
                .get(key)
                .copied()
                .unwrap_or(FType::Object))
        } else {
            Ok(match key {
                ArgId::Num(n) => usize::try_from(*n)
                    .ok()
                    .and_then(|n| self.compiled.arg_types.get(n).copied())
                    .unwrap_or(FType::Object),
                ArgId::Name(_) => FType::Object,
            })
        }
    }

    /// `format($values)`; `php_default_tz` is PHP's default time zone,
    /// applied to the top-level date/time sub-formats.
    pub fn format(
        &mut self,
        args: &[(MsgKey<'_>, MsgValue<'_>)],
        php_default_tz: &str,
    ) -> Result<String> {
        if self.compiled.pat.has_named_arguments() {
            self.named_type_table()?;
        }
        if !self.tz_set {
            let tz = crate::timezone::TimeZone::create_u16(&ustr::str_u16(php_default_tz));
            for idx in self.compiled.top_level_dates.clone() {
                if let Some(SubFormat::Date(df)) = self.compiled.formats.get_mut(&idx) {
                    if let Ok(mut cal) = df.calendar() {
                        if cal.set_time_zone(tz.id_u16()).is_ok() {
                            df.set_calendar(&cal);
                        }
                    }
                }
            }
            self.tz_set = true;
        }
        let mut names: Vec<Vec<u16>> = Vec::with_capacity(args.len());
        let mut values: Vec<Fmt> = Vec::with_capacity(args.len());
        for (key, val) in args {
            let (id, key_u) = match key {
                MsgKey::Index(n) => {
                    if !(0..=i64::from(i32::MAX)).contains(n) {
                        return Err(Error::intl(
                            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                            "Found negative or too large array key",
                        ));
                    }
                    (ArgId::Num(*n), u(&n.to_string()))
                }
                MsgKey::Name(s) => {
                    let k = ustr::to_u16(s).map_err(|c| {
                        Error::Intl(IntlError::from_bytes(
                            c,
                            [&b"Invalid UTF-8 data in argument key: '"[..], s, b"'"].concat(),
                        ))
                    })?;
                    (ArgId::Name(k.clone()), k)
                }
            };
            let ty = self.arg_type_for(&id)?;
            let f = convert_arg(ty, *val, &key_u)?;
            names.push(key_u);
            values.push(f);
        }
        let mut out = Vec::new();
        let ctx = Args {
            names: &names,
            values: &values,
        };
        self.format_part(0, None, &ctx, &mut out)
            .map_err(|c| Error::intl(c, "Call to ICU MessageFormat::format() has failed"))?;
        ustr::from_u16(&out).map_err(|c| Error::intl(c, "Error converting value to UTF-8"))
    }

    fn default_nf(&mut self) -> std::result::Result<&NumberFormat, UErrorCode> {
        if self.default_nf.is_none() {
            self.default_nf = Some(NumberFormat::open(1, None, &self.locale).map_err(|e| e.0)?);
        }
        Ok(self.default_nf.as_ref().expect("set"))
    }

    fn format_with(
        &mut self,
        idx: Option<usize>,
        v: &Fmt,
        out: &mut Vec<u16>,
    ) -> std::result::Result<(), UErrorCode> {
        let Some(sub) = idx.and_then(|i| self.compiled.formats.get(&i)) else {
            if v.is_numeric() {
                let nf = self.default_nf()?;
                out.extend(format_number(nf, v)?);
            } else if let Fmt::Date(d) = v {
                if self.default_df.is_none() {
                    self.default_df = Some(DateFormat::open(3, 3, Some(&self.locale), None, None)?);
                }
                out.extend(self.default_df.as_ref().expect("set").format(*d)?);
            } else if let Fmt::Str(s) = v {
                out.extend_from_slice(s);
            }
            return Ok(());
        };
        match sub {
            SubFormat::Number(nf) => out.extend(format_number(nf, v)?),
            SubFormat::Skeleton(sf) => out.extend(match v {
                Fmt::Double(d) => sf.format_f64(*d)?,
                Fmt::Long(l) => sf.format_i64(i64::from(*l))?,
                Fmt::Int64(l) => sf.format_i64(*l)?,
                _ => return Err(UErrorCode::ILLEGAL_ARGUMENT_ERROR),
            }),
            SubFormat::Date(df) => {
                #[allow(clippy::cast_precision_loss)]
                let d = match v {
                    Fmt::Date(d) | Fmt::Double(d) => *d,
                    Fmt::Long(l) => f64::from(*l),
                    _ => return Err(UErrorCode::ILLEGAL_ARGUMENT_ERROR),
                };
                out.extend(df.format(d)?);
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn format_part(
        &mut self,
        msg_start: usize,
        plural: Option<&PluralCtx>,
        args: &Args<'_>,
        out: &mut Vec<u16>,
    ) -> std::result::Result<(), UErrorCode> {
        let mut prev = self.compiled.pat.part(msg_start).limit();
        let mut i = msg_start + 1;
        loop {
            let part = *self.compiled.pat.part(i);
            out.extend_from_slice(&self.compiled.pat.pattern()[prev..part.index]);
            if part.ty == PartType::MsgLimit {
                return Ok(());
            }
            prev = part.limit();
            match part.ty {
                PartType::SkipSyntax | PartType::InsertChar => {
                    i += 1;
                    continue;
                }
                PartType::ReplaceNumber => {
                    if let Some(p) = plural {
                        let v = p.number.clone();
                        let nf = self.default_nf()?;
                        out.extend(format_number(nf, &v)?);
                    }
                    i += 1;
                    continue;
                }
                PartType::ArgStart => {}
                _ => {
                    i += 1;
                    continue;
                }
            }
            let arg_limit = self.compiled.pat.limit_part_index(i);
            let arg_type = part.arg_type;
            let arg_start = i;
            i += 1;
            let name_part = *self.compiled.pat.part(i);
            let arg_name = self.compiled.pat.substring(&name_part).to_vec();
            i += 1;
            let arg = args
                .names
                .iter()
                .position(|n| *n == arg_name)
                .map(|k| &args.values[k]);
            match arg {
                None => {
                    out.push(u16::from(b'{'));
                    out.extend_from_slice(&arg_name);
                    out.push(u16::from(b'}'));
                }
                Some(arg) => {
                    if self.compiled.formats.contains_key(&arg_start) {
                        self.format_with(Some(arg_start), arg, out)?;
                    } else if arg_type == ArgType::None {
                        self.format_with(None, arg, out)?;
                    } else if arg_type == ArgType::Choice {
                        let number = arg.as_double().ok_or(UErrorCode::ILLEGAL_ARGUMENT_ERROR)?;
                        let sub = choice_find_sub_message(&self.compiled.pat, i, number);
                        self.format_part(sub, None, args, out)?;
                    } else if arg_type.has_plural_style() {
                        let number = arg.as_double().ok_or(UErrorCode::ILLEGAL_ARGUMENT_ERROR)?;
                        let offset = self.compiled.pat.plural_offset(i);
                        let ctx_number = if offset == 0.0 {
                            arg.clone()
                        } else {
                            Fmt::Double(number - offset)
                        };
                        let ctx = PluralCtx {
                            start: i,
                            name: arg_name.clone(),
                            number: ctx_number,
                        };
                        let sub = self.plural_find_sub_message(
                            i,
                            arg_type == ArgType::Plural,
                            &ctx,
                            number,
                        )?;
                        self.format_part(sub, Some(&ctx), args, out)?;
                    } else if arg_type == ArgType::Select {
                        let Fmt::Str(s) = arg else {
                            return Err(UErrorCode::INVALID_FORMAT_ERROR);
                        };
                        let sub = select_find_sub_message(&self.compiled.pat, i, s);
                        self.format_part(sub, None, args, out)?;
                    } else {
                        return Err(UErrorCode::INTERNAL_PROGRAM_ERROR);
                    }
                }
            }
            prev = self.compiled.pat.part(arg_limit).limit();
            i = arg_limit + 1;
        }
    }

    fn plural_rules(&mut self, cardinal: bool) -> std::result::Result<&PluralRules, UErrorCode> {
        let slot = if cardinal {
            &mut self.cardinal
        } else {
            &mut self.ordinal
        };
        if slot.is_none() {
            *slot = Some(PluralRules::open(&self.locale, i32::from(!cardinal))?);
        }
        Ok(slot.as_ref().expect("set"))
    }

    /// The selector: the plural keyword for `number` (= arg - offset),
    /// formatted like the first `{arg,number,...}` in the "other" message.
    fn plural_select(
        &mut self,
        cardinal: bool,
        ctx: &PluralCtx,
        number: f64,
    ) -> std::result::Result<Vec<u16>, UErrorCode> {
        let pat = &self.compiled.pat;
        let other_index = find_other_sub_message(pat, ctx.start);
        let num_arg = find_first_plural_number_arg(pat, other_index, &ctx.name);
        let fmt_idx = (num_arg > 0).then(|| usize::try_from(num_arg).unwrap_or(0));
        let use_cached = fmt_idx.is_some_and(|k| self.compiled.formats.contains_key(&k));
        if use_cached {
            let k = fmt_idx.unwrap_or(0);
            // Borrow juggling: take the rules first.
            self.plural_rules(cardinal)?;
            let rules = if cardinal {
                self.cardinal.as_ref()
            } else {
                self.ordinal.as_ref()
            }
            .ok_or(UErrorCode::INTERNAL_PROGRAM_ERROR)?;
            return match self.compiled.formats.get(&k) {
                Some(SubFormat::Number(nf)) => rules.select_with_format(number, nf),
                _ => rules.select(number),
            };
        }
        self.default_nf()?;
        self.plural_rules(cardinal)?;
        let rules = if cardinal {
            self.cardinal.as_ref()
        } else {
            self.ordinal.as_ref()
        }
        .ok_or(UErrorCode::INTERNAL_PROGRAM_ERROR)?;
        let nf = self
            .default_nf
            .as_ref()
            .ok_or(UErrorCode::INTERNAL_PROGRAM_ERROR)?;
        rules.select_with_format(number, nf)
    }

    /// `PluralFormat::findSubMessage()`.
    fn plural_find_sub_message(
        &mut self,
        mut part_index: usize,
        cardinal: bool,
        ctx: &PluralCtx,
        number: f64,
    ) -> std::result::Result<usize, UErrorCode> {
        let count = self.compiled.pat.count_parts();
        let offset;
        {
            let p = self.compiled.pat.part(part_index);
            if matches!(p.ty, PartType::ArgInt | PartType::ArgDouble) {
                offset = self.compiled.pat.numeric_value(p);
                part_index += 1;
            } else {
                offset = 0.0;
            }
        }
        let other = u("other");
        let mut keyword: Option<Vec<u16>> = None;
        let mut have_keyword_match = false;
        let mut msg_start = 0;
        loop {
            let part = *self.compiled.pat.part(part_index);
            part_index += 1;
            if part.ty == PartType::ArgLimit {
                break;
            }
            let next_ty = self.compiled.pat.part(part_index).ty;
            if matches!(next_ty, PartType::ArgInt | PartType::ArgDouble) {
                let vp = *self.compiled.pat.part(part_index);
                part_index += 1;
                #[allow(clippy::float_cmp)]
                if number == self.compiled.pat.numeric_value(&vp) {
                    return Ok(part_index);
                }
            } else if !have_keyword_match {
                if self.compiled.pat.part_matches(&part, &other) {
                    if msg_start == 0 {
                        msg_start = part_index;
                        if keyword.as_deref() == Some(&other[..]) {
                            have_keyword_match = true;
                        }
                    }
                } else {
                    if keyword.is_none() {
                        let k = self.plural_select(cardinal, ctx, number - offset)?;
                        if msg_start != 0 && k == other {
                            have_keyword_match = true;
                        }
                        keyword = Some(k);
                    }
                    if !have_keyword_match
                        && self
                            .compiled
                            .pat
                            .part_matches(&part, keyword.as_deref().unwrap_or(&[]))
                    {
                        msg_start = part_index;
                        have_keyword_match = true;
                    }
                }
            }
            part_index = self.compiled.pat.limit_part_index(part_index);
            part_index += 1;
            if part_index >= count {
                break;
            }
        }
        Ok(msg_start)
    }

    /// `parse($string)`.
    pub fn parse(&mut self, source: &[u8]) -> Result<Vec<MsgParsed>> {
        let src =
            ustr::to_u16(source).map_err(|c| Error::intl(c, "Converting parse string failed"))?;
        let r = self
            .parse_u16(&src)
            .map_err(|c| Error::intl(c, "Parsing failed"))?;
        Ok(r)
    }

    #[allow(clippy::too_many_lines)]
    fn parse_u16(&mut self, source: &[u16]) -> std::result::Result<Vec<MsgParsed>, UErrorCode> {
        let pat = &self.compiled.pat;
        if pat.has_named_arguments() {
            return Err(UErrorCode::ARGUMENT_TYPE_MISMATCH);
        }
        let n_types = self.compiled.arg_types.len().max(1);
        let mut results: Vec<Option<MsgParsed>> = vec![None; n_types];
        let mut count = 0usize;
        let msg = pat.pattern();
        let mut prev = pat.part(0).limit();
        let mut offset = 0usize;
        let mut i = 1;
        // Any mismatch leaves the parse position where it was; php then
        // reports U_MESSAGE_PARSE_ERROR (the index never advanced past 0
        // from the caller's point of view).
        let failed = |_off: usize| Err(UErrorCode::MESSAGE_PARSE_ERROR);
        loop {
            let part = *pat.part(i);
            let len = part.index - prev;
            if len != 0 {
                if source.get(offset..offset + len) != Some(&msg[prev..part.index]) {
                    return failed(offset);
                }
                offset += len;
            }
            if part.ty == PartType::MsgLimit {
                break;
            }
            if matches!(part.ty, PartType::SkipSyntax | PartType::InsertChar) {
                prev = part.limit();
                i += 1;
                continue;
            }
            let arg_limit = pat.limit_part_index(i);
            let arg_type = part.arg_type;
            let arg_start = i;
            let num_part = *pat.part(i + 1);
            let arg_number = usize::try_from(num_part.value).unwrap_or(0);
            i += 2;
            let mut have = false;
            let mut result: Option<MsgParsed> = None;
            if let Some(f) = self.compiled.formats.get(&arg_start) {
                let mut pos = i32::try_from(offset).unwrap_or(i32::MAX);
                let start = pos;
                let r = match f {
                    SubFormat::Number(nf) => {
                        nf.parse_formattable(source, &mut pos)
                            .ok()
                            .map(|p| match p {
                                NumParsed::Double(d) => MsgParsed::Float(d),
                                NumParsed::Long(l) => MsgParsed::Int(i64::from(l)),
                                NumParsed::Int64(l) => MsgParsed::Int(l),
                            })
                    }
                    SubFormat::Date(df) => df
                        .parse(source, &mut pos)
                        .ok()
                        .map(|d| MsgParsed::Float(d / 1000.0)),
                    SubFormat::Skeleton(_) => None,
                };
                if r.is_none() || pos == start {
                    return failed(offset);
                }
                offset = usize::try_from(pos).unwrap_or(offset);
                result = r;
                have = true;
            } else if arg_type == ArgType::None {
                let after = literal_until_next_argument(pat, arg_limit);
                let next = if after.is_empty() {
                    Some(source.len())
                } else {
                    find_sub(source, &after, offset)
                };
                let Some(next) = next else {
                    return failed(offset);
                };
                let value = &source[offset..next];
                let comp = u(&format!("{{{arg_number}}}"));
                if value != comp.as_slice() {
                    result = Some(MsgParsed::Str(String::from_utf16_lossy(value)));
                    have = true;
                }
                offset = next;
            } else if arg_type == ArgType::Choice {
                let (v, end) = choice_parse_argument(pat, i, source, offset);
                if end == offset {
                    return failed(offset);
                }
                result = Some(MsgParsed::Float(v));
                have = true;
                offset = end;
            } else {
                // Plural/select parsing is unsupported (U_UNSUPPORTED_ERROR
                // inside ICU), which the public API reports as a parse error.
                return Err(UErrorCode::MESSAGE_PARSE_ERROR);
            }
            if have {
                if results.len() <= arg_number {
                    results.resize(arg_number + 1, None);
                }
                results[arg_number] = result;
                count = count.max(arg_number + 1);
            }
            prev = pat.part(arg_limit).limit();
            i = arg_limit + 1;
        }
        if offset == 0 {
            return Err(UErrorCode::MESSAGE_PARSE_ERROR);
        }
        results.truncate(count);
        Ok(results
            .into_iter()
            .map(|r| r.unwrap_or(MsgParsed::Int(0)))
            .collect())
    }
}

/// Arguments for one `format()` call.
struct Args<'a> {
    names: &'a [Vec<u16>],
    values: &'a [Fmt],
}

/// `PluralSelectorContext` (the parts we need).
struct PluralCtx {
    start: usize,
    name: Vec<u16>,
    number: Fmt,
}

fn format_number(nf: &NumberFormat, v: &Fmt) -> std::result::Result<Vec<u16>, UErrorCode> {
    match v {
        Fmt::Double(d) => nf.format_f64(*d),
        Fmt::Long(l) => nf.format_i32(*l),
        Fmt::Int64(l) => nf.format_i64(*l),
        _ => Err(UErrorCode::INVALID_FORMAT_ERROR),
    }
}

/// php-src's per-argument coercion (`umsg_format_helper`).
fn convert_arg(ty: FType, val: MsgValue<'_>, key: &[u16]) -> Result<Fmt> {
    let key_s = String::from_utf16_lossy(key);
    let string_arg = |val: MsgValue<'_>| -> Result<Fmt> {
        let s = val.to_php_string()?;
        let us = ustr::to_u16(&s).map_err(|c| {
            Error::Intl(IntlError::from_bytes(
                c,
                [&b"Invalid UTF-8 data in string argument: '"[..], &s, b"'"].concat(),
            ))
        })?;
        Ok(Fmt::Str(us))
    };
    match ty {
        FType::String => string_arg(val),
        FType::Double => Ok(Fmt::Double(val.to_double())),
        FType::Long => {
            let v = match val {
                MsgValue::Float(d) => {
                    if d > f64::from(i32::MAX) || d < f64::from(i32::MIN) {
                        return Err(Error::intl(
                            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                            "Found PHP float with absolute value too large for 32 bit integer argument",
                        ));
                    }
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        d as i32
                    }
                }
                MsgValue::Int(l) => i32::try_from(l).map_err(|_| {
                    Error::intl(
                        UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                        "Found PHP integer with absolute value too large for 32 bit integer argument",
                    )
                })?,
                other => crate::numfmt::wrap_i32(other.to_long()),
            };
            Ok(Fmt::Long(v))
        }
        FType::Int64 => {
            let v = match val {
                MsgValue::Float(d) => {
                    #[allow(clippy::cast_precision_loss)]
                    if d > i64::MAX as f64 || d < i64::MIN as f64 {
                        return Err(Error::intl(
                            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                            "Found PHP float with absolute value too large for 64 bit integer argument",
                        ));
                    }
                    crate::numfmt::c_double_to_i64(d)
                }
                other => other.to_long(),
            };
            Ok(Fmt::Int64(v))
        }
        FType::Date => {
            let arg = match val {
                MsgValue::Int(i) => DateArg::Int(i),
                MsgValue::Float(f) => DateArg::Float(f),
                MsgValue::Str(s) => DateArg::Str(s),
                MsgValue::DateTime(d) => DateArg::DateTime(d),
                MsgValue::Calendar(c) => DateArg::Calendar(c),
                MsgValue::Object { .. } => DateArg::OtherObject,
                _ => DateArg::Other,
            };
            dateformat::zval_to_millis(arg).map(Fmt::Date).map_err(|e| {
                Error::intl(e.intl_code(), format!("The argument for key '{key_s}' cannot be used as a date or time"))
            })
        }
        FType::Object => match val {
            MsgValue::Float(d) => Ok(Fmt::Double(d)),
            MsgValue::Int(l) => Ok(Fmt::Int64(l)),
            MsgValue::Null | MsgValue::Bool(false) => Ok(Fmt::Int64(0)),
            MsgValue::Bool(true) => Ok(Fmt::Int64(1)),
            MsgValue::Str(_) | MsgValue::Object { .. } | MsgValue::DateTime(_) | MsgValue::Calendar(_) => {
                string_arg(val)
            }
            MsgValue::Array { .. } | MsgValue::Other => Err(Error::intl(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                format!("No strategy to convert the value given for the argument with key '{key_s}' is available"),
            )),
        },
    }
}

/// `ChoiceFormat::findSubMessage()`.
fn choice_find_sub_message(pat: &MessagePattern, mut part_index: usize, number: f64) -> usize {
    let count = pat.count_parts();
    part_index += 2;
    let mut msg_start;
    loop {
        msg_start = part_index;
        part_index = pat.limit_part_index(part_index);
        part_index += 1;
        if part_index >= count {
            break;
        }
        let part = *pat.part(part_index);
        part_index += 1;
        if part.ty == PartType::ArgLimit {
            break;
        }
        let boundary = pat.numeric_value(&part);
        let sel = pat.part(part_index).index;
        part_index += 1;
        let c = pat.pattern()[sel];
        // `!(a > b)` / `!(a >= b)` also "catch" NaN, like ICU.
        let stop = if c == u16::from(b'<') {
            number.partial_cmp(&boundary) != Some(std::cmp::Ordering::Greater)
        } else {
            !matches!(
                number.partial_cmp(&boundary),
                Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
            )
        };
        if stop {
            break;
        }
    }
    msg_start
}

/// `SelectFormat::findSubMessage()`.
fn select_find_sub_message(pat: &MessagePattern, mut part_index: usize, keyword: &[u16]) -> usize {
    let other = u("other");
    let count = pat.count_parts();
    let mut msg_start = 0;
    loop {
        let part = *pat.part(part_index);
        part_index += 1;
        if part.ty == PartType::ArgLimit {
            break;
        }
        if pat.part_matches(&part, keyword) {
            return part_index;
        } else if msg_start == 0 && pat.part_matches(&part, &other) {
            msg_start = part_index;
        }
        part_index = pat.limit_part_index(part_index);
        part_index += 1;
        if part_index >= count {
            break;
        }
    }
    msg_start
}

/// `MessageFormat::findOtherSubMessage()`.
fn find_other_sub_message(pat: &MessagePattern, mut part_index: usize) -> usize {
    let count = pat.count_parts();
    if matches!(
        pat.part(part_index).ty,
        PartType::ArgInt | PartType::ArgDouble
    ) {
        part_index += 1;
    }
    let other = u("other");
    loop {
        let part = *pat.part(part_index);
        part_index += 1;
        if part.ty == PartType::ArgLimit {
            break;
        }
        if pat.part_matches(&part, &other) {
            return part_index;
        }
        if matches!(
            pat.part(part_index).ty,
            PartType::ArgInt | PartType::ArgDouble
        ) {
            part_index += 1;
        }
        part_index = pat.limit_part_index(part_index);
        part_index += 1;
        if part_index >= count {
            break;
        }
    }
    0
}

/// `MessageFormat::findFirstPluralNumberArg()`.
fn find_first_plural_number_arg(pat: &MessagePattern, msg_start: usize, arg_name: &[u16]) -> i64 {
    let mut i = msg_start + 1;
    loop {
        let part = *pat.part(i);
        match part.ty {
            PartType::MsgLimit => return 0,
            PartType::ReplaceNumber => return -1,
            PartType::ArgStart => {
                if !arg_name.is_empty()
                    && matches!(part.arg_type, ArgType::None | ArgType::Simple)
                    && pat.part_matches(pat.part(i + 1), arg_name)
                {
                    return i64::try_from(i).unwrap_or(0);
                }
                i = pat.limit_part_index(i);
            }
            _ => {}
        }
        i += 1;
    }
}

/// `MessageFormat::getLiteralStringUntilNextArgument()`.
fn literal_until_next_argument(pat: &MessagePattern, from: usize) -> Vec<u16> {
    let msg = pat.pattern();
    let mut prev = pat.part(from).limit();
    let mut b = Vec::new();
    let mut i = from + 1;
    loop {
        let part = *pat.part(i);
        b.extend_from_slice(&msg[prev..part.index]);
        if matches!(part.ty, PartType::ArgStart | PartType::MsgLimit) {
            return b;
        }
        prev = part.limit();
        i += 1;
    }
}

fn find_sub(hay: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if from > hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// `ChoiceFormat::parseArgument()` → `(number, new index)`.
fn choice_parse_argument(
    pat: &MessagePattern,
    mut part_index: usize,
    source: &[u16],
    start: usize,
) -> (f64, usize) {
    let count = pat.count_parts();
    let mut furthest = start;
    let mut best = f64::NAN;
    while part_index < count && pat.part(part_index).ty != PartType::ArgLimit {
        let temp = pat.numeric_value(pat.part(part_index));
        part_index += 2;
        let msg_limit = pat.limit_part_index(part_index);
        if let Some(len) = match_string_until_limit_part(pat, part_index, msg_limit, source, start)
        {
            let new_index = start + len;
            if new_index > furthest {
                furthest = new_index;
                best = temp;
                if furthest == source.len() {
                    break;
                }
            }
        }
        part_index = msg_limit + 1;
    }
    (best, furthest)
}

fn match_string_until_limit_part(
    pat: &MessagePattern,
    mut part_index: usize,
    limit_part_index: usize,
    source: &[u16],
    source_offset: usize,
) -> Option<usize> {
    let msg = pat.pattern();
    let mut matching = 0;
    let mut prev = pat.part(part_index).limit();
    loop {
        part_index += 1;
        let part = *pat.part(part_index);
        if part_index == limit_part_index || part.ty == PartType::SkipSyntax {
            let len = part.index - prev;
            if len != 0
                && source.get(source_offset..source_offset + len) != Some(&msg[prev..part.index])
            {
                return None;
            }
            matching += len;
            if part_index == limit_part_index {
                return Some(matching);
            }
            prev = part.limit();
        }
    }
}

fn static_creation_error(c: UErrorCode, pe: &UParseError, conv: bool) -> Error {
    if conv {
        Error::global(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "error converting pattern to UTF-16",
        )
    } else if c == UErrorCode::PATTERN_SYNTAX_ERROR {
        Error::global(c, syntax_error(pe))
    } else {
        // php-src only sets the message here; the code stays U_ZERO_ERROR.
        Error::global(UErrorCode::ZERO, "Creating message formatter failed")
    }
}

/// `MessageFormatter::formatMessage($locale, $pattern, $values)`.
pub fn format_message(
    locale: &str,
    pattern: &[u8],
    args: &[(MsgKey<'_>, MsgValue<'_>)],
    php_default_tz: &str,
) -> Result<String> {
    common::check_locale_len(locale)
        .map_err(|e| crate::timezone::rescope(e, crate::Scope::Global))?;
    let mut mf = MessageFormatter::build(locale, pattern)
        .map_err(|(c, pe, conv)| static_creation_error(c, &pe, conv))?;
    mf.format(args, php_default_tz)
        .map_err(|e| crate::timezone::rescope(e, crate::Scope::Global))
}

/// `MessageFormatter::parseMessage($locale, $pattern, $message)`.
pub fn parse_message(locale: &str, pattern: &[u8], source: &[u8]) -> Result<Vec<MsgParsed>> {
    common::check_locale_len(locale)
        .map_err(|e| crate::timezone::rescope(e, crate::Scope::Global))?;
    let mut mf = MessageFormatter::build(locale, pattern).map_err(|(c, _, conv)| {
        if conv {
            Error::global(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                "error converting pattern to UTF-16",
            )
        } else {
            Error::global(c, "Creating message formatter failed")
        }
    })?;
    mf.parse(source)
        .map_err(|e| crate::timezone::rescope(e, crate::Scope::Global))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(loc: &str, p: &str, args: &[MsgValue<'_>]) -> String {
        let a: Vec<_> = args
            .iter()
            .enumerate()
            .map(|(i, v)| (MsgKey::Index(i64::try_from(i).unwrap()), *v))
            .collect();
        format_message(loc, p.as_bytes(), &a, "UTC").unwrap()
    }

    #[test]
    fn basic() {
        assert_eq!(
            fmt("en_US", "Hello {0}!", &[MsgValue::Str(b"World")]),
            "Hello World!"
        );
        assert_eq!(
            fmt("en_US", "{0,number,integer} items", &[MsgValue::Int(1234)]),
            "1,234 items"
        );
        assert_eq!(
            fmt("de", "{0,number}", &[MsgValue::Float(1234.5)]),
            "1.234,5"
        );
        assert_eq!(
            fmt(
                "en",
                "{0} {1} {2}",
                &[MsgValue::Float(1.5), MsgValue::Bool(true), MsgValue::Null]
            ),
            "1.5 1 "
        );
        assert_eq!(
            fmt("en", "it''s '{0}' {0}", &[MsgValue::Int(3)]),
            "it's {0} 3"
        );
    }

    #[test]
    fn plural_select_choice() {
        let p = "{0,plural,offset:1 =1{only} one{# other} other{{0,number,#.00} #}}";
        assert_eq!(fmt("en", p, &[MsgValue::Int(1)]), "only");
        assert_eq!(fmt("en", p, &[MsgValue::Int(2)]), "2.00 1");
        assert_eq!(fmt("en", p, &[MsgValue::Int(3)]), "3.00 2");
        let p = "{0,plural,one{# one} other{{0,number,#.00} other}}";
        assert_eq!(fmt("en", p, &[MsgValue::Int(1)]), "1.00 other");
        let p = "{0,select,male{he} female{she} other{they}}";
        assert_eq!(fmt("en", p, &[MsgValue::Str(b"female")]), "she");
        assert_eq!(fmt("en", p, &[MsgValue::Str(b"x")]), "they");
        let p = "{0,choice,0#none|1#one|1<many {0,number}}";
        assert_eq!(fmt("en", p, &[MsgValue::Float(0.0)]), "none");
        assert_eq!(fmt("en", p, &[MsgValue::Float(1.0)]), "one");
        assert_eq!(fmt("en", p, &[MsgValue::Float(1.5)]), "many 1.5");
        assert_eq!(
            fmt(
                "en",
                "{0,selectordinal,one{#st} two{#nd} few{#rd} other{#th}}",
                &[MsgValue::Int(22)]
            ),
            "22nd"
        );
    }

    #[test]
    fn named() {
        let a = [
            (MsgKey::Name(b"n"), MsgValue::Int(5)),
            (MsgKey::Name(b"who"), MsgValue::Str(b"Bob")),
        ];
        let r = format_message(
            "en",
            b"{who} has {n, plural, one{# cat} other{# cats}}",
            &a,
            "UTC",
        )
        .unwrap();
        assert_eq!(r, "Bob has 5 cats");
        let r = format_message("en", b"{missing} x", &a, "UTC").unwrap();
        assert_eq!(r, "{missing} x");
    }

    #[test]
    fn parse() {
        let r = parse_message(
            "en",
            b"{0} has {1,number} apples",
            b"Bob has 1,234.5 apples",
        )
        .unwrap();
        assert_eq!(
            r,
            vec![MsgParsed::Str("Bob".into()), MsgParsed::Float(1234.5)]
        );
        let e = parse_message("en", b"{a}", b"x").unwrap_err();
        assert_eq!(e.intl_code(), UErrorCode::ARGUMENT_TYPE_MISMATCH);
    }

    #[test]
    fn errors() {
        let e = MessageFormatter::new("en", b"{0,choice}").unwrap_err();
        assert_eq!(e.intl_code(), UErrorCode::PATTERN_SYNTAX_ERROR);
        assert!(e
            .intl_message()
            .unwrap()
            .starts_with("pattern syntax error ("));
        let e = format_message("en", b"{0,number} {0,date}", &[], "UTC").unwrap_err();
        assert_eq!(e.intl_message(), Some("Creating message formatter failed"));
    }
}
