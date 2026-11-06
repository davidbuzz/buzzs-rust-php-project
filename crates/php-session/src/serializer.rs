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

use php_runtime::serialize::{
    serialize, unserialize_partial, EnumCaseLookup, SerializedObject, UnserializeOptions,
};
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
    Ok(encode_with(ser, vars, &mut |keys: Option<&[ZStr]>| {
        serialize_selected(vars, keys, ctx)
    }))
}

/// The `php_var_serialize()` calls of one encoding: the entries of `vars`
/// named by `keys` in order, or the whole array for `None` (the
/// `php_serialize` handler), all with one shared `var_hash`
/// (`PHP_VAR_SERIALIZE_INIT` … `PHP_VAR_SERIALIZE_DESTROY`). Returns one
/// payload per value; `Err` is an exception thrown while serializing.
pub type SerializeValues<'a> = dyn FnMut(Option<&[ZStr]>) -> RtResult<Vec<Vec<u8>>> + 'a;

/// [`SerializeValues`] over php-runtime's serializer.
pub fn serialize_selected(
    vars: &ZArray,
    keys: Option<&[ZStr]>,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Vec<Vec<u8>>> {
    match keys {
        None => Ok(vec![serialize(&Value::Array(vars.clone()), ctx)?]),
        Some(keys) => {
            let entries: Vec<(ZStr, Value)> = keys
                .iter()
                .map(|k| {
                    let v = vars.get_str(k.as_bytes()).cloned().unwrap_or_default();
                    (k.clone(), v)
                })
                .collect();
            serialize_entries(&entries, ctx)
        }
    }
}

/// [`encode`] with the values serialized by `values` (the embedder's
/// serializer, so that a `serialize()` made from
/// `Serializable::serialize()` joins the shared `var_hash`).
pub fn encode_with(ser: Serializer, vars: &ZArray, values: &mut SerializeValues<'_>) -> Encoded {
    if ser == Serializer::PhpSerialize {
        return match values(None) {
            Ok(mut data) => Encoded {
                data: Some(data.pop().unwrap_or_default()),
                ..Encoded::default()
            },
            Err(e) => Encoded {
                exception: Some(e),
                ..Encoded::default()
            },
        };
    }
    // PS_ENCODE_LOOP: numeric keys are skipped with a warning.
    let mut warnings = Vec::new();
    let mut selected: Vec<ZStr> = Vec::new();
    let mut bad_key = None;
    for (k, _) in vars.iter() {
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
                selected.push(s.clone());
            }
        }
    }
    let (payloads, exception) = if selected.is_empty() {
        (Vec::new(), None)
    } else {
        match values(Some(&selected)) {
            Ok(v) => (v, None),
            Err(e) => (Vec::new(), Some(e)),
        }
    };
    if let Some(bad) = bad_key {
        warnings.push(format!(
            "Failed to write session data. Data contains invalid key \"{}\"",
            bad.to_string_lossy()
        ));
        return Encoded {
            data: None,
            warnings,
            exception,
        };
    }
    if exception.is_some() {
        return Encoded {
            data: None,
            warnings,
            exception,
        };
    }
    let mut out = Vec::new();
    for (key, payload) in selected.iter().zip(payloads) {
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
            // Returned early above.
            Serializer::PhpSerialize => {}
        }
        out.extend_from_slice(&payload);
    }
    Encoded {
        data: Some(out),
        warnings,
        exception: None,
    }
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
        if sc.value(&mut discard).is_none() || sc.value(&mut payload).is_none() {
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
    decode_with(ser, data, &UnserializeOptions::default(), ctx)
}

/// Decodes session data with explicit `php_var_unserialize()` options
/// (`max_depth`, the function named by diagnostics).
pub fn decode_with(
    ser: Serializer,
    data: &[u8],
    opts: &UnserializeOptions,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Decoded> {
    let mut quiet = Quiet(ctx);
    if ser == Serializer::PhpSerialize {
        // `php_var_unserialize()`: no offset / extra-data warnings.
        let result = unserialize_partial(data, 0, opts, &mut quiet)?;
        return Ok(match result {
            Ok((v, _)) => Decoded::Replace { value: v, ok: true },
            Err(_) => Decoded::Replace {
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
        if sc.value(&mut payload).is_none() {
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
        // The enclosing array adds one level the C code does not have.
        let mut opts = opts.clone();
        if opts.max_depth > 0 {
            opts.max_depth += 1;
        }
        match unserialize_partial(&combined, 0, &opts, &mut quiet)? {
            Ok((Value::Array(arr), _)) => {
                for (name, (_, v)) in names.into_iter().zip(arr.iter()) {
                    entries.push((name, v.clone()));
                }
            }
            _ => ok = false,
        }
    }
    Ok(Decoded::Merge { entries, ok })
}

/// The embedder side of [`decode_nested`].
pub trait NestedDecode {
    /// One `php_var_unserialize()` of the value at `offset` of the session
    /// data in the shared `var_hash`: `Ok(Ok((value, end)))`,
    /// `Ok(Err(failure_offset))`, or `Err` for an exception; `None`
    /// declines (no shared `var_hash` is open).
    fn unserialize_at(
        &mut self,
        data: &[u8],
        offset: usize,
    ) -> Option<RtResult<Result<(Value, usize), usize>>>;

    /// `php_var_unserialize()` into a `var_tmp_var()`: as
    /// [`NestedDecode::unserialize_at`], but the value is its `var_hash`
    /// cell (a reference), which `R:` back-references in later entries of
    /// the same decode may turn into a reference shared with them; the
    /// cells are resolved by [`NestedDecode::normalize`].
    fn unserialize_cell_at(
        &mut self,
        data: &[u8],
        offset: usize,
    ) -> Option<RtResult<Result<(Value, usize), usize>>>;

    /// `php_set_session_var()`: stores a decoded entry into `$_SESSION`
    /// before the next value is parsed (the value it replaces is released
    /// first, so its object handle can be reused). For the `php` and
    /// `php_binary` decoders `value` is the cell of
    /// [`NestedDecode::unserialize_cell_at`], PHP's `IS_PTR` placeholder.
    fn store(&mut self, name: ZStr, value: Value);

    /// `php_session_normalize_vars()`: once the data is parsed (or parsing
    /// failed), replaces the stored cells by their values; a cell an `R:`
    /// targeted stays a reference shared by every entry holding it.
    fn normalize(&mut self);
}

/// Decodes session data value by value with the embedder's
/// `php_var_unserialize()` in one shared `var_hash`
/// (`PHP_VAR_UNSERIALIZE_INIT`), which also joins an active
/// `unserialize()` call's (a `Serializable::unserialize()` calling
/// `session_decode()`, bug #70219) and is joined by `unserialize()` calls
/// made from `Serializable::unserialize()` (bug #79031): `r:`/`R:`
/// back-references resolve across those boundaries. `php`/`php_binary`
/// entries are parsed into `var_tmp_var()` cells
/// ([`NestedDecode::unserialize_cell_at`]) and stored through
/// [`NestedDecode::store`] as they are parsed (the result's `entries` is
/// empty); [`NestedDecode::normalize`] then resolves the cells, so an `R:`
/// in a later entry makes the targeted entry a reference the two share
/// (`&array(3)` in `var_dump($_SESSION)`, `session_decode_variation1.phpt`).
/// Returns `None` when the embedder declines, and [`decode_with`] applies.
pub fn decode_nested(
    ser: Serializer,
    data: &[u8],
    nested: &mut dyn NestedDecode,
) -> Option<RtResult<Decoded>> {
    if ser == Serializer::PhpSerialize {
        return Some(nested.unserialize_at(data, 0)?.map(|r| match r {
            Ok((value, _)) => Decoded::Replace { value, ok: true },
            Err(_) => Decoded::Replace {
                value: Value::empty_array(),
                ok: data.is_empty(),
            },
        }));
    }
    let mut ok = true;
    let mut thrown = None;
    let mut p = 0;
    while p < data.len() {
        let (name, value_start) = if ser == Serializer::Php {
            let Some(off) = data[p..].iter().position(|&c| c == PS_DELIMITER) else {
                ok = false;
                break;
            };
            (ZStr::from(&data[p..p + off]), p + off + 1)
        } else {
            let namelen = usize::from(data[p] & !PS_BIN_UNDEF);
            if p + namelen >= data.len() {
                ok = false;
                break;
            }
            (ZStr::from(&data[p + 1..=p + namelen]), p + 1 + namelen)
        };
        match nested.unserialize_cell_at(data, value_start) {
            Some(Ok(Ok((value, end)))) => {
                nested.store(name, value);
                p = end;
            }
            Some(Ok(Err(_))) => {
                ok = false;
                break;
            }
            Some(Err(e)) => {
                thrown = Some(e);
                break;
            }
            None => {
                // The shared `var_hash` went away mid-way (not in practice):
                // what is stored is resolved and the caller decodes anew.
                nested.normalize();
                return None;
            }
        }
    }
    // `break_outer_loop:` — the placeholders are resolved whatever happened.
    nested.normalize();
    if let Some(e) = thrown {
        return Some(Err(e));
    }
    Some(Ok(Decoded::Merge {
        entries: Vec::new(),
        ok,
    }))
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
    fn unserialize_enum_case(&mut self, class: &ZStr, case: &ZStr) -> RtResult<EnumCaseLookup> {
        self.0.unserialize_enum_case(class, case)
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

/// What [`Scanner::item`] consumed.
enum Item {
    /// A complete scalar, reference or `C:` record.
    Complete,
    /// The header of an `a:`/`O:` container with this many elements.
    Open(usize),
}

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

    /// Copies one complete value (iteratively, so deep nesting cannot
    /// overflow the stack).
    fn value(&mut self, out: &mut Vec<u8>) -> Option<()> {
        // Remaining items (keys and values) of each open container.
        let mut open: Vec<usize> = Vec::new();
        loop {
            if let Item::Open(n) = self.item(out)? {
                if open.len() >= MAX_SCAN_DEPTH {
                    return None;
                }
                open.push(n.checked_mul(2)?);
            }
            loop {
                match open.last_mut() {
                    None => return Some(()),
                    Some(rem) if *rem > 0 => {
                        *rem -= 1;
                        break;
                    }
                    Some(_) => {
                        self.expect(b'}', out)?;
                        open.pop();
                    }
                }
            }
        }
    }

    /// Copies one token; for `a:`/`O:` only the header up to `{` is copied
    /// and the number of elements is returned.
    fn item(&mut self, out: &mut Vec<u8>) -> Option<Item> {
        let tag = self.peek()?;
        match tag {
            b'N' => {
                self.raw(1, out)?;
                self.expect(b';', out)?;
            }
            b'b' | b'i' | b'd' => {
                self.raw(1, out)?;
                self.expect(b':', out)?;
                self.until_semicolon(out)?;
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
                self.expect(b';', out)?;
            }
            b's' | b'E' => {
                self.raw(1, out)?;
                self.expect(b':', out)?;
                let n = self.uint(out)?;
                self.expect(b':', out)?;
                self.quoted(n, out)?;
                self.expect(b';', out)?;
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
                self.expect(b';', out)?;
            }
            b'a' => {
                self.raw(1, out)?;
                self.expect(b':', out)?;
                let n = self.uint(out)?;
                self.expect(b':', out)?;
                self.expect(b'{', out)?;
                return Some(Item::Open(n));
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
                if tag == b'O' {
                    return Some(Item::Open(m));
                }
                self.raw(m, out)?;
                self.expect(b'}', out)?;
            }
            _ => return None,
        }
        Some(Item::Complete)
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

    /// A stand-in for the VM's shared-`var_hash` parser: scalars `i:N;`
    /// only, recording the offsets it was asked to parse.
    fn int_parser(seen: &mut Vec<usize>, data: &[u8], off: usize) -> Result<(Value, usize), usize> {
        seen.push(off);
        let rest = &data[off..];
        let end = rest.iter().position(|&c| c == b';').ok_or(off)?;
        let digits = rest.strip_prefix(b"i:").ok_or(off)?;
        let n = std::str::from_utf8(&digits[..end - 2])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(off)?;
        Ok((Value::Int(n), off + end + 1))
    }

    /// A recording [`NestedDecode`]: `parse` answers each `unserialize_at`
    /// and `unserialize_cell_at` (`cells` counts the latter), stored entries
    /// are kept in order, and `normalized` counts the `normalize()` calls.
    struct Recorder<F> {
        parse: F,
        stored: Vec<(ZStr, Value)>,
        cells: usize,
        normalized: usize,
    }

    impl<F> NestedDecode for Recorder<F>
    where
        F: FnMut(&[u8], usize) -> Option<RtResult<Result<(Value, usize), usize>>>,
    {
        fn unserialize_at(
            &mut self,
            data: &[u8],
            offset: usize,
        ) -> Option<RtResult<Result<(Value, usize), usize>>> {
            (self.parse)(data, offset)
        }

        fn unserialize_cell_at(
            &mut self,
            data: &[u8],
            offset: usize,
        ) -> Option<RtResult<Result<(Value, usize), usize>>> {
            self.cells += 1;
            (self.parse)(data, offset)
        }

        fn store(&mut self, name: ZStr, value: Value) {
            self.stored.push((name, value));
        }

        fn normalize(&mut self) {
            self.normalized += 1;
        }
    }

    fn recorder<F>(parse: F) -> Recorder<F>
    where
        F: FnMut(&[u8], usize) -> Option<RtResult<Result<(Value, usize), usize>>>,
    {
        Recorder {
            parse,
            stored: Vec::new(),
            cells: 0,
            normalized: 0,
        }
    }

    #[test]
    fn decode_nested_parses_each_value_at_its_offset() {
        let mut seen = Vec::new();
        let data = b"a|i:1;bb|i:22;";
        let mut rec = recorder(|d: &[u8], off: usize| Some(Ok(int_parser(&mut seen, d, off))));
        let r = decode_nested(Serializer::Php, data, &mut rec);
        let Some(Ok(Decoded::Merge { entries, ok })) = r else {
            panic!("expected a merge")
        };
        assert!(ok);
        // Entries are stored one by one, not returned, as the cells of
        // `unserialize_cell_at`; `normalize()` resolves them once at the end.
        assert!(entries.is_empty());
        assert_eq!((rec.cells, rec.normalized), (2, 1));
        let stored = rec.stored;
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].0.as_bytes(), b"a");
        assert!(matches!(stored[0].1, Value::Int(1)));
        assert_eq!(stored[1].0.as_bytes(), b"bb");
        assert!(matches!(stored[1].1, Value::Int(22)));
        assert_eq!(seen, vec![2, 9]);

        // php_binary: length byte, name, value; a failing value stops.
        let mut seen = Vec::new();
        let data = b"\x01xi:5;\x01yz";
        let mut rec = recorder(|d: &[u8], off: usize| Some(Ok(int_parser(&mut seen, d, off))));
        let r = decode_nested(Serializer::PhpBinary, data, &mut rec);
        let Some(Ok(Decoded::Merge { entries, ok })) = r else {
            panic!("expected a merge")
        };
        assert!(!ok);
        assert!(entries.is_empty());
        // `normalize()` runs after a failure too (`break_outer_loop`).
        assert_eq!((rec.cells, rec.normalized), (2, 1));
        let stored = rec.stored;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].0.as_bytes(), b"x");
        assert!(matches!(stored[0].1, Value::Int(5)));
    }

    #[test]
    fn decode_nested_php_serialize_and_decline() {
        let mut rec = recorder(|_: &[u8], _: usize| Some(Ok(Ok((Value::Int(7), 4)))));
        let r = decode_nested(Serializer::PhpSerialize, b"r:2;", &mut rec);
        assert!(matches!(
            r,
            Some(Ok(Decoded::Replace {
                value: Value::Int(7),
                ok: true
            }))
        ));
        assert!(rec.stored.is_empty());
        // `php_serialize` unserializes `$_SESSION` as a whole: no cells.
        assert_eq!((rec.cells, rec.normalized), (0, 0));
        let mut rec = recorder(|_: &[u8], _: usize| Some(Ok(Err(0))));
        let r = decode_nested(Serializer::PhpSerialize, b"x", &mut rec);
        assert!(matches!(r, Some(Ok(Decoded::Replace { ok: false, .. }))));
        // No active unserialize() call: the caller falls back to decode_with.
        let mut rec = recorder(|_: &[u8], _: usize| None);
        let r = decode_nested(Serializer::Php, b"a|i:1;", &mut rec);
        assert!(r.is_none());
        assert!(rec.stored.is_empty());
        // Declining still resolves whatever was stored.
        assert_eq!((rec.cells, rec.normalized), (1, 1));
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
    fn deep_nesting_is_bounded() {
        let mut ctx = CollectingContext::new();
        let mut data = b"a|".to_vec();
        for _ in 0..5000 {
            data.extend_from_slice(b"a:1:{i:0;");
        }
        data.extend_from_slice(b"N;");
        data.extend(std::iter::repeat_n(b'}', 5000));
        let Decoded::Merge { ok, .. } = decode(Serializer::Php, &data, &mut ctx).unwrap() else {
            panic!()
        };
        assert!(!ok);
        let mut sc = Scanner::new(b"S:3:\"a\\62c\";", 0);
        let mut out = Vec::new();
        assert!(sc.value(&mut out).is_some());
        assert_eq!(out, b"S:3:\"a\\62c\";");
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
