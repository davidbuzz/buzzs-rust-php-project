//! `UConverter` (php-src `ext/intl/converter`).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::error::{Error, IntlError, Result, Scope, UErrorCode};
use crate::sys::ucnv::{self, ByteSink, CallbackCell, Callbacks, Converter, U16Sink};

/// `UConverter::REASON_*` (`UConverterCallbackReason`).
pub mod reason {
    /// `REASON_UNASSIGNED`
    pub const UNASSIGNED: i64 = 0;
    /// `REASON_ILLEGAL`
    pub const ILLEGAL: i64 = 1;
    /// `REASON_IRREGULAR`
    pub const IRREGULAR: i64 = 2;
    /// `REASON_RESET`
    pub const RESET: i64 = 3;
    /// `REASON_CLOSE`
    pub const CLOSE: i64 = 4;
    /// `REASON_CLONE`
    pub const CLONE: i64 = 5;
}

/// A value returned by a PHP `toUCallback()`/`fromUCallback()` override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackValue {
    /// The callback threw (nothing is appended).
    Undef,
    /// `null` — skip.
    Null,
    /// An `int` (a code point / byte).
    Int(i64),
    /// A string.
    Str(Vec<u8>),
    /// An array of further values.
    Array(Vec<CallbackValue>),
    /// Any other type (an error).
    Other,
}

/// The PHP side of a `UConverter` subclass: invokes its overridden
/// `toUCallback($reason, $source, $codeUnits, &$error)` and
/// `fromUCallback($reason, $source, $codePoint, &$error)` methods.
#[allow(clippy::wrong_self_convention)]
pub trait PhpCallbacks {
    /// Calls `toUCallback()`; `error` is the by-reference argument.
    fn to_u_callback(
        &mut self,
        reason: i64,
        source: &[u8],
        code_units: &[u8],
        error: &mut i64,
    ) -> CallbackValue;
    /// Calls `fromUCallback()`; `source` holds the code points.
    fn from_u_callback(
        &mut self,
        reason: i64,
        source: &[i64],
        code_point: i64,
        error: &mut i64,
    ) -> CallbackValue;
}

/// Errors raised while a conversion (or a callback) ran, in order.  The VM
/// records each like `intl_errors_set()`.
pub type ErrorLog = Arc<Mutex<Vec<IntlError>>>;

struct Bridge {
    php: Box<dyn PhpCallbacks>,
    log: ErrorLog,
}

impl Bridge {
    fn fail(&self, code: UErrorCode, msg: String) {
        if let Ok(mut l) = self.log.lock() {
            l.push(IntlError::new(code, msg));
        }
    }

    fn check(&self, available: i64, needed: i64) -> bool {
        if available < needed {
            self.fail(
                UErrorCode::BUFFER_OVERFLOW_ERROR,
                format!("Buffer overrun {needed} bytes needed, {available} available"),
            );
            return false;
        }
        true
    }

    fn append_u16(&self, value: &CallbackValue, sink: &mut U16Sink<'_>) {
        match value {
            CallbackValue::Undef | CallbackValue::Null => {}
            CallbackValue::Int(l) => {
                if !(0..=0x0010_ffff).contains(l) {
                    self.fail(
                        UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                        format!("Invalid codepoint U+{l:04x}"),
                    );
                    return;
                }
                if *l > 0xffff {
                    if self.check(sink.available(), 2) {
                        let c = u32::try_from(*l).unwrap_or(0) - 0x10000;
                        sink.push(u16::try_from(0xd800 + (c >> 10)).unwrap_or(0));
                        sink.push(u16::try_from(0xdc00 + (c & 0x3ff)).unwrap_or(0));
                    }
                    return;
                }
                if self.check(sink.available(), 1) {
                    sink.push(u16::try_from(*l).unwrap_or(0));
                }
            }
            CallbackValue::Str(s) => {
                let mut i = 0;
                while i != s.len() && self.check(sink.available(), 1) {
                    let (c, n) = crate::ustr::u8_next(&s[i..]);
                    i += n;
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    sink.push(c as u16);
                }
            }
            CallbackValue::Array(items) => {
                for it in items {
                    self.append_u16(it, sink);
                }
            }
            CallbackValue::Other => self.fail(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                "toUCallback() specified illegal type for substitution character".into(),
            ),
        }
    }

    fn append_bytes(&self, value: &CallbackValue, sink: &mut ByteSink<'_>) {
        match value {
            CallbackValue::Undef | CallbackValue::Null => {}
            CallbackValue::Int(l) => {
                if self.check(sink.available(), 1) {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    sink.push(&[*l as u8]);
                }
            }
            CallbackValue::Str(s) => {
                if self.check(sink.available(), i64::try_from(s.len()).unwrap_or(i64::MAX)) {
                    sink.push(s);
                }
            }
            CallbackValue::Array(items) => {
                for it in items {
                    self.append_bytes(it, sink);
                }
            }
            CallbackValue::Other => self.fail(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                "fromUCallback() specified illegal type for substitution character".into(),
            ),
        }
    }
}

impl Callbacks for Bridge {
    fn to_u(
        &mut self,
        reason: i32,
        source: &[u8],
        units: &[u8],
        error: &mut i32,
        sink: &mut U16Sink<'_>,
    ) {
        let mut e = i64::from(*error);
        let v = self
            .php
            .to_u_callback(i64::from(reason), source, units, &mut e);
        self.append_u16(&v, sink);
        #[allow(clippy::cast_possible_truncation)]
        {
            *error = e as i32;
        }
    }

    fn from_u(
        &mut self,
        reason: i32,
        units: &[u16],
        code_point: i32,
        error: &mut i32,
        sink: &mut ByteSink<'_>,
    ) {
        let source: Vec<i64> = char::decode_utf16(units.iter().copied())
            .map(|r| {
                r.map_or_else(
                    |e| i64::from(e.unpaired_surrogate()),
                    |c| i64::from(u32::from(c)),
                )
            })
            .collect();
        let mut e = i64::from(*error);
        let v = self
            .php
            .from_u_callback(i64::from(reason), &source, i64::from(code_point), &mut e);
        self.append_bytes(&v, sink);
        #[allow(clippy::cast_possible_truncation)]
        {
            *error = e as i32;
        }
    }
}

/// A PHP `UConverter` object.
pub struct UConverter {
    src: Option<Converter>,
    dest: Option<Converter>,
    php: Option<Rc<RefCell<Box<dyn PhpCallbacks>>>>,
    log: ErrorLog,
}

impl std::fmt::Debug for UConverter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UConverter")
            .field("src", &self.src)
            .field("dest", &self.dest)
            .finish_non_exhaustive()
    }
}

/// Shares one `PhpCallbacks` between the source and destination bridges.
struct Shared(Rc<RefCell<Box<dyn PhpCallbacks>>>);

impl PhpCallbacks for Shared {
    fn to_u_callback(
        &mut self,
        reason: i64,
        source: &[u8],
        code_units: &[u8],
        error: &mut i64,
    ) -> CallbackValue {
        self.0
            .try_borrow_mut()
            .map_or(CallbackValue::Undef, |mut p| {
                p.to_u_callback(reason, source, code_units, error)
            })
    }

    fn from_u_callback(
        &mut self,
        reason: i64,
        source: &[i64],
        code_point: i64,
        error: &mut i64,
    ) -> CallbackValue {
        self.0
            .try_borrow_mut()
            .map_or(CallbackValue::Undef, |mut p| {
                p.from_u_callback(reason, source, code_point, error)
            })
    }
}

fn ufailure(code: UErrorCode) -> IntlError {
    IntlError::new(code, format!("returned error {}: {}", code.0, code.name()))
}

impl UConverter {
    /// An empty object (before the constructor ran).  `callbacks` is the
    /// PHP object's overridden callback methods when the class is a
    /// subclass of `UConverter` (php-src only installs ICU callbacks then).
    pub fn new_empty(callbacks: Option<Box<dyn PhpCallbacks>>) -> Self {
        Self {
            src: None,
            dest: None,
            php: callbacks.map(|c| Rc::new(RefCell::new(c))),
            log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Takes the errors logged since the last call.
    pub fn take_errors(&self) -> Vec<IntlError> {
        self.log
            .lock()
            .map(|mut l| std::mem::take(&mut *l))
            .unwrap_or_default()
    }

    fn log_err(&self, e: IntlError) {
        if let Ok(mut l) = self.log.lock() {
            l.push(e);
        }
    }

    /// `php_converter_set_encoding()`; returns `false` on failure (errors
    /// in the log).  An ambiguous alias logs a global-only warning-coded
    /// message but succeeds.
    fn set_encoding(&mut self, dest: bool, enc: &str) -> bool {
        let (mut cnv, st) = match Converter::open(enc) {
            Ok(v) => v,
            Err(e) => {
                self.log_err(ufailure(e));
                return false;
            }
        };
        if st == UErrorCode::AMBIGUOUS_ALIAS_WARNING {
            let actual = cnv.name().unwrap_or_else(|_| "(unknown)".into());
            self.log_err(
                IntlError::new(st, format!("Ambiguous encoding specified, using {actual}"))
                    .with_scope(Scope::Global),
            );
        }
        if let Some(php) = &self.php {
            let bridge: CallbackCell = Box::new(Bridge {
                php: Box::new(Shared(php.clone())),
                log: self.log.clone(),
            });
            if let Err(e) = cnv.set_callbacks(bridge) {
                self.log_err(ufailure(e));
                return false;
            }
        }
        if dest {
            self.dest = Some(cnv);
        } else {
            self.src = Some(cnv);
        }
        true
    }

    /// `new UConverter($destination = "utf-8", $source = "utf-8")`.  On
    /// failure the logged errors are returned (the constructor throws the
    /// first as an `IntlException`).
    pub fn construct(
        &mut self,
        dest: Option<&str>,
        src: Option<&str>,
    ) -> std::result::Result<(), Vec<IntlError>> {
        if !self.set_encoding(false, src.unwrap_or("utf-8"))
            || !self.set_encoding(true, dest.unwrap_or("utf-8"))
        {
            return Err(self.take_errors());
        }
        Ok(())
    }

    /// `setSourceEncoding()` / `setDestinationEncoding()`.
    pub fn set_source_encoding(&mut self, enc: &str) -> bool {
        self.set_encoding(false, enc)
    }

    /// `setDestinationEncoding()`.
    pub fn set_destination_encoding(&mut self, enc: &str) -> bool {
        self.set_encoding(true, enc)
    }

    fn get_name(&self, c: Option<&Converter>) -> Option<std::result::Result<String, ()>> {
        let c = c?;
        Some(c.name().map_err(|e| self.log_err(ufailure(e))))
    }

    /// `getSourceEncoding()` (`None` → `null`, `Some(Err)` → `false`).
    pub fn source_encoding(&self) -> Option<std::result::Result<String, ()>> {
        self.get_name(self.src.as_ref())
    }

    /// `getDestinationEncoding()`.
    pub fn destination_encoding(&self) -> Option<std::result::Result<String, ()>> {
        self.get_name(self.dest.as_ref())
    }

    /// `getSourceType()` (`None` → `null`).
    pub fn source_type(&self) -> Option<i64> {
        self.src.as_ref().map(|c| i64::from(c.conv_type()))
    }

    /// `getDestinationType()`.
    pub fn destination_type(&self) -> Option<i64> {
        self.dest.as_ref().map(|c| i64::from(c.conv_type()))
    }

    /// `setSubstChars($chars)`.
    pub fn set_subst_chars(&mut self, chars: &[u8]) -> bool {
        let mut ok = true;
        let mut errs = Vec::new();
        match self.src.as_mut() {
            Some(c) => {
                if let Err(e) = c.set_subst_chars(chars) {
                    errs.push(ufailure(e));
                    ok = false;
                }
            }
            None => {
                errs.push(IntlError::new(
                    UErrorCode::INVALID_STATE_ERROR,
                    "Source Converter has not been initialized yet",
                ));
                ok = false;
            }
        }
        match self.dest.as_mut() {
            Some(c) => {
                if let Err(e) = c.set_subst_chars(chars) {
                    errs.push(ufailure(e));
                    ok = false;
                }
            }
            None => {
                errs.push(IntlError::new(
                    UErrorCode::INVALID_STATE_ERROR,
                    "Destination Converter has not been initialized yet",
                ));
                ok = false;
            }
        }
        for e in errs {
            self.log_err(e);
        }
        ok
    }

    /// `getSubstChars()` (`None` → `null`, `Some(Err)` → `false`).
    pub fn subst_chars(&self) -> Option<std::result::Result<Vec<u8>, ()>> {
        let c = self.src.as_ref()?;
        Some(c.subst_chars().map_err(|e| self.log_err(ufailure(e))))
    }

    /// The default `toUCallback()`/`fromUCallback()` implementation
    /// (`parent::toUCallback(...)`): returns the value and the new `$error`
    /// (`None` = leave it).
    pub fn default_callback(&self, reason: i64) -> (CallbackValue, Option<i64>) {
        if !matches!(
            reason,
            reason::UNASSIGNED | reason::ILLEGAL | reason::IRREGULAR
        ) {
            return (CallbackValue::Null, None);
        }
        let Some(src) = self.src.as_ref() else {
            self.log_err(IntlError::new(
                UErrorCode::INVALID_STATE_ERROR,
                "Source Converter has not been initialized yet",
            ));
            return (
                CallbackValue::Str(vec![0x1a]),
                Some(i64::from(UErrorCode::INVALID_STATE_ERROR.0)),
            );
        };
        match src.subst_chars() {
            Ok(c) => (CallbackValue::Str(c), Some(0)),
            Err(e) => {
                self.log_err(ufailure(e));
                (CallbackValue::Str(vec![0x1a]), Some(i64::from(e.0)))
            }
        }
    }

    /// `convert($str, $reverse)` (`None` → `false`; see [`Self::take_errors`]).
    pub fn convert(&mut self, s: &[u8], reverse: bool) -> Option<Vec<u8>> {
        let (from, to) = if reverse {
            (&mut self.dest, &mut self.src)
        } else {
            (&mut self.src, &mut self.dest)
        };
        let (Some(from), Some(to)) = (from.as_mut(), to.as_mut()) else {
            self.log_err(IntlError::new(
                UErrorCode::INVALID_STATE_ERROR,
                "Internal converters not initialized",
            ));
            return None;
        };
        let r = from.to_uchars(s).and_then(|u| to.from_uchars(&u));
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.log_err(ufailure(e));
                None
            }
        }
    }
}

/// `UConverter::reasonText($reason)`.
pub fn reason_text(r: i64) -> Result<&'static str> {
    Ok(match r {
        reason::UNASSIGNED => "REASON_UNASSIGNED",
        reason::ILLEGAL => "REASON_ILLEGAL",
        reason::IRREGULAR => "REASON_IRREGULAR",
        reason::RESET => "REASON_RESET",
        reason::CLOSE => "REASON_CLOSE",
        reason::CLONE => "REASON_CLONE",
        _ => {
            return Err(Error::arg_value(
                1,
                "must be a UConverter::REASON_* constant",
            ))
        }
    })
}

/// `UConverter::transcode($str, $toEncoding, $fromEncoding, $options)`;
/// `from_subst`/`to_subst` are the string-valued options.  Errors are
/// global.
pub fn transcode(
    s: &[u8],
    dest: &str,
    src: &str,
    from_subst: Option<&[u8]>,
    to_subst: Option<&[u8]>,
) -> Result<(Vec<u8>, Vec<IntlError>)> {
    let mut notes = Vec::new();
    let open = |enc: &str, notes: &mut Vec<IntlError>| -> Result<Converter> {
        let (c, st) = Converter::open(enc).map_err(|e| {
            Error::global(e, format!("Error setting encoding: {} - {}", e.0, e.name()))
        })?;
        if st == UErrorCode::AMBIGUOUS_ALIAS_WARNING {
            let actual = c.name().unwrap_or_else(|_| "(unknown)".into());
            notes.push(
                IntlError::new(st, format!("Ambiguous encoding specified, using {actual}"))
                    .with_scope(Scope::Global),
            );
        }
        Ok(c)
    };
    let mut sc = open(src, &mut notes)?;
    let mut dc = open(dest, &mut notes)?;
    let set = |c: &mut Converter, v: &[u8]| -> std::result::Result<(), UErrorCode> {
        if v.len() > 127 {
            return Err(UErrorCode::ILLEGAL_ARGUMENT_ERROR);
        }
        c.set_subst_chars(v)
    };
    let mut err = Ok(());
    if let Some(v) = from_subst {
        err = set(&mut sc, v);
    }
    if err.is_ok() {
        if let Some(v) = to_subst {
            err = set(&mut dc, v);
        }
    }
    if let Err(e) = err {
        return Err(Error::global(
            e,
            format!("returned error {}: {}", e.0, e.name()),
        ));
    }
    let r = sc.to_uchars(s).and_then(|u| dc.from_uchars(&u));
    match r {
        Ok(v) => Ok((v, notes)),
        Err(e) => Err(Error::global(
            e,
            format!("returned error {}: {}", e.0, e.name()),
        )),
    }
}

/// `UConverter::getAvailable()`.
pub fn get_available() -> Vec<String> {
    ucnv::available()
}

/// `UConverter::getAliases($name)` (errors are global).
pub fn get_aliases(name: &str) -> Result<Vec<String>> {
    ucnv::aliases(name)
        .map_err(|e| Error::global(e, format!("returned error {}: {}", e.0, e.name())))
}

/// `UConverter::getStandards()`.
pub fn get_standards() -> Result<Vec<String>> {
    ucnv::standards().map_err(|e| Error::global(e, format!("returned error {}: {}", e.0, e.name())))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo(Arc<Mutex<Vec<String>>>);

    impl PhpCallbacks for Echo {
        fn to_u_callback(
            &mut self,
            reason: i64,
            _s: &[u8],
            _u: &[u8],
            _e: &mut i64,
        ) -> CallbackValue {
            self.0
                .lock()
                .unwrap()
                .push(format!("to:{}", reason_text(reason).unwrap()));
            CallbackValue::Null
        }

        fn from_u_callback(
            &mut self,
            reason: i64,
            _s: &[i64],
            _c: i64,
            e: &mut i64,
        ) -> CallbackValue {
            self.0
                .lock()
                .unwrap()
                .push(format!("from:{}", reason_text(reason).unwrap()));
            if reason == 0 {
                *e = 0;
                return CallbackValue::Str(b"?".to_vec());
            }
            CallbackValue::Null
        }
    }

    #[test]
    fn convert_basic() {
        let mut c = UConverter::new_empty(None);
        c.construct(Some("latin1"), Some("utf-8")).unwrap();
        assert_eq!(c.convert("é".as_bytes(), false).unwrap(), b"\xe9");
        assert_eq!(c.convert(b"\xe9", true).unwrap(), "é".as_bytes());
        let (v, _) = transcode("aé".as_bytes(), "ascii", "utf-8", None, Some(b"?")).unwrap();
        assert_eq!(v, b"a?");
        assert!(!get_aliases("utf-8").unwrap().is_empty());
        assert!(get_available().len() > 100);
    }

    #[test]
    fn callbacks_fire() {
        let log = Arc::new(Mutex::new(Vec::new()));
        {
            let mut c = UConverter::new_empty(Some(Box::new(Echo(log.clone()))));
            c.construct(Some("ascii"), Some("utf-8")).unwrap();
            assert_eq!(c.convert("\u{a1}x".as_bytes(), false).unwrap(), b"?x");
        }
        let l = log.lock().unwrap().clone();
        assert!(l.contains(&"from:REASON_UNASSIGNED".to_owned()));
        assert_eq!(l.iter().filter(|s| s.ends_with("REASON_CLOSE")).count(), 4);
    }
}
