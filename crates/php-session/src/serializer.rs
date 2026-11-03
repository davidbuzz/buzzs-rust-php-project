//! The three built-in session serializers (`php`, `php_binary`,
//! `php_serialize`) from `ext/session/session.c`.
//!
//! PHP serializes every `$_SESSION` entry with one shared `var_hash`, so
//! back-references (`R:`/`r:`) may point into *other* entries. php-runtime's
//! [`serialize`]/[`unserialize`] work on one value at a time, so the entries
//! are combined into a single array and the reference numbers are shifted by
//! one (the enclosing array occupies slot 1): encoding serializes a filtered
//! copy of `$_SESSION` and splits the result, decoding assembles
//! `a:N:{i:0;<v0>i:1;<v1>…}` from the individual payloads. Splitting and
//! assembling use a small scanner over the serialization grammar of
//! `ext/standard/var_unserializer.re`.

use php_runtime::serialize::{serialize, unserialize, SerializedObject, UnserializeOptions};
use php_runtime::{
    ArrayKey, BinaryOp, CastTarget, ErrorLevel, Obj, ObjectDump, PropertyPurpose, RtResult,
    RuntimeContext, RuntimeError, Value, ZArray, ZStr,
};

/// `PS_DELIMITER` of the `php` serializer.
pub const PS_DELIMITER: u8 = b'|';
/// `PS_BIN_MAX`: longest key the `php_binary` serializer writes.
pub const PS_BIN_MAX: usize = 127;
/// `PS_BIN_UNDEF`: the high bit of a `php_binary` length byte.
const PS_BIN_UNDEF: u8 = 0x80;

/// A session serializer (`session.serialize_handler`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Serializer {
    /// `php_serialize`: `serialize($_SESSION)`.
    PhpSerialize,
    /// `php` (default): `key|value` pairs.
    Php,
    /// `php_binary`: length byte, key, value.
    PhpBinary,
}

impl Serializer {
    /// All serializers in registration order (`ps_serializers`).
    pub const ALL: [Serializer; 3] = [
        Serializer::PhpSerialize,
        Serializer::Php,
        Serializer::PhpBinary,
    ];

    /// `_php_find_ps_serializer()`: case-insensitive lookup by name.
    #[must_use]
    pub fn find(name: &[u8]) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|s| s.name().as_bytes().eq_ignore_ascii_case(name))
    }

    /// The registered name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Serializer::PhpSerialize => "php_serialize",
            Serializer::Php => "php",
            Serializer::PhpBinary => "php_binary",
        }
    }
}

/// Result of [`encode`].
#[derive(Debug, Clone, Default)]
pub struct Encoded {
    /// The encoded data, `None` when encoding failed (a `php` key containing
    /// `|`, or an exception thrown while serializing).
    pub data: Option<Vec<u8>>,
    /// Docref warnings to emit, in order (`Skipping numeric key N`,
    /// `Failed to write session data. Data contains invalid key "…"`).
    pub warnings: Vec<String>,
    /// An exception thrown by an object's serialization hook (e.g.
    /// "Serialization of 'Closure' is not allowed"); it becomes pending
    /// after the warnings are reported.
    pub exception: Option<RuntimeError>,
}

/// Result of [`decode`].
#[derive(Debug, Clone)]
pub enum Decoded {
    /// `php`/`php_binary`: entries to store into `$_SESSION` with verbatim
    /// string keys (`php_set_session_var`), and whether decoding succeeded.
    Merge {
        /// Decoded `(name, value)` pairs in order.
        entries: Vec<(ZStr, Value)>,
        /// `false` when the data was malformed.
        ok: bool,
    },
    /// `php_serialize`: the new value of `$_SESSION` (a fresh reference is
    /// bound), and whether decoding succeeded.
    Replace {
        /// The new `$_SESSION` value (an empty array on failure).
        value: Value,
        /// `false` when the data was malformed and non-empty.
        ok: bool,
    },
}

/// Encodes `vars` (the dereferenced `$_SESSION` array).
pub fn encode(
    ser: Serializer,
    vars: &ZArray,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Encoded> {
    if ser == Serializer::PhpSerialize {
        return Ok(match serialize(&Value::Array(vars.clone()), ctx) {
            Ok(data) => Encoded {
                data: Some(data),
                ..Encoded::default()
            },
            Err(e) => Encoded {
                exception: Some(e),
                ..Encoded::default()
            },
        });
    }
    // PS_ENCODE_LOOP: numeric keys are skipped with a warning.
    let mut warnings = Vec::new();
    let mut selected: Vec<(ZStr, Value)> = Vec::new();
    let mut bad_key = None;
    for (k, v) in vars.iter() {
        match k {
            ArrayKey::Int(n) => warnings.push(format!("Skipping numeric key {n}")),
            ArrayKey::Str(s) => {
                if ser == Serializer::PhpBinary && s.len() > PS_BIN_MAX {
                    continue;
                }
                if ser == Serializer::Php && s.as_bytes().contains(&PS_DELIMITER) {
                    bad_key = Some(s.clone());
                    break;
                }
                selected.push((s.clone(), v.clone()));
            }
        }
    }
    let (values, exception) = match serialize_entries(&selected, ctx) {
        Ok(v) => (v, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    if let Some(bad) = bad_key {
        warnings.push(format!(
            "Failed to write session data. Data contains invalid key \"{}\"",
            bad.to_string_lossy()
        ));
        return Ok(Encoded {
            data: None,
            warnings,
            exception,
        });
    }
    if exception.is_some() {
        return Ok(Encoded {
            data: None,
            warnings,
            exception,
        });
    }
    let mut out = Vec::new();
    for ((key, _), payload) in selected.iter().zip(values) {
        match ser {
            Serializer::Php => {
                out.extend_from_slice(key.as_bytes());
                out.push(PS_DELIMITER);
            }
            Serializer::PhpBinary => {
                // Length is at most PS_BIN_MAX (checked above).
                out.push(u8::try_from(key.len()).unwrap_or(0));
                out.extend_from_slice(key.as_bytes());
            }
            Serializer::PhpSerialize => unreachable!("handled above"),
        }
        out.extend_from_slice(&payload);
    }
    Ok(Encoded {
        data: Some(out),
        warnings,
        exception: None,
    })
}

/// Serializes the values with one shared reference table and returns each
/// payload with references renumbered as if serialized on their own.
fn serialize_entries(
    entries: &[(ZStr, Value)],
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Vec<Vec<u8>>> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let combined: ZArray = ZArray::from_values(entries.iter().map(|(_, v)| v.clone()));
    let bytes = serialize(&Value::Array(combined), ctx)?;
    let mut sc = Scanner::new(&bytes, -1);
    let n = sc.array_header().unwrap_or(0);
    let mut payloads = Vec::with_capacity(n);
    for _ in 0..n {
        let mut discard = Vec::new();
        let mut payload = Vec::new();
        if sc.value(&mut discard, 0).is_none() || sc.value(&mut payload, 0).is_none() {
            break;
        }
        payloads.push(payload);
    }
    Ok(payloads)
}

/// Decodes session data.
pub fn decode(
    ser: Serializer,
    data: &[u8],
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Decoded> {
    let mut quiet = Quiet(ctx);
    if ser == Serializer::PhpSerialize {
        let result = unserialize(data, &UnserializeOptions::default(), &mut quiet)?;
        return Ok(match result {
            Some(v) => Decoded::Replace { value: v, ok: true },
            None => Decoded::Replace {
                value: Value::empty_array(),
                ok: data.is_empty(),
            },
        });
    }
    let mut names: Vec<ZStr> = Vec::new();
    let mut payloads: Vec<Vec<u8>> = Vec::new();
    let mut ok = true;
    let mut p = 0;
    while p < data.len() {
        let name_start;
        let value_start;
        if ser == Serializer::Php {
            let Some(off) = data[p..].iter().position(|&c| c == PS_DELIMITER) else {
                ok = false;
                break;
            };
            name_start = p;
            value_start = p + off + 1;
            names.push(ZStr::from(&data[name_start..p + off]));
        } else {
            let namelen = usize::from(data[p] & !PS_BIN_UNDEF);
            if p + namelen >= data.len() {
                ok = false;
                break;
            }
            name_start = p + 1;
            value_start = name_start + namelen;
            names.push(ZStr::from(&data[name_start..value_start]));
        }
        let mut sc = Scanner::new(data, 1);
        sc.pos = value_start;
        let mut payload = Vec::new();
        if sc.value(&mut payload, 0).is_none() {
            names.pop();
            ok = false;
            break;
        }
        payloads.push(payload);
        p = sc.pos;
    }
    let mut entries = Vec::with_capacity(names.len());
    if !payloads.is_empty() {
        let mut combined = format!("a:{}:{{", payloads.len()).into_bytes();
        for (i, payload) in payloads.iter().enumerate() {
            combined.extend_from_slice(format!("i:{i};").as_bytes());
            combined.extend_from_slice(payload);
        }
        combined.push(b'}');
        match unserialize(&combined, &UnserializeOptions::default(), &mut quiet)? {
            Some(Value::Array(arr)) => {
                for (name, (_, v)) in names.into_iter().zip(arr.iter()) {
                    entries.push((name, v.clone()));
                }
            }
            _ => ok = false,
        }
    }
    Ok(Decoded::Merge { entries, ok })
}

/// A context wrapper that hides the `unserialize()` "Error at offset" /
/// "Extra data" warnings: `php_var_unserialize()` itself is silent.
struct Quiet<'a, C: RuntimeContext + ?Sized>(&'a mut C);

impl<C: RuntimeContext + ?Sized> RuntimeContext for Quiet<'_, C> {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        if message.starts_with("unserialize(): Error at offset")
            || message.starts_with("unserialize(): Extra data starting at offset")
        {
            return Ok(());
        }
        self.0.diagnostic(level, message)
    }
    fn precision(&self) -> i64 {
        self.0.precision()
    }
    fn serialize_precision(&self) -> i64 {
        self.0.serialize_precision()
    }
    fn cast_object(&mut self, obj: &Obj, target: CastTarget) -> RtResult<Option<Value>> {
        self.0.cast_object(obj, target)
    }
    fn do_operation(&mut self, op: BinaryOp, a: &Value, b: &Value) -> RtResult<Option<Value>> {
        self.0.do_operation(op, a, b)
    }
    fn compare_objects(&mut self, a: &Value, b: &Value) -> RtResult<i32> {
        self.0.compare_objects(a, b)
    }
    fn object_properties(
        &mut self,
        obj: &Obj,
        purpose: PropertyPurpose,
    ) -> RtResult<Option<ZArray>> {
        self.0.object_properties(obj, purpose)
    }
    fn object_dump(&mut self, obj: &Obj, purpose: PropertyPurpose) -> RtResult<ObjectDump> {
        self.0.object_dump(obj, purpose)
    }
    fn serialize_object(&mut self, obj: &Obj) -> RtResult<SerializedObject> {
        self.0.serialize_object(obj)
    }
    fn unserialize_object_create(&mut self, class: &ZStr) -> RtResult<Value> {
        self.0.unserialize_object_create(class)
    }
    fn unserialize_object_finish(&mut self, obj: &Value, props: ZArray) -> RtResult<()> {
        self.0.unserialize_object_finish(obj, props)
    }
    fn unserialize_enum(&mut self, class: &ZStr, case: &ZStr) -> RtResult<Value> {
        self.0.unserialize_enum(class, case)
    }
    fn unserialize_custom(&mut self, class: &ZStr, data: &[u8]) -> RtResult<Value> {
        self.0.unserialize_custom(class, data)
    }
    fn is_enum(&mut self, obj: &Obj) -> bool {
        self.0.is_enum(obj)
    }
    fn count_object(&mut self, obj: &Obj) -> RtResult<Option<i64>> {
        self.0.count_object(obj)
    }
    fn property_read(&mut self, obj: &Obj, name: &ZStr) -> RtResult<Option<Value>> {
        self.0.property_read(obj, name)
    }
    fn property_write(&mut self, obj: &Obj, name: &ZStr, value: Value) -> RtResult<()> {
        self.0.property_write(obj, name, value)
    }
    fn create_std_object(&mut self, props: ZArray) -> RtResult<Value> {
        self.0.create_std_object(props)
    }
}

/// Maximum nesting the scanner follows (deeper data is rejected; PHP's
/// own default `unserialize_max_depth` is 4096).
const MAX_SCAN_DEPTH: usize = 4096;

/// A scanner over serialized data that copies one value while shifting
/// `R:`/`r:` back-reference numbers by `delta`.
struct Scanner<'a> {
    data: &'a [u8],
    pos: usize,
    delta: i64,
}

impl<'a> Scanner<'a> {
    fn new(data: &'a [u8], delta: i64) -> Self {
        Self {
            data,
            pos: 0,
            delta,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn expect(&mut self, c: u8, out: &mut Vec<u8>) -> Option<()> {
        if self.peek()? == c {
            out.push(c);
            self.pos += 1;
            Some(())
        } else {
            None
        }
    }

    /// Reads an unsigned decimal number (`uiv`).
    fn uint(&mut self, out: &mut Vec<u8>) -> Option<usize> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        let digits = &self.data[start..self.pos];
        out.extend_from_slice(digits);
        std::str::from_utf8(digits).ok()?.parse().ok()
    }

    /// Copies everything up to and including the next `;`.
    fn until_semicolon(&mut self, out: &mut Vec<u8>) -> Option<()> {
        let off = self.data[self.pos..].iter().position(|&c| c == b';')?;
        out.extend_from_slice(&self.data[self.pos..=self.pos + off]);
        self.pos += off + 1;
        Some(())
    }

    /// Copies `n` raw bytes.
    fn raw(&mut self, n: usize, out: &mut Vec<u8>) -> Option<()> {
        let end = self.pos.checked_add(n)?;
        out.extend_from_slice(self.data.get(self.pos..end)?);
        self.pos = end;
        Some(())
    }

    /// Copies a quoted string of `n` bytes: `"…"`.
    fn quoted(&mut self, n: usize, out: &mut Vec<u8>) -> Option<()> {
        self.expect(b'"', out)?;
        self.raw(n, out)?;
        self.expect(b'"', out)
    }

    /// Parses `a:N:{` (encode side) and returns `N`.
    fn array_header(&mut self) -> Option<usize> {
        let mut sink = Vec::new();
        self.expect(b'a', &mut sink)?;
        self.expect(b':', &mut sink)?;
        let n = self.uint(&mut sink)?;
        self.expect(b':', &mut sink)?;
        self.expect(b'{', &mut sink)?;
        Some(n)
    }

    /// Copies `n` key/value pairs and the closing `}`.
    fn elements(&mut self, n: usize, out: &mut Vec<u8>, depth: usize) -> Option<()> {
        for _ in 0..n {
            self.value(out, depth + 1)?;
            self.value(out, depth + 1)?;
        }
        self.expect(b'}', out)
    }

    /// Copies one complete value.
    fn value(&mut self, out: &mut Vec<u8>, depth: usize) -> Option<()> {
        if depth > MAX_SCAN_DEPTH {
            return None;
        }
        let tag = self.peek()?;
        match tag {
            b'N' => {
                self.raw(1, out)?;
                self.expect(b';', out)
            }
            b'b' | b'i' | b'd' => {
                self.raw(1, out)?;
                self.expect(b':', out)?;
                self.until_semicolon(out)
            }
            b'R' | b'r' => {
                self.raw(1, out)?;
                self.expect(b':', out)?;
                let mut digits = Vec::new();
                let n = i64::try_from(self.uint(&mut digits)?).ok()?;
                let shifted = n.checked_add(self.delta)?;
                if n < 1 || shifted < 1 {
                    return None;
                }
                out.extend_from_slice(shifted.to_string().as_bytes());
                self.expect(b';', out)
            }
            b's' | b'E' => {
                self.raw(1, out)?;
                self.expect(b':', out)?;
                let n = self.uint(out)?;
                self.expect(b':', out)?;
                self.quoted(n, out)?;
                self.expect(b';', out)
            }
            b'S' => {
                self.raw(1, out)?;
                self.expect(b':', out)?;
                let n = self.uint(out)?;
                self.expect(b':', out)?;
                self.expect(b'"', out)?;
                for _ in 0..n {
                    if self.peek()? == b'\\' {
                        self.raw(3, out)?;
                    } else {
                        self.raw(1, out)?;
                    }
                }
                self.expect(b'"', out)?;
                self.expect(b';', out)
            }
            b'a' => {
                self.raw(1, out)?;
                self.expect(b':', out)?;
                let n = self.uint(out)?;
                self.expect(b':', out)?;
                self.expect(b'{', out)?;
                self.elements(n, out, depth)
            }
            b'O' | b'C' => {
                self.raw(1, out)?;
                self.expect(b':', out)?;
                let n = self.uint(out)?;
                self.expect(b':', out)?;
                self.quoted(n, out)?;
                self.expect(b':', out)?;
                let m = self.uint(out)?;
                self.expect(b':', out)?;
                self.expect(b'{', out)?;
                if tag == b'C' {
                    self.raw(m, out)?;
                    self.expect(b'}', out)
                } else {
                    self.elements(m, out, depth)
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use php_runtime::{CollectingContext, PhpRef};

    fn arr(pairs: Vec<(&str, Value)>) -> ZArray {
        let mut t = ZArray::new();
        for (k, v) in pairs {
            t.make_mut().insert_str_raw(ZStr::from(k), v);
        }
        t
    }

    #[test]
    fn find_is_case_insensitive() {
        assert_eq!(Serializer::find(b"PHP"), Some(Serializer::Php));
        assert_eq!(Serializer::find(b"php_binary"), Some(Serializer::PhpBinary));
        assert_eq!(
            Serializer::find(b"Php_Serialize"),
            Some(Serializer::PhpSerialize)
        );
        assert_eq!(Serializer::find(b"blah"), None);
    }

    #[test]
    fn php_encode_with_references() {
        let mut ctx = CollectingContext::new();
        let r = PhpRef::new(Value::Int(2));
        let vars = arr(vec![
            ("123", Value::Int(1)),
            ("a", Value::Reference(r.clone())),
            ("b", Value::Reference(r.clone())),
            ("c", Value::Reference(r)),
        ]);
        let e = encode(Serializer::Php, &vars, &mut ctx).unwrap();
        assert_eq!(e.data.unwrap(), b"123|i:1;a|i:2;b|R:2;c|R:2;");
        let e = encode(Serializer::PhpBinary, &vars, &mut ctx).unwrap();
        assert_eq!(e.data.unwrap(), b"\x03123i:1;\x01ai:2;\x01bR:2;\x01cR:2;");
    }

    #[test]
    fn numeric_keys_are_skipped() {
        let mut ctx = CollectingContext::new();
        let mut vars = ZArray::new();
        vars.make_mut().insert(5, Value::Int(1));
        vars.make_mut().insert("x", Value::Int(2));
        let e = encode(Serializer::Php, &vars, &mut ctx).unwrap();
        assert_eq!(e.data.unwrap(), b"x|i:2;");
        assert_eq!(e.warnings, vec!["Skipping numeric key 5".to_string()]);
    }

    #[test]
    fn pipe_in_key_fails() {
        let mut ctx = CollectingContext::new();
        let vars = arr(vec![("ok", Value::Int(1)), ("bad|key", Value::Int(2))]);
        let e = encode(Serializer::Php, &vars, &mut ctx).unwrap();
        assert!(e.data.is_none());
        assert_eq!(
            e.warnings,
            vec!["Failed to write session data. Data contains invalid key \"bad|key\"".to_string()]
        );
        // php_binary has no such restriction.
        let e = encode(Serializer::PhpBinary, &vars, &mut ctx).unwrap();
        assert!(e.data.is_some());
    }

    #[test]
    fn php_decode_keeps_string_keys_and_references() {
        let mut ctx = CollectingContext::new();
        let Decoded::Merge { entries, ok } =
            decode(Serializer::Php, b"123|i:1;a|i:2;b|R:2;", &mut ctx).unwrap()
        else {
            panic!()
        };
        assert!(ok);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].0.as_bytes(), b"123");
        assert!(matches!(entries[0].1, Value::Int(1)));
        match (&entries[1].1, &entries[2].1) {
            (Value::Reference(a), Value::Reference(b)) => assert!(PhpRef::ptr_eq(a, b)),
            other => panic!("{other:?}"),
        }
        assert!(ctx.diagnostics.is_empty());
    }

    #[test]
    fn php_decode_failures() {
        let mut ctx = CollectingContext::new();
        for bad in [
            &b"foo"[..],
            b"a|i:1",
            b"a|R:0;",
            b"a|x:1;",
            b"a|s:5:\"abc\";",
        ] {
            let Decoded::Merge { ok, .. } = decode(Serializer::Php, bad, &mut ctx).unwrap() else {
                panic!()
            };
            assert!(!ok, "{bad:?}");
        }
        assert!(ctx.diagnostics.is_empty(), "{:?}", ctx.diagnostics);
    }

    #[test]
    fn php_serialize_round_trip() {
        let mut ctx = CollectingContext::new();
        let vars = arr(vec![("a", Value::Int(1)), ("b", Value::string("x"))]);
        let e = encode(Serializer::PhpSerialize, &vars, &mut ctx).unwrap();
        assert_eq!(
            e.data.as_deref().unwrap(),
            b"a:2:{s:1:\"a\";i:1;s:1:\"b\";s:1:\"x\";}"
        );
        let Decoded::Replace { value, ok } =
            decode(Serializer::PhpSerialize, &e.data.unwrap(), &mut ctx).unwrap()
        else {
            panic!()
        };
        assert!(ok);
        assert_eq!(value.as_array().unwrap().len(), 2);
        let Decoded::Replace { ok, .. } =
            decode(Serializer::PhpSerialize, b"garbage", &mut ctx).unwrap()
        else {
            panic!()
        };
        assert!(!ok);
        let Decoded::Replace { ok, .. } = decode(Serializer::PhpSerialize, b"", &mut ctx).unwrap()
        else {
            panic!()
        };
        assert!(ok);
        assert!(ctx.diagnostics.is_empty());
    }

    #[test]
    fn binary_decode() {
        let mut ctx = CollectingContext::new();
        let Decoded::Merge { entries, ok } = decode(
            Serializer::PhpBinary,
            b"\x03fooi:1;\x03bars:1:\"x\";",
            &mut ctx,
        )
        .unwrap() else {
            panic!()
        };
        assert!(ok);
        assert_eq!(entries.len(), 2);
        let Decoded::Merge { ok, .. } =
            decode(Serializer::PhpBinary, b"\x09foo", &mut ctx).unwrap()
        else {
            panic!()
        };
        assert!(!ok);
    }
}
