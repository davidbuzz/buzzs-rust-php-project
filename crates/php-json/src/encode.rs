//! `json_encode()`: a port of `ext/json/json_encoder.c`.

use crate::error::{EncodeError, ErrorCode, JsonError};
use crate::flags::{
    FORCE_OBJECT, HEX_AMP, HEX_APOS, HEX_QUOT, HEX_TAG, INVALID_UTF8_IGNORE,
    INVALID_UTF8_SUBSTITUTE, NUMERIC_CHECK, PARTIAL_OUTPUT_ON_ERROR, PRESERVE_ZERO_FRACTION,
    PRETTY_PRINT, UNESCAPED_LINE_TERMINATORS, UNESCAPED_SLASHES, UNESCAPED_UNICODE,
};
use crate::host::{GuardKey, JsonHost};
use php_runtime::float::zend_gcvt;
use php_runtime::numeric::{is_numeric_string, Number};
use php_runtime::{ArrayKey, Obj, RtResult, Value, ZArray};

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// The bitmap of bytes `php_json_escape_string` must look at: control
/// characters, `"`, `&`, `'`, `/`, `<`, `>`, `\` and all bytes `>= 0x80`.
const CHARMAP: [u32; 8] = [
    0xffff_ffff,
    0x5000_80c4,
    0x1000_0000,
    0x0000_0000,
    0xffff_ffff,
    0xffff_ffff,
    0xffff_ffff,
    0xffff_ffff,
];

#[inline]
fn needs_escape(b: u8) -> bool {
    CHARMAP[usize::from(b >> 5)] & (1 << (b & 31)) != 0
}

/// Port of `php_next_utf8_char` (`get_next_char_utf8` in
/// `ext/standard/html.c`): decodes one character at the start of `s`.
/// Returns the code point (or `None` for an invalid sequence) and the number
/// of bytes consumed, following UTR #36 strategy 2 for invalid sequences.
pub(crate) fn next_utf8_char(s: &[u8]) -> (Option<u32>, usize) {
    let lead = |c: u8| c < 0x80 || (0xC2..=0xF4).contains(&c);
    let trail = |c: u8| (0x80..=0xBF).contains(&c);
    let at = |i: usize| s.get(i).copied();
    let c = s[0];
    let avail = s.len();
    if c < 0x80 {
        return (Some(u32::from(c)), 1);
    }
    if c < 0xC2 {
        return (None, 1);
    }
    if c < 0xE0 {
        let Some(c1) = at(1) else { return (None, 1) };
        if !trail(c1) {
            return (None, if lead(c1) { 1 } else { 2 });
        }
        let cp = (u32::from(c & 0x1F) << 6) | u32::from(c1 & 0x3F);
        if cp < 0x80 {
            return (None, 2);
        }
        return (Some(cp), 2);
    }
    if c < 0xF0 {
        if avail < 3 || !trail(s[1]) || !trail(s[2]) {
            let n = if avail < 2 || lead(s[1]) {
                1
            } else if avail < 3 || lead(s[2]) {
                2
            } else {
                3
            };
            return (None, n);
        }
        let cp = (u32::from(c & 0x0F) << 12) | (u32::from(s[1] & 0x3F) << 6) | u32::from(s[2] & 0x3F);
        if cp < 0x800 || (0xD800..=0xDFFF).contains(&cp) {
            return (None, 3);
        }
        return (Some(cp), 3);
    }
    if c < 0xF5 {
        if avail < 4 || !trail(s[1]) || !trail(s[2]) || !trail(s[3]) {
            let n = if avail < 2 || lead(s[1]) {
                1
            } else if avail < 3 || lead(s[2]) {
                2
            } else if avail < 4 || lead(s[3]) {
                3
            } else {
                4
            };
            return (None, n);
        }
        let cp = (u32::from(c & 0x07) << 18)
            | (u32::from(s[1] & 0x3F) << 12)
            | (u32::from(s[2] & 0x3F) << 6)
            | u32::from(s[3] & 0x3F);
        if !(0x10000..=0x10_FFFF).contains(&cp) {
            return (None, 4);
        }
        return (Some(cp), 4);
    }
    (None, 1)
}

/// Appends `\uXXXX` with lowercase hex digits.
fn push_u_escape(buf: &mut Vec<u8>, us: u32) {
    buf.extend_from_slice(b"\\u");
    for shift in [12, 8, 4, 0] {
        buf.push(HEX_DIGITS[((us >> shift) & 0xF) as usize]);
    }
}

/// Formats a double like `php_json_encode_double` (`zend_gcvt` with
/// `serialize_precision`, lowercase `e`, optional `.0`).
pub(crate) fn format_double(d: f64, precision: i64, options: i64) -> String {
    let p = i32::try_from(precision).unwrap_or(if precision < 0 { -1 } else { i32::MAX });
    let mut s = zend_gcvt(d, p.min(1000), b'.', b'e');
    if options & PRESERVE_ZERO_FRACTION != 0 && !s.contains('.') {
        s.push_str(".0");
    }
    s
}

/// The encoder state (`php_json_encoder`).
pub(crate) struct Encoder<'h> {
    host: &'h mut dyn JsonHost,
    /// Current depth (`encoder->depth`), also the pretty-print indent level.
    depth: i64,
    max_depth: i64,
    /// `encoder->error_code`: the last error recorded.
    pub(crate) error: ErrorCode,
    /// Number of currently open arrays/objects (for the stack-limit check).
    nesting: usize,
    precision: i64,
}

/// `zend_result`-like outcome of the encoding functions: `Ok(true)` is
/// `SUCCESS`, `Ok(false)` is `FAILURE`; `Err` is a thrown exception.
type Step = RtResult<bool>;

impl<'h> Encoder<'h> {
    /// `php_json_encode_init` with `encoder.max_depth = max_depth`.
    pub(crate) fn new(host: &'h mut dyn JsonHost, max_depth: i64) -> Self {
        let precision = host.serialize_precision();
        Encoder {
            host,
            depth: 0,
            max_depth,
            error: ErrorCode::None,
            nesting: 0,
            precision,
        }
    }

    fn pretty_char(buf: &mut Vec<u8>, options: i64, c: u8) {
        if options & PRETTY_PRINT != 0 {
            buf.push(c);
        }
    }

    fn pretty_indent(&self, buf: &mut Vec<u8>, options: i64) {
        if options & PRETTY_PRINT != 0 {
            for _ in 0..self.depth {
                buf.extend_from_slice(b"    ");
            }
        }
    }

    /// `php_json_encode_zval`.
    pub(crate) fn encode_zval(&mut self, buf: &mut Vec<u8>, val: &Value, options: i64) -> Step {
        match val {
            Value::Null => buf.extend_from_slice(b"null"),
            Value::Bool(true) => buf.extend_from_slice(b"true"),
            Value::Bool(false) => buf.extend_from_slice(b"false"),
            Value::Int(i) => buf.extend_from_slice(i.to_string().as_bytes()),
            Value::Float(d) => {
                if d.is_finite() {
                    buf.extend_from_slice(format_double(*d, self.precision, options).as_bytes());
                } else {
                    self.error = ErrorCode::InfOrNan;
                    buf.push(b'0');
                }
            }
            Value::Str(s) => return Ok(self.escape_string(buf, s.as_bytes(), options)),
            Value::Object(o) => {
                if self.host.is_json_serializable(o) {
                    return self.encode_serializable_object(buf, o, options);
                }
                if self.host.is_enum(o) {
                    return self.encode_serializable_enum(buf, o, options);
                }
                return self.encode_array(buf, val, options);
            }
            Value::Array(_) => return self.encode_array(buf, val, options),
            Value::Reference(r) => {
                let inner = r.get();
                return self.encode_zval(buf, &inner, options);
            }
            Value::Resource(_) => {
                self.error = ErrorCode::UnsupportedType;
                if options & PARTIAL_OUTPUT_ON_ERROR != 0 {
                    buf.extend_from_slice(b"null");
                }
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// `php_json_encode_serializable_object`.
    fn encode_serializable_object(&mut self, buf: &mut Vec<u8>, obj: &Obj, options: i64) -> Step {
        let guard = GuardKey::json_serialize(obj);
        if !self.host.json_protect(guard) {
            self.error = ErrorCode::Recursion;
            if options & PARTIAL_OUTPUT_ON_ERROR != 0 {
                buf.extend_from_slice(b"null");
            }
            return Ok(false);
        }
        let retval = match self.host.json_serialize(obj) {
            Ok(v) => v,
            Err(e) => {
                self.host.json_unprotect(guard);
                return Err(e);
            }
        };
        if let Value::Object(r) = &retval {
            if Obj::ptr_eq(r, obj) {
                // `return $this;`: encode the properties directly.
                self.host.json_unprotect(guard);
                return self.encode_array(buf, &retval, options);
            }
        }
        let result = self.encode_zval(buf, &retval, options);
        self.host.json_unprotect(guard);
        result
    }

    /// `php_json_encode_serializable_enum`.
    fn encode_serializable_enum(&mut self, buf: &mut Vec<u8>, obj: &Obj, options: i64) -> Step {
        match self.host.enum_backing_value(obj) {
            None => {
                self.error = ErrorCode::NonBackedEnum;
                buf.push(b'0');
                Ok(false)
            }
            Some(v) => self.encode_zval(buf, &v, options),
        }
    }

    /// `php_json_encode_array` for arrays and (non-serializable) objects.
    #[allow(clippy::too_many_lines)]
    fn encode_array(&mut self, buf: &mut Vec<u8>, val: &Value, options: i64) -> Step {
        let partial = options & PARTIAL_OUTPUT_ON_ERROR != 0;
        if self.host.json_stack_limit_reached(self.nesting) {
            self.error = ErrorCode::Depth;
            if partial {
                buf.extend_from_slice(b"null");
            }
            return Ok(false);
        }
        let (guard, encode_as_object, is_object) = match val {
            Value::Array(a) => (
                GuardKey::array(a),
                options & FORCE_OBJECT != 0 || !a.is_list(),
                false,
            ),
            Value::Object(o) => (GuardKey::object(o), true, true),
            _ => unreachable!("encode_array on a scalar"),
        };
        if !self.host.json_protect(guard) {
            self.error = ErrorCode::Recursion;
            buf.extend_from_slice(b"null");
            return Ok(false);
        }
        let table: Option<ZArray> = match val {
            Value::Array(a) => Some(a.clone()),
            Value::Object(o) => match self.host.json_properties(o) {
                Ok(t) => t,
                Err(e) => {
                    self.host.json_unprotect(guard);
                    return Err(e);
                }
            },
            _ => None,
        };
        self.nesting += 1;
        let result = self.encode_table(buf, table.as_ref(), options, encode_as_object, is_object);
        self.nesting -= 1;
        self.host.json_unprotect(guard);
        let empty = match result {
            Ok(Some(empty)) => empty,
            Ok(None) => return Ok(false),
            Err(e) => return Err(e),
        };
        if self.depth > self.max_depth {
            self.error = ErrorCode::Depth;
            if !partial {
                return Ok(false);
            }
        }
        self.depth -= 1;
        if !empty {
            Self::pretty_char(buf, options, b'\n');
            self.pretty_indent(buf, options);
        }
        buf.push(if encode_as_object { b'}' } else { b']' });
        Ok(true)
    }

    /// The body of `php_json_encode_array` between the recursion guards:
    /// writes the opening bracket and the members. Returns `Some(empty)` on
    /// success and `None` on a failure that aborts the encoding.
    fn encode_table(
        &mut self,
        buf: &mut Vec<u8>,
        table: Option<&ZArray>,
        options: i64,
        encode_as_object: bool,
        is_object: bool,
    ) -> RtResult<Option<bool>> {
        let partial = options & PARTIAL_OUTPUT_ON_ERROR != 0;
        buf.push(if encode_as_object { b'{' } else { b'[' });
        self.depth += 1;
        let Some(table) = table.filter(|t| !t.is_empty()) else {
            return Ok(Some(true));
        };
        for (key, data) in table.iter() {
            if encode_as_object {
                match key {
                    ArrayKey::Str(k) => {
                        let k = k.as_bytes();
                        if is_object && k.first() == Some(&0) {
                            // Skip protected and private members.
                            continue;
                        }
                        Self::pretty_char(buf, options, b'\n');
                        self.pretty_indent(buf, options);
                        if !self.escape_string(buf, k, options & !NUMERIC_CHECK) {
                            if !partial {
                                return Ok(None);
                            }
                            // Replace the "null" written on error by "".
                            buf.truncate(buf.len() - 4);
                            buf.extend_from_slice(b"\"\"");
                        }
                    }
                    ArrayKey::Int(i) => {
                        Self::pretty_char(buf, options, b'\n');
                        self.pretty_indent(buf, options);
                        buf.push(b'"');
                        buf.extend_from_slice(i.to_string().as_bytes());
                        buf.push(b'"');
                    }
                }
                buf.push(b':');
                Self::pretty_char(buf, options, b' ');
            } else {
                Self::pretty_char(buf, options, b'\n');
                self.pretty_indent(buf, options);
            }
            if !self.encode_zval(buf, data, options)? && !partial {
                return Ok(None);
            }
            buf.push(b',');
        }
        let empty = buf.last() != Some(&b',');
        if !empty {
            // Drop the trailing comma.
            buf.pop();
        }
        Ok(Some(empty))
    }

    /// `php_json_escape_string`. Returns `false` on failure (invalid UTF-8),
    /// in which case nothing is appended unless `JSON_PARTIAL_OUTPUT_ON_ERROR`
    /// is set (then `null` is).
    pub(crate) fn escape_string(&mut self, buf: &mut Vec<u8>, s: &[u8], options: i64) -> bool {
        if s.is_empty() {
            buf.extend_from_slice(b"\"\"");
            return true;
        }
        if options & NUMERIC_CHECK != 0 {
            match is_numeric_string(s, false) {
                Some(Number::Int(i)) => {
                    buf.extend_from_slice(i.to_string().as_bytes());
                    return true;
                }
                Some(Number::Float(d)) if d.is_finite() => {
                    buf.extend_from_slice(format_double(d, self.precision, options).as_bytes());
                    return true;
                }
                _ => {}
            }
        }
        let checkpoint = buf.len();
        buf.reserve(s.len() + 2);
        buf.push(b'"');
        let mut pos = 0;
        while pos < s.len() {
            // Copy the run of bytes that need no escaping.
            let run = s[pos..].iter().position(|&b| needs_escape(b)).unwrap_or(s.len() - pos);
            buf.extend_from_slice(&s[pos..pos + run]);
            pos += run;
            let Some(&us) = s.get(pos) else { break };
            if us >= 0x80 {
                let (cp, n) = next_utf8_char(&s[pos..]);
                match cp {
                    None => {
                        if options & INVALID_UTF8_IGNORE != 0 {
                            // Drop the invalid sequence.
                        } else if options & INVALID_UTF8_SUBSTITUTE != 0 {
                            if options & UNESCAPED_UNICODE != 0 {
                                buf.extend_from_slice("\u{FFFD}".as_bytes());
                            } else {
                                buf.extend_from_slice(b"\\ufffd");
                            }
                        } else {
                            buf.truncate(checkpoint);
                            self.error = ErrorCode::Utf8;
                            if options & PARTIAL_OUTPUT_ON_ERROR != 0 {
                                buf.extend_from_slice(b"null");
                            }
                            return false;
                        }
                    }
                    Some(cp)
                        if options & UNESCAPED_UNICODE != 0
                            && (options & UNESCAPED_LINE_TERMINATORS != 0
                                || !(0x2028..=0x2029).contains(&cp)) =>
                    {
                        buf.extend_from_slice(&s[pos..pos + n]);
                    }
                    Some(mut cp) => {
                        if cp >= 0x10000 {
                            cp -= 0x10000;
                            push_u_escape(buf, 0xD800 | (cp >> 10));
                            cp = 0xDC00 | (cp & 0x3FF);
                        }
                        push_u_escape(buf, cp);
                    }
                }
                pos += n;
                continue;
            }
            pos += 1;
            match us {
                b'"' => {
                    if options & HEX_QUOT != 0 {
                        buf.extend_from_slice(b"\\u0022");
                    } else {
                        buf.extend_from_slice(b"\\\"");
                    }
                }
                b'\\' => buf.extend_from_slice(b"\\\\"),
                b'/' => {
                    if options & UNESCAPED_SLASHES != 0 {
                        buf.push(b'/');
                    } else {
                        buf.extend_from_slice(b"\\/");
                    }
                }
                0x08 => buf.extend_from_slice(b"\\b"),
                0x0C => buf.extend_from_slice(b"\\f"),
                b'\n' => buf.extend_from_slice(b"\\n"),
                b'\r' => buf.extend_from_slice(b"\\r"),
                b'\t' => buf.extend_from_slice(b"\\t"),
                b'<' if options & HEX_TAG != 0 => buf.extend_from_slice(b"\\u003C"),
                b'>' if options & HEX_TAG != 0 => buf.extend_from_slice(b"\\u003E"),
                b'&' if options & HEX_AMP != 0 => buf.extend_from_slice(b"\\u0026"),
                b'\'' if options & HEX_APOS != 0 => buf.extend_from_slice(b"\\u0027"),
                b'<' | b'>' | b'&' | b'\'' => buf.push(us),
                _ => push_u_escape(buf, u32::from(us)),
            }
        }
        buf.push(b'"');
        true
    }
}

/// Clamps a `zend_long` depth to the C `int` stored in the encoder.
fn int_depth(depth: i64) -> i64 {
    depth.clamp(i64::from(i32::MIN), i64::from(i32::MAX))
}

/// Encodes a value as JSON (`php_json_encode_zval` behind `json_encode()`).
///
/// * `flags`: any combination of the encode `JSON_*` flags
///   (`JSON_THROW_ON_ERROR` is handled by the caller, see
///   [`crate::functions::json_encode`]).
/// * `depth`: the maximum nesting depth; a value nested deeper fails with
///   `JSON_ERROR_DEPTH` (`json_encode()` rejects values outside the C `int`
///   range with a `ValueError` first).
///
/// On a JSON error, [`EncodeError::Json`] carries the last error code and,
/// with `JSON_PARTIAL_OUTPUT_ON_ERROR`, the partial output (with `null` or
/// `0` in place of the failing values) that `json_encode()` returns.
/// Exceptions thrown by `jsonSerialize()` or the host abort the encoding with
/// [`EncodeError::Exception`].
pub fn encode(
    value: &Value,
    flags: i64,
    depth: i64,
    host: &mut dyn JsonHost,
) -> Result<Vec<u8>, EncodeError> {
    let mut buf = Vec::new();
    let mut encoder = Encoder::new(host, int_depth(depth));
    if let Err(e) = encoder.encode_zval(&mut buf, value, flags) {
        return Err(EncodeError::Exception(e));
    }
    if encoder.error == ErrorCode::None {
        Ok(buf)
    } else {
        Err(EncodeError::Json {
            error: JsonError::new(encoder.error),
            partial_output: (flags & PARTIAL_OUTPUT_ON_ERROR != 0).then_some(buf),
        })
    }
}

/// `php_json_encode_string`: encodes one string with the given options, or
/// `None` if it is not valid UTF-8 (and no `INVALID_UTF8_*` flag is set).
/// Uses the default `serialize_precision` (-1) for `JSON_NUMERIC_CHECK`.
pub fn encode_string(s: &[u8], flags: i64) -> Option<Vec<u8>> {
    let mut host = php_runtime::CollectingContext::new();
    let mut buf = Vec::new();
    let mut encoder = Encoder::new(&mut host, crate::flags::DEFAULT_DEPTH);
    encoder
        .escape_string(&mut buf, s, flags & !PARTIAL_OUTPUT_ON_ERROR)
        .then_some(buf)
}
