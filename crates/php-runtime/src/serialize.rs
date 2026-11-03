//! `serialize()` / `unserialize()` (`ext/standard/var.c`,
//! `ext/standard/var_unserializer.re`).
//!
//! Scalars, arrays and references are handled here; objects go through the
//! [`RuntimeContext`] hooks ([`RuntimeContext::serialize_object`],
//! [`RuntimeContext::unserialize_object_create`] and friends) because they
//! involve `__serialize`, `__sleep`, `__wakeup`, `Serializable` and class
//! lookup, which live in the VM.

use crate::array::{ArrayKey, HashTable, ZArray};
use crate::error::{ErrorLevel, RtResult, RuntimeContext};
use crate::float::zend_gcvt;
use crate::numeric::{handle_numeric_str, strtod};
use crate::string::ZStr;
use crate::value::{PhpRef, Value};
use std::collections::HashMap;

/// How an object serializes itself (decided by the VM).
#[derive(Debug, Clone)]
pub enum SerializedObject {
    /// `O:len:"Class":n:{...}` with the given (mangled-key) property table
    /// (from `__serialize`, `__sleep` or the default property table).
    Object {
        /// Class name written to the output.
        class: ZStr,
        /// Properties / `__serialize` data.
        props: ZArray,
    },
    /// `E:len:"Class:Case";` for enum cases.
    Enum {
        /// Enum class name.
        class: ZStr,
        /// Case name.
        case: ZStr,
    },
    /// `C:len:"Class":n:{data}` for `Serializable` implementations.
    Custom {
        /// Class name.
        class: ZStr,
        /// Payload returned by `serialize()`.
        data: Vec<u8>,
    },
    /// Serialization failed without exception: written as `N;` and later
    /// references to the object also become `N;`.
    Null,
}

// ---------------------------------------------------------------------------
// serialize
// ---------------------------------------------------------------------------

struct Serializer {
    out: Vec<u8>,
    n: i64,
    seen: HashMap<usize, i64>,
}

/// `serialize($value)`.
pub fn serialize(value: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Vec<u8>> {
    let mut s = Serializer {
        out: Vec::new(),
        n: 0,
        seen: HashMap::new(),
    };
    s.value(value, ctx)?;
    Ok(s.out)
}

fn write_string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(format!("s:{}:\"", s.len()).as_bytes());
    out.extend_from_slice(s);
    out.extend_from_slice(b"\";");
}

fn write_key(out: &mut Vec<u8>, k: &ArrayKey) {
    match k {
        ArrayKey::Int(i) => out.extend_from_slice(format!("i:{i};").as_bytes()),
        ArrayKey::Str(s) => write_string(out, s.as_bytes()),
    }
}

impl Serializer {
    fn value(&mut self, v: &Value, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<()> {
        self.n += 1;
        let key = match v {
            Value::Reference(r) => match &*r.borrow() {
                Value::Object(o) => Some(o.addr()),
                _ => Some(r.addr()),
            },
            Value::Object(o) => Some(o.addr()),
            _ => None,
        };
        if let Some(key) = key {
            if let Some(&id) = self.seen.get(&key) {
                if v.is_reference() && id != -1 {
                    self.n -= 1;
                }
                if id == -1 {
                    self.out.extend_from_slice(b"N;");
                } else if v.is_reference() {
                    self.out.extend_from_slice(format!("R:{id};").as_bytes());
                } else {
                    self.out.extend_from_slice(format!("r:{id};").as_bytes());
                }
                return Ok(());
            }
            self.seen.insert(key, self.n);
        }
        let v = v.deref();
        match &v {
            Value::Null | Value::Resource(_) => {
                // Resources serialize as integer 0 in PHP ("i:0;").
                if matches!(v, Value::Resource(_)) {
                    self.out.extend_from_slice(b"i:0;");
                } else {
                    self.out.extend_from_slice(b"N;");
                }
            }
            Value::Bool(b) => self
                .out
                .extend_from_slice(if *b { b"b:1;" } else { b"b:0;" }),
            Value::Int(i) => self.out.extend_from_slice(format!("i:{i};").as_bytes()),
            Value::Float(d) => {
                let p = i32::try_from(ctx.serialize_precision().clamp(-1, 1000)).unwrap_or(17);
                self.out.extend_from_slice(b"d:");
                self.out
                    .extend_from_slice(zend_gcvt(*d, p, b'.', b'E').as_bytes());
                self.out.push(b';');
            }
            Value::Str(s) => write_string(&mut self.out, s.as_bytes()),
            Value::Array(a) => self.table(b"a", None, a, ctx)?,
            Value::Object(o) => match ctx.serialize_object(o)? {
                SerializedObject::Object { class, props } => {
                    self.table(b"O", Some(&class), &props, ctx)?;
                }
                SerializedObject::Enum { class, case } => {
                    self.out.extend_from_slice(
                        format!("E:{}:\"", class.len() + 1 + case.len()).as_bytes(),
                    );
                    self.out.extend_from_slice(class.as_bytes());
                    self.out.push(b':');
                    self.out.extend_from_slice(case.as_bytes());
                    self.out.extend_from_slice(b"\";");
                }
                SerializedObject::Custom { class, data } => {
                    self.out
                        .extend_from_slice(format!("C:{}:\"", class.len()).as_bytes());
                    self.out.extend_from_slice(class.as_bytes());
                    self.out
                        .extend_from_slice(format!("\":{}:{{", data.len()).as_bytes());
                    self.out.extend_from_slice(&data);
                    self.out.push(b'}');
                }
                SerializedObject::Null => {
                    self.seen.insert(o.addr(), -1);
                    self.out.extend_from_slice(b"N;");
                }
            },
            Value::Reference(_) => unreachable!("dereferenced"),
        }
        Ok(())
    }

    fn table(
        &mut self,
        tag: &[u8],
        class: Option<&ZStr>,
        t: &HashTable,
        ctx: &mut (impl RuntimeContext + ?Sized),
    ) -> RtResult<()> {
        self.out.extend_from_slice(tag);
        self.out.push(b':');
        if let Some(c) = class {
            self.out
                .extend_from_slice(format!("{}:\"", c.len()).as_bytes());
            self.out.extend_from_slice(c.as_bytes());
            self.out.extend_from_slice(b"\":");
        }
        self.out
            .extend_from_slice(format!("{}:{{", t.len()).as_bytes());
        for (k, v) in t {
            write_key(&mut self.out, k);
            match v {
                Value::Reference(r) if r.refcount() == 1 => {
                    let inner = r.get();
                    self.value(&inner, ctx)?;
                }
                other => self.value(other, ctx)?,
            }
        }
        self.out.push(b'}');
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// unserialize
// ---------------------------------------------------------------------------

enum Node {
    Scalar(Value),
    Array(Vec<(ArrayKey, usize)>),
    /// `R:` — reference to another node.
    Ref(usize),
    /// `r:` — the same object as another node.
    ObjRef(usize),
    Object {
        class: ZStr,
        props: Vec<(ArrayKey, usize)>,
    },
    Enum {
        class: ZStr,
        case: ZStr,
    },
    Custom {
        class: ZStr,
        data: Vec<u8>,
    },
}

struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
    nodes: Vec<Node>,
    /// `var_hash` slots: slot id - 1 → node index.
    slots: Vec<usize>,
    max_depth: usize,
    depth: usize,
    depth_error: bool,
}

/// Result of a failed parse step: the C code's `return 0` (with `*p` set).
struct Fail;

impl Parser<'_> {
    fn at(&self, i: usize) -> u8 {
        self.data.get(i).copied().unwrap_or(0)
    }

    /// `uiv = [0-9]+` starting at `i`; returns (value, end).
    fn uiv(&self, i: usize) -> Option<(u64, usize)> {
        let mut j = i;
        let mut v: u64 = 0;
        while self.at(j).is_ascii_digit() && j < self.data.len() {
            v = v
                .wrapping_mul(10)
                .wrapping_add(u64::from(self.at(j) - b'0'));
            j += 1;
        }
        (j > i).then_some((v, j))
    }

    /// `iv = [+-]? [0-9]+`; returns the end index.
    fn iv_end(&self, i: usize) -> Option<usize> {
        let mut j = i;
        if matches!(self.at(j), b'+' | b'-') {
            j += 1;
        }
        let (_, end) = self.uiv(j)?;
        Some(end)
    }

    /// `parse_iv2(p, &q)`: optional sign and digits (possibly none) starting
    /// at `i`; returns the value and the end position.
    fn parse_iv2(&self, i: usize) -> (i64, usize) {
        let mut j = i;
        if matches!(self.at(j), b'+' | b'-') {
            j += 1;
        }
        while j < self.data.len() && self.at(j).is_ascii_digit() {
            j += 1;
        }
        (self.parse_iv(i, j).0, j)
    }

    /// `parse_iv2` with saturation on overflow.
    fn parse_iv(&self, i: usize, end: usize) -> (i64, bool) {
        let text = &self.data[i..end];
        let (neg, digits) = match text.first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        let mut v: u128 = 0;
        let mut overflow = false;
        for &c in digits {
            v = v * 10 + u128::from(c - b'0');
            if v > u128::from(u64::MAX) {
                overflow = true;
                v = u128::from(u64::MAX);
            }
        }
        let limit = if neg { 1u128 << 63 } else { (1u128 << 63) - 1 };
        if overflow || v > limit {
            return (if neg { i64::MIN } else { i64::MAX }, true);
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let r = if neg {
            (v as u64).wrapping_neg() as i64
        } else {
            v as i64
        };
        (r, false)
    }

    fn push_node(&mut self, n: Node, slot: Option<usize>) -> usize {
        let idx = self.nodes.len();
        self.nodes.push(n);
        if let Some(s) = slot {
            self.slots[s] = idx;
        }
        idx
    }

    /// Parses one value; `as_key` means `var_hash == NULL` (array keys).
    #[allow(clippy::too_many_lines)]
    fn value(
        &mut self,
        as_key: bool,
        ctx: &mut (impl RuntimeContext + ?Sized),
    ) -> Result<usize, Fail> {
        let start = self.pos;
        if start >= self.data.len() {
            return Err(Fail);
        }
        let slot = if !as_key && self.data[start] != b'R' {
            self.slots.push(usize::MAX);
            Some(self.slots.len() - 1)
        } else {
            None
        };
        let tag = self.data[start];
        let colon = self.at(start + 1) == b':';
        match (tag, colon) {
            (b'N', _) if self.at(start + 1) == b';' => {
                self.pos = start + 2;
                Ok(self.push_node(Node::Scalar(Value::Null), slot))
            }
            (b'b', true)
                if matches!(self.at(start + 2), b'0' | b'1') && self.at(start + 3) == b';' =>
            {
                self.pos = start + 4;
                let b = self.at(start + 2) == b'1';
                Ok(self.push_node(Node::Scalar(Value::Bool(b)), slot))
            }
            (b'i', true) => {
                let end = self.iv_end(start + 2).ok_or(Fail)?;
                if self.at(end) != b';' {
                    return Err(Fail);
                }
                let (v, overflow) = self.parse_iv(start + 2, end);
                self.pos = end + 1;
                if overflow {
                    let _ = ctx.diagnostic(
                        ErrorLevel::Warning,
                        "unserialize(): Numerical result out of range",
                    );
                }
                Ok(self.push_node(Node::Scalar(Value::Int(v)), slot))
            }
            (b'd', true) => {
                let rest = &self.data[start + 2..];
                for (lit, v) in [
                    (&b"NAN;"[..], f64::NAN),
                    (&b"INF;"[..], f64::INFINITY),
                    (&b"-INF;"[..], f64::NEG_INFINITY),
                ] {
                    if rest.starts_with(lit) {
                        self.pos = start + 2 + lit.len();
                        return Ok(self.push_node(Node::Scalar(Value::Float(v)), slot));
                    }
                }
                let end = self.float_end(start + 2).ok_or(Fail)?;
                if self.at(end) != b';' {
                    return Err(Fail);
                }
                let (v, _) = strtod(&self.data[start + 2..end]);
                self.pos = end + 1;
                Ok(self.push_node(Node::Scalar(Value::Float(v)), slot))
            }
            (b's' | b'S', true) => {
                let (len, after) = self.uiv(start + 2).ok_or(Fail)?;
                if self.at(after) != b':' || self.at(after + 1) != b'"' {
                    return Err(Fail);
                }
                let body = after + 2;
                let maxlen = self.data.len() - body;
                let len = usize::try_from(len).unwrap_or(usize::MAX);
                if maxlen < len {
                    self.pos = start + 2;
                    return Err(Fail);
                }
                let s = if tag == b'S' {
                    self.unescape_s(body, len).ok_or(Fail)?
                } else {
                    (self.data[body..body + len].to_vec(), body + len)
                };
                let end = s.1;
                if self.at(end) != b'"' {
                    self.pos = end;
                    return Err(Fail);
                }
                if self.at(end + 1) != b';' {
                    self.pos = end + 1;
                    return Err(Fail);
                }
                self.pos = end + 2;
                if tag == b'S' {
                    let _ = ctx.diagnostic(
                        ErrorLevel::Deprecated,
                        "unserialize(): Unserializing the 'S' format is deprecated",
                    );
                }
                Ok(self.push_node(Node::Scalar(Value::Str(ZStr::new(s.0))), slot))
            }
            (b'a', true) if !as_key => {
                let end = self.iv_end(start + 2).ok_or(Fail)?;
                if self.at(end) != b':' || self.at(end + 1) != b'{' {
                    return Err(Fail);
                }
                let (elements, _) = self.parse_iv(start + 2, end);
                self.pos = end + 2;
                if !(0..0x4000_0000).contains(&elements) {
                    return Err(Fail);
                }
                let idx = self.push_node(Node::Array(Vec::new()), slot);
                let items = self.nested(elements, ctx)?;
                self.nodes[idx] = Node::Array(items);
                self.finish_nested()?;
                Ok(idx)
            }
            (b'R' | b'r', true) if !as_key => {
                let (id, end) = self.uiv(start + 2).ok_or(Fail)?;
                if self.at(end) != b';' {
                    return Err(Fail);
                }
                self.pos = end + 1;
                let id = usize::try_from(id).unwrap_or(0);
                if id == 0 || id > self.slots.len() {
                    return Err(Fail);
                }
                let target = self.slots[id - 1];
                if target == usize::MAX {
                    // The slot being filled right now (self reference).
                    return Err(Fail);
                }
                if tag == b'R' {
                    Ok(self.push_node(Node::Ref(target), slot))
                } else {
                    let mut t = target;
                    while let Node::Ref(next) | Node::ObjRef(next) = self.nodes[t] {
                        t = next;
                    }
                    if !matches!(
                        self.nodes[t],
                        Node::Object { .. } | Node::Enum { .. } | Node::Custom { .. }
                    ) {
                        return Err(Fail);
                    }
                    Ok(self.push_node(Node::ObjRef(t), slot))
                }
            }
            (b'O' | b'C', true) if !as_key => self.object(start, tag == b'C', slot, ctx),
            (b'E', true) if !as_key => {
                let (len, after) = self.uiv(start + 2).ok_or(Fail)?;
                if self.at(after) != b':' || self.at(after + 1) != b'"' {
                    return Err(Fail);
                }
                let body = after + 2;
                let len = usize::try_from(len).unwrap_or(usize::MAX);
                if self.data.len() - body < len {
                    self.pos = start + 2;
                    return Err(Fail);
                }
                let end = body + len;
                if self.at(end) != b'"' {
                    self.pos = end;
                    return Err(Fail);
                }
                if self.at(end + 1) != b';' {
                    self.pos = end + 1;
                    return Err(Fail);
                }
                let text = &self.data[body..end];
                let Some(colon) = text.iter().position(|&c| c == b':') else {
                    let _ = ctx.diagnostic(
                        ErrorLevel::Warning,
                        &format!(
                            "unserialize(): Invalid enum name '{}' (missing colon)",
                            String::from_utf8_lossy(text)
                        ),
                    );
                    return Err(Fail);
                };
                let class = ZStr::from(&text[..colon]);
                let case = ZStr::from(&text[colon + 1..]);
                self.pos = end + 2;
                Ok(self.push_node(Node::Enum { class, case }, slot))
            }
            _ => Err(Fail),
        }
    }

    /// End of `d:` payload matching `iv | nv | nvexp`.
    fn float_end(&self, i: usize) -> Option<usize> {
        let mut j = i;
        if matches!(self.at(j), b'+' | b'-') {
            j += 1;
        }
        let int_start = j;
        while self.at(j).is_ascii_digit() && j < self.data.len() {
            j += 1;
        }
        let int_digits = j - int_start;
        if self.at(j) == b'.' {
            let f = j + 1;
            let mut k = f;
            while self.at(k).is_ascii_digit() && k < self.data.len() {
                k += 1;
            }
            if int_digits == 0 && k == f {
                return None;
            }
            j = k;
        } else if int_digits == 0 {
            return None;
        }
        if matches!(self.at(j), b'e' | b'E') {
            let e = self.iv_end(j + 1)?;
            j = e;
        }
        Some(j)
    }

    /// Decodes the escaped `S:` format (`\xx` hex escapes).
    fn unescape_s(&self, body: usize, len: usize) -> Option<(Vec<u8>, usize)> {
        let mut out = Vec::with_capacity(len);
        let mut i = body;
        for _ in 0..len {
            if i >= self.data.len() {
                return None;
            }
            if self.data[i] == b'\\' {
                let hi = (*self.data.get(i + 1)? as char).to_digit(16)?;
                let lo = (*self.data.get(i + 2)? as char).to_digit(16)?;
                #[allow(clippy::cast_possible_truncation)]
                out.push((hi * 16 + lo) as u8);
                i += 3;
            } else {
                out.push(self.data[i]);
                i += 1;
            }
        }
        Some((out, i))
    }

    /// `process_nested_array_data`.
    fn nested(
        &mut self,
        elements: i64,
        ctx: &mut (impl RuntimeContext + ?Sized),
    ) -> Result<Vec<(ArrayKey, usize)>, Fail> {
        if self.max_depth > 0 && self.depth >= self.max_depth {
            self.depth_error = true;
            let _ = ctx.diagnostic(
                ErrorLevel::Warning,
                &format!(
                    "unserialize(): Maximum depth of {} exceeded. The depth limit can be changed using the max_depth unserialize() option or the unserialize_max_depth ini setting",
                    self.max_depth
                ),
            );
            return Err(Fail);
        }
        self.depth += 1;
        let mut items: Vec<(ArrayKey, usize)> = Vec::new();
        let mut remaining = elements;
        while remaining > 0 {
            remaining -= 1;
            let k = self.value(true, ctx)?;
            let key = match &self.nodes[k] {
                Node::Scalar(Value::Int(i)) => ArrayKey::Int(*i),
                Node::Scalar(Value::Str(s)) => match handle_numeric_str(s.as_bytes()) {
                    Some(i) => ArrayKey::Int(i),
                    None => ArrayKey::Str(s.clone()),
                },
                _ => return Err(Fail),
            };
            let v = self.value(false, ctx)?;
            if let Some(e) = items.iter_mut().find(|e| e.0 == key) {
                e.1 = v;
            } else {
                items.push((key, v));
            }
            if remaining > 0 {
                let prev = self.at(self.pos.wrapping_sub(1));
                if prev != b';' && prev != b'}' {
                    return Err(Fail);
                }
            }
        }
        self.depth -= 1;
        Ok(items)
    }

    fn finish_nested(&mut self) -> Result<(), Fail> {
        if self.pos >= self.data.len() || self.data[self.pos] != b'}' {
            return Err(Fail);
        }
        self.pos += 1;
        Ok(())
    }

    fn object(
        &mut self,
        start: usize,
        custom: bool,
        slot: Option<usize>,
        ctx: &mut (impl RuntimeContext + ?Sized),
    ) -> Result<usize, Fail> {
        let (len, after) = self.uiv(start + 2).ok_or(Fail)?;
        if self.at(after) != b':' || self.at(after + 1) != b'"' {
            return Err(Fail);
        }
        let body = after + 2;
        let len = usize::try_from(len).unwrap_or(usize::MAX);
        if self.data.len() - body < len || len == 0 {
            self.pos = start + 2;
            return Err(Fail);
        }
        let end = body + len;
        if self.at(end) != b'"' {
            self.pos = end;
            return Err(Fail);
        }
        if self.at(end + 1) != b':' {
            self.pos = end + 1;
            return Err(Fail);
        }
        let name = &self.data[body..end];
        if name[0] == 0 || name[0] == b'\\' {
            return Err(Fail);
        }
        let class = ZStr::from(name);
        self.pos = end;
        if custom {
            // C:len:"Class":datalen:{data}
            let (datalen, dl_end) = self.parse_iv2(end + 2);
            self.pos = dl_end;
            if self.data.len() - self.pos < 2 || self.at(self.pos) != b':' {
                return Err(Fail);
            }
            if self.at(self.pos + 1) != b'{' {
                self.pos += 1;
                return Err(Fail);
            }
            self.pos += 2;
            let avail = self.data.len() - self.pos;
            let dlen = usize::try_from(datalen).unwrap_or(usize::MAX);
            if datalen < 0 || avail <= dlen {
                let _ = ctx.diagnostic(
                    ErrorLevel::Warning,
                    &format!(
                        "Insufficient data for unserializing - {datalen} required, {avail} present"
                    ),
                );
                return Err(Fail);
            }
            if self.data[self.pos + dlen] != b'}' {
                self.pos += dlen;
                return Err(Fail);
            }
            let data = self.data[self.pos..self.pos + dlen].to_vec();
            self.pos += dlen + 1;
            return Ok(self.push_node(Node::Custom { class, data }, slot));
        }
        // O:len:"Class":n:{props}
        let (elements, n_end) = self.parse_iv2(end + 2);
        if elements < 0 {
            return Err(Fail);
        }
        self.pos = n_end;
        if self.at(n_end) != b':' {
            return Err(Fail);
        }
        if self.at(n_end + 1) != b'{' {
            self.pos = n_end + 1;
            return Err(Fail);
        }
        self.pos = n_end + 2;
        let idx = self.push_node(
            Node::Object {
                class: class.clone(),
                props: Vec::new(),
            },
            slot,
        );
        let props = self.nested(elements, ctx)?;
        // Property tables keep string keys verbatim.
        let props = props
            .into_iter()
            .map(|(k, v)| match k {
                ArrayKey::Int(i) => (ArrayKey::Str(ZStr::from(i.to_string())), v),
                ArrayKey::Str(s) => (ArrayKey::Str(s), v),
            })
            .collect();
        self.nodes[idx] = Node::Object { class, props };
        self.finish_nested()?;
        Ok(idx)
    }
}

/// Options for [`unserialize`].
#[derive(Debug, Clone, Default)]
pub struct UnserializeOptions {
    /// `max_depth` (0 = unlimited).
    pub max_depth: usize,
}

/// `unserialize($data)`. Returns `Ok(None)` (PHP's `false`) on malformed
/// input after emitting "`unserialize()`: Error at offset X of Y bytes"; emits
/// "`unserialize()`: Extra data starting at offset X of Y bytes" for trailing
/// bytes after a valid value.
pub fn unserialize(
    data: &[u8],
    options: &UnserializeOptions,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Option<Value>> {
    let mut p = Parser {
        data,
        pos: 0,
        nodes: Vec::new(),
        slots: Vec::new(),
        max_depth: options.max_depth,
        depth: 0,
        depth_error: false,
    };
    let Ok(root) = p.value(false, ctx) else {
        if !data.is_empty() {
            ctx.diagnostic(
                ErrorLevel::Warning,
                &format!(
                    "unserialize(): Error at offset {} of {} bytes",
                    p.pos,
                    data.len()
                ),
            )?;
        }
        return Ok(None);
    };
    let mut b = Builder {
        nodes: &p.nodes,
        referenced: vec![false; p.nodes.len()],
        refs: HashMap::new(),
        objects: HashMap::new(),
    };
    for n in &p.nodes {
        if let Node::Ref(t) = n {
            b.referenced[*t] = true;
        }
    }
    let mut v = b.build(root, ctx)?;
    v.unwrap_reference();
    if p.pos < data.len() {
        ctx.diagnostic(
            ErrorLevel::Warning,
            &format!(
                "unserialize(): Extra data starting at offset {} of {} bytes",
                p.pos,
                data.len()
            ),
        )?;
    }
    Ok(Some(v))
}

struct Builder<'a> {
    nodes: &'a [Node],
    referenced: Vec<bool>,
    refs: HashMap<usize, PhpRef>,
    objects: HashMap<usize, Value>,
}

impl Builder<'_> {
    fn build(&mut self, idx: usize, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
        if self.referenced[idx] {
            if let Some(r) = self.refs.get(&idx) {
                return Ok(Value::Reference(r.clone()));
            }
            let r = PhpRef::new(Value::Null);
            self.refs.insert(idx, r.clone());
            let v = self.build_raw(idx, ctx)?;
            r.set(v);
            return Ok(Value::Reference(r));
        }
        self.build_raw(idx, ctx)
    }

    fn build_table(
        &mut self,
        items: &[(ArrayKey, usize)],
        ctx: &mut (impl RuntimeContext + ?Sized),
    ) -> RtResult<HashTable> {
        // PHP's unserializer uses `array_finit_size_mixed`: non-empty arrays
        // are always hash tables, empty ones are the (packed) empty array.
        let mut t = if items.is_empty() {
            HashTable::new()
        } else {
            HashTable::with_capacity_mixed(items.len())
        };
        for (k, v) in items {
            let val = self.build(*v, ctx)?;
            match k {
                ArrayKey::Str(s) => {
                    t.insert_str_raw(s.clone(), val);
                }
                ArrayKey::Int(i) => {
                    t.insert(*i, val);
                }
            }
        }
        Ok(t)
    }

    fn build_raw(
        &mut self,
        idx: usize,
        ctx: &mut (impl RuntimeContext + ?Sized),
    ) -> RtResult<Value> {
        let nodes = self.nodes;
        match &nodes[idx] {
            Node::Scalar(v) => Ok(v.clone()),
            Node::Array(items) => Ok(Value::Array(self.build_table(items, ctx)?.into())),
            Node::Ref(t) => self.build(*t, ctx),
            Node::ObjRef(t) => {
                let v = self.build(*t, ctx)?;
                Ok(v.deref())
            }
            Node::Object { class, props } => {
                if let Some(o) = self.objects.get(&idx) {
                    return Ok(o.clone());
                }
                let obj = ctx.unserialize_object_create(class)?;
                self.objects.insert(idx, obj.clone());
                let table = self.build_table(props, ctx)?;
                ctx.unserialize_object_finish(&obj, ZArray::from_table(table))?;
                Ok(obj)
            }
            Node::Enum { class, case } => {
                if let Some(o) = self.objects.get(&idx) {
                    return Ok(o.clone());
                }
                let v = ctx.unserialize_enum(class, case)?;
                self.objects.insert(idx, v.clone());
                Ok(v)
            }
            Node::Custom { class, data } => {
                if let Some(o) = self.objects.get(&idx) {
                    return Ok(o.clone());
                }
                let v = ctx.unserialize_custom(class, data)?;
                self.objects.insert(idx, v.clone());
                Ok(v)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::error::CollectingContext;

    fn ser(v: &Value) -> String {
        let mut c = CollectingContext::new();
        String::from_utf8(serialize(v, &mut c).unwrap()).unwrap()
    }

    fn unser(s: &str) -> (Option<Value>, Vec<String>) {
        let mut c = CollectingContext::new();
        let v = unserialize(s.as_bytes(), &UnserializeOptions::default(), &mut c).unwrap();
        (v, c.messages())
    }

    #[test]
    fn scalars() {
        assert_eq!(ser(&Value::Null), "N;");
        assert_eq!(ser(&Value::Bool(true)), "b:1;");
        assert_eq!(ser(&Value::Int(-5)), "i:-5;");
        assert_eq!(ser(&Value::Float(0.1)), "d:0.1;");
        assert_eq!(ser(&Value::Float(1.0)), "d:1;");
        assert_eq!(ser(&Value::Float(1e100)), "d:1.0E+100;");
        assert_eq!(ser(&Value::Float(-0.0)), "d:-0;");
        assert_eq!(ser(&Value::Float(f64::NAN)), "d:NAN;");
        assert_eq!(ser(&Value::Float(f64::NEG_INFINITY)), "d:-INF;");
        assert_eq!(ser(&Value::from("a\"b")), "s:3:\"a\"b\";");
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn arrays_and_references() {
        // $x = 1; $a = [&$x, &$x, "k" => [1.5]];
        let x = PhpRef::new(Value::Int(1));
        let mut t = HashTable::new();
        t.push(Value::Reference(x.clone())).unwrap();
        t.push(Value::Reference(x.clone())).unwrap();
        t.insert("k", Value::Array(ZArray::from_values([Value::Float(1.5)])));
        let a = Value::Array(t.into());
        let s = ser(&a);
        assert_eq!(s, "a:3:{i:0;i:1;i:1;R:2;s:1:\"k\";a:1:{i:0;d:1.5;}}");
        let (v, msgs) = unser(&s);
        assert!(msgs.is_empty());
        let v = v.unwrap();
        let arr = v.as_array().unwrap();
        let (Some(Value::Reference(r0)), Some(Value::Reference(r1))) =
            (arr.get_int(0), arr.get_int(1))
        else {
            panic!("expected references")
        };
        assert!(PhpRef::ptr_eq(r0, r1));
        r0.set(Value::Int(9));
        assert_eq!(r1.get().as_int(), Some(9));
        // Re-serializing gives the same string back.
        drop(x);
        assert_eq!(ser(&v).replace("i:9;", "i:1;"), s);
    }

    #[test]
    fn errors_and_offsets() {
        assert_eq!(unser("").0.map(|_| ()), None);
        assert!(unser("").1.is_empty());
        let (v, m) = unser("i:5");
        assert!(v.is_none());
        assert_eq!(m, ["Warning: unserialize(): Error at offset 0 of 3 bytes"]);
        let (_, m) = unser("s:5:\"abc\";");
        assert_eq!(
            m,
            ["Warning: unserialize(): Error at offset 10 of 10 bytes"]
        );
        let (_, m) = unser("s:50:\"abc\";");
        assert_eq!(m, ["Warning: unserialize(): Error at offset 2 of 11 bytes"]);
        let (_, m) = unser("a:1:{i:0;i:1;");
        assert_eq!(
            m,
            ["Warning: unserialize(): Error at offset 13 of 13 bytes"]
        );
        let (_, m) = unser("b:2;");
        assert_eq!(m, ["Warning: unserialize(): Error at offset 0 of 4 bytes"]);
        // a:1:{i:0;R:1;} is an array containing a reference to itself.
        let (v, m) = unser("a:1:{i:0;R:1;}");
        assert!(m.is_empty());
        let v = v.unwrap();
        assert!(v
            .as_array()
            .unwrap()
            .get_int(0)
            .is_some_and(Value::is_reference));
        let (_, m) = unser("a:2:{i:0;i:1;}");
        assert_eq!(
            m,
            ["Warning: unserialize(): Error at offset 13 of 14 bytes"]
        );
        let (v, m) = unser("i:5;xyz");
        assert_eq!(v.and_then(|v| v.as_int()), Some(5));
        assert_eq!(
            m,
            ["Warning: unserialize(): Extra data starting at offset 4 of 7 bytes"]
        );
        let (v, _) = unser("a:1:{s:1:\"5\";b:1;}");
        assert!(v.unwrap().as_array().unwrap().get_int(5).is_some());
        // Unserialized arrays are hash tables, like PHP's.
        let (v, _) = unser("a:2:{i:0;i:1;i:1;i:2;}");
        let v = v.unwrap();
        assert!(!v.as_array().unwrap().is_packed());
        assert!(unser("a:0:{}").0.unwrap().as_array().unwrap().is_packed());
        let (v, _) = unser("d:1.5e3;");
        assert_eq!(v.and_then(|v| v.as_float()), Some(1500.0));
    }
}
