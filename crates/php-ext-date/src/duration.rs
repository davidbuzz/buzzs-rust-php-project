//! `Time\Duration` and `Time\TimeException` (PHP 8.6, `ext/date/php_time.c`
//! and `time_duration.c`).
//!
//! The arithmetic is a port of timelib's `duration.c`; it lives here until
//! `php-date` grows a duration module.

use std::cell::RefCell;

use php_date::parse::parse_iso_interval;
use php_runtime::{ArrayKey, Obj, Value, ZStr};
use php_vm::class::cflags;
use php_vm::ext::{
    acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, Throw, VmResult,
};
use php_vm::object;
use php_vm::Vm;

use crate::objects::{class, instantiate};

const NSECS_PER_SEC: u64 = 1_000_000_000;
/// `MAX_DURATION_SECONDS`.
const MAX_DURATION_SECONDS: u64 = 9_223_372_035;
const CLASS: &str = "Time\\Duration";
const EXCEPTION: &str = "Time\\TimeException";

/// timelib's duration error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurationError {
    SecondsOutOfRange,
    NanosecondsOutOfRange,
    DivisionByZero,
    Overflow,
    OnlyPeriodAllowed,
    MissingPeriod,
    DaysFound,
    ParseFailure,
}

impl DurationError {
    /// `timelib_get_error_message()`.
    fn message(self) -> &'static str {
        match self {
            Self::SecondsOutOfRange => "Seconds value out of range",
            Self::NanosecondsOutOfRange => "Nanoseconds value out of range",
            Self::DivisionByZero => "Division by zero",
            Self::Overflow => "Integer Overflow",
            Self::OnlyPeriodAllowed => {
                "The ISO 8601 duration string may only contain the period (P) aspect"
            }
            Self::MissingPeriod => "The ISO 8601 duration string is missing the period (P) aspect",
            Self::DaysFound => "The ISO 8601 duration string may only contain the time (T) aspect",
            Self::ParseFailure => "The ISO 8601 duration string could not be parsed",
        }
    }
}

/// `timelib_duration`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Duration {
    seconds: u64,
    nanoseconds: u64,
    negative: bool,
}

type DResult = Result<Duration, DurationError>;

impl Duration {
    /// `timelib_duration_ctor_static()`.
    fn new(seconds: u64, nanoseconds: u64, negative: bool) -> DResult {
        if seconds > MAX_DURATION_SECONDS {
            return Err(DurationError::SecondsOutOfRange);
        }
        if nanoseconds >= NSECS_PER_SEC {
            return Err(DurationError::NanosecondsOutOfRange);
        }
        Ok(Self {
            seconds,
            nanoseconds,
            negative: negative && (seconds != 0 || nanoseconds != 0),
        })
    }

    fn add_abs(a: &Self, b: &Self) -> DResult {
        let mut s = a.seconds.wrapping_add(b.seconds);
        let mut ns = a.nanoseconds + b.nanoseconds;
        if ns >= NSECS_PER_SEC {
            s = s.wrapping_add(1);
            ns -= NSECS_PER_SEC;
        }
        Self::new(s, ns, a.negative)
    }

    fn sub_abs(a: &Self, b: &Self) -> DResult {
        let mut s = a.seconds.wrapping_sub(b.seconds);
        let mut ns = a.nanoseconds.cast_signed_compat() - b.nanoseconds.cast_signed_compat();
        if ns < 0 {
            s = s.wrapping_sub(1);
            ns += NSECS_PER_SEC.cast_signed_compat();
        }
        Self::new(s, ns.cast_unsigned_compat(), a.negative)
    }

    /// `timelib_duration_abs_compare()`.
    fn abs_compare(a: &Self, b: &Self) -> i32 {
        match (a.seconds, a.nanoseconds).cmp(&(b.seconds, b.nanoseconds)) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    }

    /// `timelib_duration_compare()`.
    fn compare(a: &Self, b: &Self) -> i32 {
        match (a.negative, b.negative) {
            (true, false) => -1,
            (false, true) => 1,
            (true, true) => Self::abs_compare(b, a),
            (false, false) => Self::abs_compare(a, b),
        }
    }

    fn add(a: &Self, b: &Self) -> DResult {
        if a.negative == b.negative {
            return Self::add_abs(a, b);
        }
        match Self::abs_compare(a, b) {
            -1 => Self::sub_abs(b, a),
            0 => Ok(Self::default()),
            _ => Self::sub_abs(a, b),
        }
    }

    fn sub(a: &Self, b: &Self) -> DResult {
        if a.negative != b.negative {
            return Self::add_abs(a, b);
        }
        match Self::abs_compare(a, b) {
            -1 => Self::sub_abs(b, a)?.negate(),
            0 => Ok(Self::default()),
            _ => Self::sub_abs(a, b),
        }
    }

    fn negate(&self) -> DResult {
        Self::new(self.seconds, self.nanoseconds, !self.negative)
    }

    fn mul(&self, factor: u64) -> DResult {
        if factor == 0 {
            return Ok(Self::default());
        }
        if self.seconds > u64::MAX / factor {
            return Err(DurationError::Overflow);
        }
        let seconds = self.seconds * factor;
        let mut extra = self.nanoseconds * (factor / NSECS_PER_SEC);
        let mut ns = self.nanoseconds * (factor % NSECS_PER_SEC);
        extra += ns / NSECS_PER_SEC;
        ns %= NSECS_PER_SEC;
        if u64::MAX - extra < seconds {
            return Err(DurationError::Overflow);
        }
        Self::new(seconds + extra, ns, self.negative)
    }

    fn div(&self, divisor: u64) -> DResult {
        if divisor < 1 {
            return Err(DurationError::DivisionByZero);
        }
        let seconds = self.seconds / divisor;
        let ns = (self.nanoseconds + (self.seconds % divisor) * NSECS_PER_SEC) / divisor;
        Self::new(seconds, ns, self.negative)
    }

    /// `timelib_duration_create_from_iso8601string()`.
    fn from_iso(s: &[u8]) -> DResult {
        let r = parse_iso_interval(s);
        if !r.errors.errors.is_empty() || !r.errors.warnings.is_empty() {
            return Err(DurationError::ParseFailure);
        }
        let Some(p) = r.period else {
            return Err(DurationError::MissingPeriod);
        };
        if r.begin.is_some() || r.end.is_some() || r.recurrences.is_some() {
            return Err(DurationError::OnlyPeriodAllowed);
        }
        if p.y > 0 || p.m > 0 || p.d > 0 {
            return Err(DurationError::DaysFound);
        }
        let secs =
            p.h.wrapping_mul(3600)
                .wrapping_add(p.i.wrapping_mul(60))
                .wrapping_add(p.s);
        Self::new(secs.cast_unsigned_compat(), 0, false)
    }
}

thread_local! {
    /// `DATEG(duration_cache)`: the last non-negative duration created by
    /// `php_date_time_duration_create()`.
    static CACHE: RefCell<Option<(Duration, Obj)>> = const { RefCell::new(None) };
}

/// Drops the cached object (`RSHUTDOWN`).
pub(crate) fn reset_cache() {
    CACHE.with(|c| *c.borrow_mut() = None);
}

fn out_of_range(vm: &mut Vm) -> Throw {
    vm.throw(
        EXCEPTION,
        "The maximum representable range is 9_223_372_035 seconds (roughly 292 years)",
    )
}

/// `throw_timelib_error()`.
fn timelib_error(vm: &mut Vm, e: DurationError) -> Throw {
    match e {
        DurationError::SecondsOutOfRange => out_of_range(vm),
        DurationError::DivisionByZero => vm.throw("DivisionByZeroError", "Division by zero"),
        DurationError::ParseFailure
        | DurationError::MissingPeriod
        | DurationError::OnlyPeriodAllowed
        | DurationError::DaysFound => vm.throw(EXCEPTION, e.message()),
        DurationError::NanosecondsOutOfRange | DurationError::Overflow => vm.throw(
            EXCEPTION,
            &format!("Failed to create a Time\\Duration: {}", e.message()),
        ),
    }
}

fn duration_of(o: &Obj) -> Duration {
    object::internal::<Duration, _>(o, |d| *d).unwrap_or_default()
}

/// Writes a declared (readonly) property slot directly.
fn write_slot(o: &Obj, name: &str, v: Value) {
    let d = object::data(o);
    if let Some(pi) = d.class.prop_map.get(&ZStr::from(name)) {
        let slot = pi.slot as usize;
        if let Some(s) = d.slots.borrow_mut().get_mut(slot) {
            *s = Some(v);
        }
        if let Some(f) = d.readonly_init.borrow_mut().get_mut(slot) {
            *f = true;
        }
    }
}

fn slot_initialized(o: &Obj, name: &str) -> bool {
    let d = object::data(o);
    d.class
        .prop_map
        .get(&ZStr::from(name))
        .is_some_and(|pi| matches!(d.slots.borrow().get(pi.slot as usize), Some(Some(_))))
}

/// `sync_properties()` on a fresh object.
fn make(vm: &mut Vm, r: DResult) -> VmResult<Value> {
    let d = r.map_err(|e| timelib_error(vm, e))?;
    if d.seconds > i64::MAX.cast_unsigned_compat() || d.seconds > MAX_DURATION_SECONDS {
        return Err(out_of_range(vm));
    }
    let cls = class(vm, CLASS);
    let o = instantiate(vm, &cls)?;
    object::set_internal(&o, d);
    write_slot(&o, "seconds", Value::Int(d.seconds.cast_signed_compat()));
    write_slot(
        &o,
        "nanoseconds",
        Value::Int(d.nanoseconds.cast_signed_compat()),
    );
    write_slot(&o, "negative", Value::Bool(d.negative));
    Ok(Value::Object(o))
}

/// `php_date_time_duration_create()` (non-negative, with the cache).
fn create(vm: &mut Vm, seconds: u64, nanoseconds: u64) -> VmResult<Value> {
    if let Some(o) = CACHE.with(|c| {
        c.borrow().as_ref().and_then(|(d, o)| {
            (d.seconds == seconds && d.nanoseconds == nanoseconds).then(|| o.clone())
        })
    }) {
        return Ok(Value::Object(o));
    }
    let v = make(vm, Duration::new(seconds, nanoseconds, false))?;
    if let Value::Object(o) = &v {
        let d = duration_of(o);
        CACHE.with(|c| *c.borrow_mut() = Some((d, o.clone())));
    }
    Ok(v)
}

fn method_value_error(vm: &mut Vm, method: &str, n: usize, pname: &str, msg: &str) -> Throw {
    vm.throw(
        "ValueError",
        &format!("{CLASS}::{method}(): Argument #{n} (${pname}) {msg}"),
    )
}

/// `Z_PARAM_ULONG`.
fn ulong(vm: &mut Vm, a: &Args, n: usize, method: &str, pname: &str) -> VmResult<u64> {
    let v = a.int(n);
    if v < 0 {
        return Err(method_value_error(
            vm,
            method,
            n + 1,
            pname,
            "must be greater than or equal to 0",
        ));
    }
    Ok(v.cast_unsigned_compat())
}

fn construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw(
        "Error",
        "Cannot directly construct Time\\Duration, use Time\\Duration::from*() methods instead",
    ))
}

/// The `Invalid serialization data` exception, chaining the problem found
/// by `object_properties_load()` as its previous exception.
fn invalid_data(vm: &mut Vm, prev: Option<(&str, String)>) -> Throw {
    const MSG: &str = "Invalid serialization data for Time\\Duration object";
    let Some((cls, msg)) = prev else {
        return vm.throw("Exception", MSG);
    };
    let Throw::Exception(p) = vm.throw(cls, &msg) else {
        return vm.throw("Exception", MSG);
    };
    let ecls = class(vm, "Exception");
    match vm.new_object_with_args(
        &ecls,
        vec![Value::from(MSG), Value::Int(0), Value::Object(p)],
    ) {
        Ok(o) => Throw::Exception(o),
        Err(t) => t,
    }
}

/// Validates an `__unserialize()` payload like `object_properties_load()`
/// followed by the range checks of `Time\Duration::__unserialize()`.
fn check_payload(
    o: &Obj,
    h: &php_runtime::ZArray,
) -> Result<Duration, Option<(&'static str, String)>> {
    let names = ["seconds", "nanoseconds", "negative"];
    for (k, v) in h.iter() {
        let ArrayKey::Str(name) = k else {
            return Err(None);
        };
        let n = name.to_string_lossy();
        let Some(field) = names.iter().find(|f| **f == n) else {
            return Err(Some((
                "Error",
                format!("Cannot create dynamic property {CLASS}::${n}"),
            )));
        };
        if slot_initialized(o, field) {
            return Err(Some((
                "Error",
                format!("Cannot modify readonly property {CLASS}::${field}"),
            )));
        }
        if matches!(v, Value::Reference(_)) {
            return Err(Some((
                "Error",
                format!("Cannot indirectly modify readonly property {CLASS}::${field}"),
            )));
        }
        let ok = match *field {
            "negative" => matches!(v, Value::Bool(_)),
            _ => matches!(v, Value::Int(_)),
        };
        if !ok {
            let ty = if *field == "negative" { "bool" } else { "int" };
            return Err(Some((
                "TypeError",
                format!(
                    "Cannot assign {} to property {CLASS}::${field} of type {ty}",
                    type_name(v)
                ),
            )));
        }
    }
    let get = |n: &str| h.get(&ArrayKey::Str(ZStr::from(n))).map(Value::deref);
    let (Some(Value::Int(s)), Some(Value::Int(ns)), Some(Value::Bool(neg))) =
        (get("seconds"), get("nanoseconds"), get("negative"))
    else {
        return Err(None);
    };
    if s < 0 || ns < 0 || ns >= NSECS_PER_SEC.cast_signed_compat() {
        return Err(None);
    }
    let d = match Duration::new(s.cast_unsigned_compat(), ns.cast_unsigned_compat(), neg) {
        Ok(d) => d,
        Err(DurationError::SecondsOutOfRange) => {
            return Err(Some((
                EXCEPTION,
                "The maximum representable range is 9_223_372_035 seconds (roughly 292 years)"
                    .to_string(),
            )))
        }
        Err(_) => return Err(None),
    };
    if d.negative != neg {
        return Err(None);
    }
    Ok(d)
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "string",
        Value::Array(_) => "array",
        _ => "object",
    }
}

fn unserialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let h = a.array(0);
    let d = check_payload(&o, &h).map_err(|prev| invalid_data(vm, prev))?;
    object::set_internal(&o, d);
    write_slot(&o, "seconds", Value::Int(d.seconds.cast_signed_compat()));
    write_slot(
        &o,
        "nanoseconds",
        Value::Int(d.nanoseconds.cast_signed_compat()),
    );
    write_slot(&o, "negative", Value::Bool(d.negative));
    Ok(Value::Null)
}

fn from_seconds(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = ulong(vm, a, 0, "fromSeconds", "seconds")?;
    let ns = if a.has(1) {
        ulong(vm, a, 1, "fromSeconds", "nanoseconds")?
    } else {
        0
    };
    if ns >= NSECS_PER_SEC {
        return Err(method_value_error(
            vm,
            "fromSeconds",
            2,
            "nanoseconds",
            "must be less than 1_000_000_000",
        ));
    }
    create(vm, s, ns)
}

fn from_nanoseconds(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = ulong(vm, a, 0, "fromNanoseconds", "nanoseconds")?;
    create(vm, n / NSECS_PER_SEC, n % NSECS_PER_SEC)
}

fn from_microseconds(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = ulong(vm, a, 0, "fromMicroseconds", "microseconds")?;
    create(vm, n / 1_000_000, (n % 1_000_000) * 1000)
}

fn from_milliseconds(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = ulong(vm, a, 0, "fromMilliseconds", "milliseconds")?;
    create(vm, n / 1000, (n % 1000) * 1_000_000)
}

fn from_minutes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = ulong(vm, a, 0, "fromMinutes", "minutes")?;
    if n > u64::MAX / 60 {
        return Err(out_of_range(vm));
    }
    create(vm, n * 60, 0)
}

fn from_hours(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = ulong(vm, a, 0, "fromHours", "hours")?;
    if n > u64::MAX / 3600 {
        return Err(out_of_range(vm));
    }
    create(vm, n * 3600, 0)
}

fn from_iso(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    if s.as_bytes().contains(&0) {
        return Err(timelib_error(vm, DurationError::ParseFailure));
    }
    make(vm, Duration::from_iso(s.as_bytes()))
}

fn negate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let d = duration_of(&a.this_obj());
    make(vm, d.negate())
}

fn absolute(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let d = duration_of(&o);
    if !d.negative {
        return Ok(Value::Object(o));
    }
    create(vm, d.seconds, d.nanoseconds)
}

fn other(a: &Args, n: usize) -> Duration {
    match a.at(n) {
        Value::Object(o) => duration_of(&o),
        _ => Duration::default(),
    }
}

fn add(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let d = duration_of(&a.this_obj());
    make(vm, Duration::add(&d, &other(a, 0)))
}

fn sub(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let d = duration_of(&a.this_obj());
    make(vm, Duration::sub(&d, &other(a, 0)))
}

fn multiply_by(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let f = ulong(vm, a, 0, "multiplyBy", "factor")?;
    let d = duration_of(&a.this_obj());
    make(vm, d.mul(f))
}

fn divide_by(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let f = ulong(vm, a, 0, "divideBy", "divisor")?;
    let d = duration_of(&a.this_obj());
    make(vm, d.div(f))
}

#[allow(clippy::unnecessary_wraps)]
fn compare_m(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(i64::from(Duration::compare(
        &other(a, 0),
        &other(a, 1),
    ))))
}

fn create_hook(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    object::set_internal(o, Duration::default());
    Ok(())
}

fn clone_hook(_vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    object::set_internal(new, duration_of(old));
    Ok(())
}

fn compare(_vm: &mut Vm, a: &Value, b: &Value) -> VmResult<Option<i32>> {
    let (Value::Object(x), Value::Object(y)) = (a, b) else {
        return Ok(None);
    };
    if !object::has_internal::<Duration>(x) || !object::has_internal::<Duration>(y) {
        return Ok(None);
    }
    Ok(Some(Duration::compare(&duration_of(x), &duration_of(y))))
}

/// Registers `Time\TimeException` and `Time\Duration`.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let vm = m.vm();
    ClassBuilder::new(EXCEPTION)
        .extends(vm, "Exception")
        .flags(cflags::NO_DYNAMIC_PROPERTIES)
        .register(vm);
    let p = acc::PUBLIC;
    let ps = acc::PUBLIC | acc::STATIC;
    let ro = acc::PUBLIC | acc::READONLY;
    ClassBuilder::new(CLASS)
        .flags(class_flags::FINAL | cflags::READONLY | cflags::NO_DYNAMIC_PROPERTIES)
        .prop("seconds", ro, Some("int"), None)
        .prop("nanoseconds", ro, Some("int"), None)
        .prop("negative", ro, Some("bool"), None)
        .method("__construct()", acc::PRIVATE, construct)
        .method("__unserialize(array $data): void", p, unserialize)
        .method(
            "fromSeconds(int $seconds, int $nanoseconds = 0): Time\\Duration",
            ps,
            from_seconds,
        )
        .method(
            "fromNanoseconds(int $nanoseconds): Time\\Duration",
            ps,
            from_nanoseconds,
        )
        .method(
            "fromMicroseconds(int $microseconds): Time\\Duration",
            ps,
            from_microseconds,
        )
        .method(
            "fromMilliseconds(int $milliseconds): Time\\Duration",
            ps,
            from_milliseconds,
        )
        .method(
            "fromMinutes(int $minutes): Time\\Duration",
            ps,
            from_minutes,
        )
        .method("fromHours(int $hours): Time\\Duration", ps, from_hours)
        .method(
            "fromIso8601DurationString(string $specification): Time\\Duration",
            ps,
            from_iso,
        )
        .method("negate(): Time\\Duration", p, negate)
        .method("absolute(): Time\\Duration", p, absolute)
        .method("add(Time\\Duration $duration): Time\\Duration", p, add)
        .method("sub(Time\\Duration $duration): Time\\Duration", p, sub)
        .method("multiplyBy(int $factor): Time\\Duration", p, multiply_by)
        .method("divideBy(int $divisor): Time\\Duration", p, divide_by)
        .method(
            "compare(Time\\Duration $a, Time\\Duration $b): int",
            ps,
            compare_m,
        )
        .hooks(ClassHooks {
            create: Some(create_hook),
            clone: Some(clone_hook),
            compare: Some(compare),
            ..ClassHooks::default()
        })
        .register(vm);
}

/// `u64`/`i64` reinterpretation (`cast_signed`/`cast_unsigned` need Rust
/// 1.87, above the workspace MSRV).
trait CastCompat {
    type Out;
    fn cast_signed_compat(self) -> Self::Out;
}

impl CastCompat for u64 {
    type Out = i64;
    #[allow(clippy::cast_possible_wrap)]
    fn cast_signed_compat(self) -> i64 {
        self as i64
    }
}

trait CastUnsignedCompat {
    fn cast_unsigned_compat(self) -> u64;
}

impl CastUnsignedCompat for i64 {
    #[allow(clippy::cast_sign_loss)]
    fn cast_unsigned_compat(self) -> u64 {
        self as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: u64, ns: u64, neg: bool) -> Duration {
        Duration::new(s, ns, neg).unwrap()
    }

    #[test]
    fn arithmetic() {
        assert_eq!(
            Duration::add(&d(1, 5, false), &d(0, 999_999_999, false)),
            Ok(d(2, 4, false))
        );
        assert_eq!(
            Duration::add(&d(1, 0, false), &d(2, 0, true)),
            Ok(d(1, 0, true))
        );
        assert_eq!(
            Duration::sub(&d(1, 0, false), &d(1, 0, false)),
            Ok(Duration::default())
        );
        assert_eq!(
            Duration::sub(&d(1, 0, false), &d(2, 1, false)),
            Ok(d(1, 1, true))
        );
        assert_eq!(
            d(1, 500_000_000, false).mul(3),
            Ok(d(4, 500_000_000, false))
        );
        assert_eq!(d(3, 0, false).div(2), Ok(d(1, 500_000_000, false)));
        assert_eq!(d(3, 0, false).div(0), Err(DurationError::DivisionByZero));
        assert_eq!(Duration::new(0, 0, true).map(|x| x.negative), Ok(false));
    }

    #[test]
    fn iso() {
        assert_eq!(Duration::from_iso(b"PT1H1M1S"), Ok(d(3661, 0, false)));
        assert_eq!(Duration::from_iso(b"P1D"), Err(DurationError::DaysFound));
        assert_eq!(Duration::from_iso(b""), Err(DurationError::ParseFailure));
    }

    #[test]
    fn ordering() {
        assert_eq!(Duration::compare(&d(1, 0, true), &d(0, 1, false)), -1);
        assert_eq!(Duration::compare(&d(2, 0, true), &d(1, 0, true)), -1);
        assert_eq!(Duration::compare(&d(2, 0, false), &d(2, 0, false)), 0);
    }
}
