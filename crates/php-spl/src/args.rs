//! Argument parsing for SPL methods and functions (`zend_parse_parameters`
//! in weak typing mode), producing PHP's exact `ArgumentCountError`,
//! `TypeError` and deprecation texts.

use crate::host::SplHost;
use php_runtime::convert::{double_fits_long, is_long_compatible};
use php_runtime::float::format_double_h;
use php_runtime::numeric::{is_numeric_string_ex, Number};
use php_runtime::{CastTarget, ErrorLevel, Obj, RtResult, RuntimeError, Value, ZArray, ZStr};

/// The arguments of one internal call together with the function name
/// (`Class::method` or `function`) and the parameter names used in error
/// messages.
#[derive(Debug, Clone, Copy)]
pub struct Args<'a> {
    func: &'a str,
    args: &'a [Value],
    names: &'a [&'a str],
}

/// `zend_wrong_parameters_count_error`.
#[must_use]
pub fn count_error(func: &str, given: usize, min: usize, max: Option<usize>) -> RuntimeError {
    let (word, n) = match max {
        Some(max) if min == max => ("exactly", min),
        _ if given < min => ("at least", min),
        Some(max) => ("at most", max),
        None => ("at least", min),
    };
    RuntimeError::ArgumentCountError(format!(
        "{func}() expects {word} {n} argument{}, {given} given",
        if n == 1 { "" } else { "s" }
    ))
}

/// The value name used in type errors (`zend_zval_value_name`).
#[must_use]
pub fn value_name(v: &Value) -> String {
    match v {
        Value::Reference(r) => value_name(&r.get()),
        other => other.value_name(),
    }
}

impl<'a> Args<'a> {
    /// Checks the argument count (`min..=max`, `max == None` for variadics)
    /// and returns the parser.
    pub fn new(
        func: &'a str,
        args: &'a [Value],
        names: &'a [&'a str],
        min: usize,
        max: Option<usize>,
    ) -> RtResult<Self> {
        if args.len() < min || max.is_some_and(|m| args.len() > m) {
            return Err(count_error(func, args.len(), min, max));
        }
        Ok(Args { func, args, names })
    }

    /// `ZEND_PARSE_PARAMETERS_NONE()`.
    pub fn none(func: &str, args: &[Value]) -> RtResult<()> {
        if args.is_empty() {
            Ok(())
        } else {
            Err(count_error(func, args.len(), 0, Some(0)))
        }
    }

    /// The function name.
    #[must_use]
    pub fn func(&self) -> &'a str {
        self.func
    }

    /// Number of passed arguments.
    #[must_use]
    pub fn len(&self) -> usize {
        self.args.len()
    }

    /// Whether no arguments were passed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.args.is_empty()
    }

    /// Whether argument `i` (0-based) was passed.
    #[must_use]
    pub fn has(&self, i: usize) -> bool {
        i < self.args.len()
    }

    /// The raw argument (dereferenced), `null` when absent.
    #[must_use]
    pub fn raw(&self, i: usize) -> Value {
        self.args.get(i).map(Value::deref).unwrap_or_default()
    }

    /// The raw argument without dereferencing (for by-reference parameters).
    #[must_use]
    pub fn raw_ref(&self, i: usize) -> Option<&'a Value> {
        self.args.get(i)
    }

    fn name(&self, i: usize) -> &str {
        self.names.get(i).copied().unwrap_or("")
    }

    /// `"Func(): Argument #n ($name) "` prefix.
    #[must_use]
    pub fn arg_prefix(&self, i: usize) -> String {
        let name = self.name(i);
        if name.is_empty() {
            format!("{}(): Argument #{}", self.func, i + 1)
        } else {
            format!("{}(): Argument #{} (${name})", self.func, i + 1)
        }
    }

    /// A `TypeError` "must be of type `ty`, X given".
    #[must_use]
    pub fn type_error(&self, i: usize, ty: &str) -> RuntimeError {
        RuntimeError::TypeError(format!(
            "{} must be of type {ty}, {} given",
            self.arg_prefix(i),
            value_name(&self.raw(i))
        ))
    }

    /// A `ValueError` for argument `i`: "Func(): Argument #n ($x) `msg`".
    #[must_use]
    pub fn value_error(&self, i: usize, msg: &str) -> RuntimeError {
        RuntimeError::ValueError(format!("{} {msg}", self.arg_prefix(i)))
    }

    /// An error of an arbitrary exception class for argument `i`
    /// (`zend_argument_error(ce, ...)`).
    #[must_use]
    pub fn arg_error(&self, class: &str, i: usize, msg: &str) -> RuntimeError {
        crate::exceptions::throw(class, format!("{} {msg}", self.arg_prefix(i)))
    }

    fn null_deprecated(&self, host: &mut dyn SplHost, i: usize, ty: &str) -> RtResult<()> {
        let name = self.name(i);
        let label = if name.is_empty() {
            String::new()
        } else {
            format!(" (${name})")
        };
        host.diagnostic(
            ErrorLevel::Deprecated,
            &format!(
                "{}(): Passing null to parameter #{}{label} of type {ty} is deprecated",
                self.func,
                i + 1
            ),
        )
    }

    /// `l`: an integer argument.
    pub fn long(&self, host: &mut dyn SplHost, i: usize) -> RtResult<i64> {
        let v = self.raw(i);
        match parse_long_weak(host, &v)? {
            LongParse::Ok(n) => Ok(n),
            LongParse::Null => {
                self.null_deprecated(host, i, "int")?;
                Ok(0)
            }
            LongParse::Fail => Err(self.type_error(i, "int")),
        }
    }

    /// Optional `l` with a default.
    pub fn opt_long(&self, host: &mut dyn SplHost, i: usize, default: i64) -> RtResult<i64> {
        if self.has(i) {
            self.long(host, i)
        } else {
            Ok(default)
        }
    }

    /// `l!`: an integer or `null`.
    pub fn long_or_null(&self, host: &mut dyn SplHost, i: usize) -> RtResult<Option<i64>> {
        let v = self.raw(i);
        if !self.has(i) || v.is_null() {
            return Ok(None);
        }
        match parse_long_weak(host, &v)? {
            LongParse::Ok(n) => Ok(Some(n)),
            _ => Err(self.type_error(i, "?int")),
        }
    }

    /// `b`: a boolean argument.
    pub fn bool(&self, host: &mut dyn SplHost, i: usize) -> RtResult<bool> {
        let v = self.raw(i);
        match v {
            Value::Bool(b) => Ok(b),
            Value::Null => {
                self.null_deprecated(host, i, "bool")?;
                Ok(false)
            }
            Value::Int(n) => Ok(n != 0),
            Value::Float(f) => v.to_bool(host).map(|_| f != 0.0),
            Value::Str(s) => Ok(!(s.is_empty() || s.as_bytes() == b"0")),
            _ => Err(self.type_error(i, "bool")),
        }
    }

    /// Optional `b`.
    pub fn opt_bool(&self, host: &mut dyn SplHost, i: usize, default: bool) -> RtResult<bool> {
        if self.has(i) {
            self.bool(host, i)
        } else {
            Ok(default)
        }
    }

    /// `S`/`s`: a string argument.
    pub fn str(&self, host: &mut dyn SplHost, i: usize) -> RtResult<ZStr> {
        let v = self.raw(i);
        match self.str_weak(host, &v)? {
            Some(s) => Ok(s),
            None if v.is_null() => {
                self.null_deprecated(host, i, "string")?;
                Ok(ZStr::empty())
            }
            None => Err(self.type_error(i, "string")),
        }
    }

    /// Optional string with a default.
    pub fn opt_str(&self, host: &mut dyn SplHost, i: usize, default: &str) -> RtResult<ZStr> {
        if self.has(i) {
            self.str(host, i)
        } else {
            Ok(ZStr::from(default))
        }
    }

    /// `S!`: a string or `null`.
    pub fn str_or_null(&self, host: &mut dyn SplHost, i: usize) -> RtResult<Option<ZStr>> {
        let v = self.raw(i);
        if !self.has(i) || v.is_null() {
            return Ok(None);
        }
        match self.str_weak(host, &v)? {
            Some(s) => Ok(Some(s)),
            None => Err(self.type_error(i, "?string")),
        }
    }

    /// `P`/`p`: a path (string without NUL bytes).
    pub fn path(&self, host: &mut dyn SplHost, i: usize) -> RtResult<ZStr> {
        let s = self.str(host, i)?;
        if s.as_bytes().contains(&0) {
            return Err(self.value_error(i, "must not contain any null bytes"));
        }
        Ok(s)
    }

    fn str_weak(&self, host: &mut dyn SplHost, v: &Value) -> RtResult<Option<ZStr>> {
        Ok(match v {
            Value::Str(s) => Some(s.clone()),
            Value::Int(_) | Value::Float(_) | Value::Bool(_) => Some(v.to_zstr(host)?),
            Value::Object(o) => match host.cast_object(o, CastTarget::String)? {
                Some(Value::Str(s)) => Some(s),
                _ => None,
            },
            _ => None,
        })
    }

    /// `a`/`h`: an array argument.
    pub fn array(&self, i: usize) -> RtResult<ZArray> {
        match self.raw(i) {
            Value::Array(a) => Ok(a),
            _ => Err(self.type_error(i, "array")),
        }
    }

    /// `o`: an object argument.
    pub fn object(&self, i: usize) -> RtResult<Obj> {
        match self.raw(i) {
            Value::Object(o) => Ok(o),
            _ => Err(self.type_error(i, "object")),
        }
    }

    /// `O`: an object of the given class or interface.
    pub fn object_of(&self, host: &mut dyn SplHost, i: usize, class: &str) -> RtResult<Obj> {
        match self.raw(i) {
            Value::Object(o) if host.instance_of(&o, class) => Ok(o),
            _ => Err(self.type_error(i, class)),
        }
    }

    /// `f`: a callable argument (checked with the host).
    pub fn callable(&self, host: &mut dyn SplHost, i: usize) -> RtResult<Value> {
        let v = self.raw(i);
        match host.check_callable(&v) {
            Ok(()) => Ok(v),
            Err(reason) => Err(RuntimeError::TypeError(format!(
                "{} must be a valid callback, {reason}",
                self.arg_prefix(i)
            ))),
        }
    }
}

/// Result of [`parse_long_weak`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongParse {
    /// Accepted.
    Ok(i64),
    /// The argument is `null` (deprecated, coerces to 0).
    Null,
    /// Rejected (`TypeError`).
    Fail,
}

/// `zend_parse_arg_long_weak`: coerces a value to `int` the way internal
/// functions do in weak mode. Emits the "Implicit conversion" deprecations.
pub fn parse_long_weak(host: &mut dyn SplHost, v: &Value) -> RtResult<LongParse> {
    Ok(match v {
        Value::Int(n) => LongParse::Ok(*n),
        Value::Float(d) => match float_to_long_arg(host, *d, None)? {
            Some(n) => LongParse::Ok(n),
            None => LongParse::Fail,
        },
        Value::Str(s) => match is_numeric_string_ex(s.as_bytes(), false) {
            Some(n) => match n.value {
                Number::Int(i) => LongParse::Ok(i),
                Number::Float(d) => match float_to_long_arg(host, d, Some(s))? {
                    Some(n) => LongParse::Ok(n),
                    None => LongParse::Fail,
                },
            },
            None => LongParse::Fail,
        },
        Value::Null => LongParse::Null,
        Value::Bool(b) => LongParse::Ok(i64::from(*b)),
        Value::Reference(r) => return parse_long_weak(host, &r.get()),
        _ => LongParse::Fail,
    })
}

#[allow(clippy::cast_possible_truncation)]
fn float_to_long_arg(host: &mut dyn SplHost, d: f64, from: Option<&ZStr>) -> RtResult<Option<i64>> {
    if d.is_nan() || !double_fits_long(d) {
        return Ok(None);
    }
    let l = d as i64;
    if !is_long_compatible(d, l) {
        let msg = match from {
            Some(s) => {
                format!("Implicit conversion from float-string \"{s}\" to int loses precision")
            }
            None => format!(
                "Implicit conversion from float {} to int loses precision",
                format_double_h(d, -1)
            ),
        };
        host.diagnostic(ErrorLevel::Deprecated, &msg)?;
    }
    Ok(Some(l))
}
