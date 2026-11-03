//! `pack.c`, `scanf.c`, `uuencode.c`, `quot_print.c`, `versioning.c`,
//! `soundex.c`, `metaphone.c` and `levenshtein.c`.

use php_runtime::{Value, ZArray};
use php_stdstr::error::{ErrorKind, Notices, StdError, StdResult};
use php_stdstr::pack::{self, PackArg, PackValue, UnpackKey, UnpackValue};
use php_stdstr::scanf::{self, ScanValue, SscanfResult};
use php_stdstr::{analysis, quoted_printable, uuencode, versioning};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::{assoc, bytes, check, deprecated_function, emit, mem, opt_bytes, to_str};

/// A `pack()` argument with its conversions computed up front (the
/// library's [`PackValue`] has no access to the VM).
struct VmPackArg {
    long: i64,
    double: f64,
    bytes: StdResult<Vec<u8>>,
}

impl PackValue for VmPackArg {
    fn to_long(&self) -> i64 {
        self.long
    }
    fn to_double(&self) -> f64 {
        self.double
    }
    fn to_bytes(&self) -> StdResult<Vec<u8>> {
        self.bytes.clone()
    }
}

impl VmPackArg {
    fn from_scalar(p: &PackArg) -> Self {
        Self {
            long: p.to_long(),
            double: p.to_double(),
            bytes: p.to_bytes(),
        }
    }

    fn new(vm: &mut Vm, v: &Value) -> VmResult<Self> {
        Ok(match v {
            Value::Null => Self::from_scalar(&PackArg::Null),
            Value::Bool(b) => Self::from_scalar(&PackArg::Bool(*b)),
            Value::Int(i) => Self::from_scalar(&PackArg::Int(*i)),
            Value::Float(f) => Self::from_scalar(&PackArg::Float(*f)),
            Value::Str(s) => Self::from_scalar(&PackArg::Str(s.as_bytes().to_vec())),
            Value::Reference(r) => Self::new(vm, &r.get())?,
            Value::Array(arr) => {
                let n = i64::from(!arr.is_empty());
                Self {
                    long: n,
                    #[allow(clippy::cast_precision_loss)]
                    double: n as f64,
                    bytes: Ok(b"Array".to_vec()),
                }
            }
            other => {
                let s = vm.value_to_string(other);
                Self {
                    long: 1,
                    double: 1.0,
                    bytes: s.map(|s| s.as_bytes().to_vec()).map_err(|_| {
                        StdError::new(ErrorKind::Error, "Object could not be converted to string")
                    }),
                }
            }
        })
    }
}

fn pack_fn(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let format = a.str(0);
    let mut args = Vec::with_capacity(a.values.len().saturating_sub(1));
    for v in a.values.iter().skip(1) {
        args.push(VmPackArg::new(vm, v)?);
    }
    let mut n = Notices::new();
    let r = pack::pack(format.as_bytes(), &args, &mut n);
    emit(vm, n)?;
    Ok(bytes(check(vm, r)?))
}

fn unpack_fn(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut n = Notices::new();
    let r = pack::unpack(a.str(0).as_bytes(), a.str(1).as_bytes(), a.int(2), &mut n);
    emit(vm, n)?;
    let r = check(vm, r)?;
    if let Some(items) = &r {
        let size = items.iter().fold(0usize, |acc, (_, v)| {
            acc.saturating_add(match v {
                UnpackValue::Str(s) => mem::BUCKET + mem::zstr(s.len()),
                _ => mem::BUCKET,
            })
        });
        mem::check(vm, 0, size)?;
    }
    Ok(match r {
        None => Value::Bool(false),
        Some(items) => assoc(items.into_iter().map(|(k, v)| {
            let key = match k {
                UnpackKey::Int(i) => php_runtime::ArrayKey::Int(i),
                UnpackKey::Str(s) => php_runtime::ArrayKey::Str(s.into()),
            };
            let val = match v {
                UnpackValue::Int(i) => Value::Int(i),
                UnpackValue::Float(f) => Value::Float(f),
                UnpackValue::Str(s) => bytes(s),
            };
            (key, val)
        })),
    })
}

fn scan_value(v: ScanValue) -> Value {
    match v {
        ScanValue::Int(i) => Value::Int(i),
        ScanValue::Float(f) => Value::Float(f),
        ScanValue::Str(s) => bytes(s),
    }
}

fn sscanf(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let num_vars = a.values.len().saturating_sub(2);
    let r = scanf::sscanf(a.str(0).as_bytes(), a.str(1).as_bytes(), num_vars);
    Ok(match check(vm, r)? {
        SscanfResult::Array(None) => Value::Null,
        SscanfResult::Array(Some(vals)) => Value::Array(ZArray::from_values(
            vals.into_iter().map(|v| v.map_or(Value::Null, scan_value)),
        )),
        SscanfResult::Assigned { values, count } => {
            for (i, v) in values.into_iter().enumerate() {
                if let Some(v) = v {
                    a.set_ref(i + 2, scan_value(v));
                }
            }
            Value::Int(count)
        }
    })
}

fn convert_uuencode(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(uuencode::convert_uuencode(a.str(0).as_bytes())))
}

fn convert_uudecode(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut n = Notices::new();
    let r = uuencode::convert_uudecode(a.str(0).as_bytes(), &mut n);
    emit(vm, n)?;
    Ok(opt_bytes(r))
}

fn quoted_printable_decode(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(quoted_printable::quoted_printable_decode(
        a.str(0).as_bytes(),
    )))
}

fn quoted_printable_encode(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(quoted_printable::quoted_printable_encode(
        a.str(0).as_bytes(),
    )))
}

fn version_compare(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (v1, v2) = (a.str(0), a.str(1));
    match a.opt(2) {
        None => Ok(Value::Int(i64::from(versioning::version_compare(
            v1.as_bytes(),
            v2.as_bytes(),
        )))),
        Some(op) => {
            let op = to_str(vm, &op)?;
            let r = versioning::version_compare_op(v1.as_bytes(), v2.as_bytes(), op.as_bytes());
            Ok(Value::Bool(check(vm, r)?))
        }
    }
}

fn soundex(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(analysis::soundex(a.str(0).as_bytes())))
}

fn metaphone(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    deprecated_function(
        vm,
        "metaphone",
        "8.6",
        "use a userland phonetic matching library instead",
    )?;
    let r = analysis::metaphone(a.str(0).as_bytes(), a.int(1));
    Ok(bytes(check(vm, r)?))
}

fn levenshtein(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cost = |n: usize| if a.has(n) { a.int(n) } else { 1 };
    let (s1, s2) = (a.str(0), a.str(1));
    if !s1.is_empty() && !s2.is_empty() {
        // The first of the two workspace rows.
        let row = (s2.len() + 1).saturating_mul(8);
        mem::check(vm, mem::zstr(s1.len()) + mem::zstr(s2.len()), row)?;
    }
    let r = analysis::levenshtein(s1.as_bytes(), s2.as_bytes(), cost(2), cost(3), cost(4));
    Ok(Value::Int(r))
}

/// Registers the functions of this module.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function("pack(string $format, mixed ...$values): string", pack_fn)
        .function("unpack(string $format, string $string, int $offset = 0): array|false", unpack_fn)
        .function("sscanf(string $string, string $format, mixed &...$vars): array|int|null", sscanf)
        .function("convert_uuencode(string $string): string", convert_uuencode)
        .function("convert_uudecode(string $string): string|false", convert_uudecode)
        .function("quoted_printable_decode(string $string): string", quoted_printable_decode)
        .function("quoted_printable_encode(string $string): string", quoted_printable_encode)
        .function("version_compare(string $version1, string $version2, ?string $operator = null): int|bool", version_compare)
        .function("soundex(string $string): string", soundex)
        .function("metaphone(string $string, int $max_phonemes = 0): string", metaphone)
        .function("levenshtein(string $string1, string $string2, int $insertion_cost = 1, int $replacement_cost = 1, int $deletion_cost = 1): int", levenshtein);
}
