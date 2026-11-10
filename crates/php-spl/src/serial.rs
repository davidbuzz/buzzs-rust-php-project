//! Helpers for the legacy `Serializable` formats of SPL classes, which
//! embed several `php_var_unserialize` calls in one buffer: a scanner that
//! finds the extent of one serialized value, and a context wrapper that
//! unserializes it without emitting `unserialize()` warnings.

use crate::host::SplHost;
use php_runtime::serialize::{unserialize_partial, EnumCaseLookup, SerializedObject};
use php_runtime::{
    BinaryOp, CastTarget, ErrorLevel, Obj, ObjectDump, PropertyPurpose, RtResult, RuntimeContext,
    Value, ZArray, ZStr,
};

fn digits(buf: &[u8], mut p: usize, signed: bool) -> Option<(i64, usize)> {
    let start = p;
    if signed && matches!(buf.get(p), Some(b'-' | b'+')) {
        p += 1;
    }
    let ds = p;
    while buf.get(p).is_some_and(u8::is_ascii_digit) {
        p += 1;
    }
    if p == ds {
        return None;
    }
    let s = std::str::from_utf8(&buf[start..p]).ok()?;
    Some((s.parse().unwrap_or(i64::MAX), p))
}

fn expect(buf: &[u8], p: usize, c: u8) -> Option<usize> {
    (buf.get(p) == Some(&c)).then_some(p + 1)
}

fn quoted(buf: &[u8], p: usize) -> Option<usize> {
    let (len, p) = digits(buf, p, false)?;
    let p = expect(buf, p, b':')?;
    let p = expect(buf, p, b'"')?;
    let end = p.checked_add(usize::try_from(len).ok()?)?;
    if end > buf.len() {
        return None;
    }
    expect(buf, end, b'"')
}

/// Returns the position just after the serialized value starting at `p`,
/// or `None` if the value is malformed.
#[must_use]
pub fn value_end(buf: &[u8], p: usize) -> Option<usize> {
    let tag = *buf.get(p)?;
    let p = expect(buf, p + 1, b':').or_else(|| (tag == b'N').then_some(p + 1))?;
    match tag {
        b'N' => expect(buf, p, b';'),
        b'b' | b'i' | b'r' | b'R' => {
            let (_, p) = digits(buf, p, tag == b'i')?;
            expect(buf, p, b';')
        }
        b'd' => {
            let mut q = p;
            while buf.get(q).is_some_and(|c| *c != b';') {
                q += 1;
            }
            if q == p {
                return None;
            }
            expect(buf, q, b';')
        }
        b's' | b'E' => {
            let p = quoted(buf, p)?;
            expect(buf, p, b';')
        }
        b'S' => {
            let (len, mut q) = digits(buf, p, false)?;
            q = expect(buf, q, b':')?;
            q = expect(buf, q, b'"')?;
            for _ in 0..len {
                if buf.get(q) == Some(&b'\\') {
                    q += 3;
                } else {
                    q += 1;
                }
            }
            q = expect(buf, q, b'"')?;
            expect(buf, q, b';')
        }
        b'a' => {
            let (n, q) = digits(buf, p, false)?;
            elements(buf, q, n)
        }
        b'O' => {
            let q = quoted(buf, p)?;
            let q = expect(buf, q, b':')?;
            let (n, q) = digits(buf, q, false)?;
            elements(buf, q, n)
        }
        b'C' => {
            let q = quoted(buf, p)?;
            let q = expect(buf, q, b':')?;
            let (len, q) = digits(buf, q, false)?;
            let q = expect(buf, q, b':')?;
            let q = expect(buf, q, b'{')?;
            let end = q.checked_add(usize::try_from(len).ok()?)?;
            expect(buf, end, b'}')
        }
        _ => None,
    }
}

fn elements(buf: &[u8], p: usize, n: i64) -> Option<usize> {
    let mut q = expect(buf, p, b':')?;
    q = expect(buf, q, b'{')?;
    for _ in 0..n.max(0) {
        q = value_end(buf, q)?;
        q = value_end(buf, q)?;
    }
    expect(buf, q, b'}')
}

/// A [`RuntimeContext`] that forwards everything to the host but drops the
/// `unserialize()` warnings (the SPL caller reports its own error).
struct Quiet<'a>(&'a mut dyn SplHost);

impl RuntimeContext for Quiet<'_> {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        if message.starts_with("unserialize(): ") {
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
    fn object_properties(&mut self, obj: &Obj, p: PropertyPurpose) -> RtResult<Option<ZArray>> {
        self.0.object_properties(obj, p)
    }
    fn object_dump(&mut self, obj: &Obj, p: PropertyPurpose) -> RtResult<ObjectDump> {
        self.0.object_dump(obj, p)
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
        RuntimeContext::is_enum(self.0, obj)
    }
    fn count_object(&mut self, obj: &Obj) -> RtResult<Option<i64>> {
        RuntimeContext::count_object(self.0, obj)
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

/// Unserializes one value starting at `p` (`php_var_unserialize()` with
/// the host's [`SplHost::unserialize_options`]); returns the value and the
/// position after it. `Err(pos)` reports the error offset. Exceptions thrown
/// by user code during unserialization are reported as errors at `p` (the
/// caller converts them to its own exception).
pub fn unserialize_at(
    host: &mut dyn SplHost,
    buf: &[u8],
    p: usize,
) -> Result<(Value, usize), usize> {
    match host.unserialize_nested(buf, p) {
        Some(Ok(r)) => return r,
        Some(Err(_)) => return Err(p),
        None => {}
    }
    let opts = host.unserialize_options();
    let mut q = Quiet(host);
    match unserialize_partial(buf, p, &opts, &mut q) {
        Ok(r) => r,
        Err(_) => Err(p),
    }
}

/// Rewrites the back-reference numbers (`r:N;` / `R:N;`) of the
/// serialized value at `p` by `delta`, appending the result to `out`.
/// Returns the position after the value.
fn copy_adjusting(buf: &[u8], p: usize, delta: i64, out: &mut Vec<u8>) -> Option<usize> {
    let end = value_end(buf, p)?;
    match buf.get(p)? {
        b'r' | b'R' => {
            let (n, _) = digits(buf, p + 2, false)?;
            out.extend_from_slice(&buf[p..p + 2]);
            out.extend_from_slice((n + delta).to_string().as_bytes());
            out.push(b';');
        }
        b'a' | b'O' => {
            let open = p + buf[p..end].iter().position(|&c| c == b'{')? + 1;
            out.extend_from_slice(&buf[p..open]);
            let mut q = open;
            while buf.get(q) != Some(&b'}') {
                q = copy_adjusting(buf, q, delta, out)?;
                q = copy_adjusting(buf, q, delta, out)?;
            }
            out.push(b'}');
        }
        _ => out.extend_from_slice(&buf[p..end]),
    }
    Some(end)
}

/// Serializes `values` as one `php_var_serialize` sequence sharing the
/// back-reference table (like consecutive calls with one `var_hash`), and
/// returns each value's serialization.
pub fn serialize_seq(host: &mut dyn SplHost, values: &[Value]) -> RtResult<Vec<Vec<u8>>> {
    if let Some(r) = host.serialize_values(values) {
        return r;
    }
    let all = php_runtime::serialize::serialize(
        &Value::Array(ZArray::from_values(values.iter().cloned())),
        host,
    )?;
    let mut out = Vec::with_capacity(values.len());
    let Some(open) = all.iter().position(|&c| c == b'{') else {
        return Ok(out);
    };
    let mut q = open + 1;
    while all.get(q).is_some_and(|&c| c != b'}') {
        let Some(after_key) = value_end(&all, q) else {
            break;
        };
        let mut piece = Vec::new();
        match copy_adjusting(&all, after_key, -1, &mut piece) {
            Some(e) => {
                out.push(piece);
                q = e;
            }
            None => break,
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::value_end;

    #[test]
    fn extents() {
        let s = b"i:5;:s:3:\"abc\";:a:2:{i:0;b:1;i:1;N;}O:3:\"Foo\":1:{s:1:\"a\";d:1.5;}x";
        assert_eq!(value_end(s, 0), Some(4));
        assert_eq!(value_end(s, 5), Some(15));
        let a = value_end(s, 16).unwrap();
        assert_eq!(&s[16..a], b"a:2:{i:0;b:1;i:1;N;}");
        let o = value_end(s, a).unwrap();
        assert_eq!(s[o], b'x');
        assert_eq!(value_end(b"N;", 0), Some(2));
        assert_eq!(value_end(b"i:;", 0), None);
        assert_eq!(value_end(b"s:5:\"ab\";", 0), None);
        assert_eq!(value_end(b"C:3:\"Foo\":2:{ab}", 0), Some(16));
    }
}
