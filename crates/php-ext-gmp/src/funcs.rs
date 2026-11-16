// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The procedural `gmp_*()` functions.

use std::cell::RefCell;

use php_gmp::php::{self as gmp, GmpRandom, StrOrInt};
use php_gmp::{GmpError, Mpz};
use php_runtime::{Value, ZArray, ZStr};
use php_vm::ext::{Args, ModuleBuilder, NativeFn, VmResult};
use php_vm::Vm;

use crate::{check_bits, is_gmp, new_gmp, num_arg, throw, GmpState};

thread_local! {
    /// `GMPG(rand_state)`: per-request Mersenne Twister state.
    static RANDOM: RefCell<GmpRandom> = RefCell::new(GmpRandom::new());
}

/// Forgets the random state (`RSHUTDOWN`).
pub(crate) fn reset_random(_vm: &mut Vm) {
    RANDOM.with(|r| r.borrow_mut().reset());
}

/// Seed material for an unseeded generator (`gmp_init_random`).
fn entropy() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    if let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        h.write_u128(d.as_nanos());
    }
    h.finish()
}

fn lift<T>(vm: &mut Vm, func: &str, r: Result<T, GmpError>) -> VmResult<T> {
    r.map_err(|e| throw(vm, func, &e))
}

fn gmp_result(vm: &mut Vm, func: &str, r: Result<Mpz, GmpError>) -> VmResult<Value> {
    let m = lift(vm, func, r)?;
    new_gmp(vm, m)
}

fn pair(vm: &mut Vm, func: &str, r: Result<(Mpz, Mpz), GmpError>) -> VmResult<Value> {
    let (x, y) = lift(vm, func, r)?;
    let x = new_gmp(vm, x)?;
    let y = new_gmp(vm, y)?;
    Ok(Value::Array(ZArray::from_values([x, y])))
}

/// An `int` argument with a stub default.
fn int_or(a: &Args, n: usize, default: i64) -> i64 {
    if a.has(n) {
        a.int(n)
    } else {
        default
    }
}

/// `int|string $num` of `gmp_init()` / `GMP::__construct()`.
pub(crate) fn init_number(vm: &mut Vm, a: &Args, func: &str) -> VmResult<Mpz> {
    let base = int_or(a, 1, 0);
    let v = if a.has(0) { a.at(0) } else { Value::Int(0) };
    let r = match &v {
        Value::Str(s) => gmp::gmp_init(StrOrInt::Str(s.as_bytes()), base),
        other => {
            let i = vm.value_to_int(other)?;
            gmp::gmp_init(StrOrInt::Int(i), base)
        }
    };
    lift(vm, func, r)
}

fn gmp_init(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = init_number(vm, a, "gmp_init")?;
    new_gmp(vm, m)
}

fn gmp_import(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = gmp::gmp_import(
        a.str(0).as_bytes(),
        int_or(a, 1, 1),
        int_or(a, 2, gmp::DEFAULT_IMPORT_EXPORT_FLAGS),
    );
    gmp_result(vm, "gmp_import", r)
}

fn gmp_export(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = num_arg(vm, a, 0, "gmp_export")?;
    let r = gmp::gmp_export(
        &n,
        int_or(a, 1, 1),
        int_or(a, 2, gmp::DEFAULT_IMPORT_EXPORT_FLAGS),
    );
    let bytes = lift(vm, "gmp_export", r)?;
    Ok(Value::Str(ZStr::from(bytes.as_slice())))
}

fn gmp_strval(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = num_arg(vm, a, 0, "gmp_strval")?;
    let r = gmp::gmp_strval(&n, int_or(a, 1, 10));
    lift(vm, "gmp_strval", r).map(Value::from)
}

macro_rules! unary {
    ($($name:ident => |$x:ident| $body:expr;)*) => {
        $(fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let func = stringify!($name);
            let $x = num_arg(vm, a, 0, func)?;
            let r: Result<Mpz, GmpError> = $body;
            gmp_result(vm, func, r)
        })*
    };
}

unary! {
    gmp_neg => |x| Ok(gmp::gmp_neg(&x));
    gmp_abs => |x| Ok(gmp::gmp_abs(&x));
    gmp_com => |x| Ok(gmp::gmp_com(&x));
    gmp_nextprime => |x| Ok(gmp::gmp_nextprime(&x));
    gmp_sqrt => |x| gmp::gmp_sqrt(&x);
}

macro_rules! binary {
    ($($name:ident => |$x:ident, $y:ident| $body:expr;)*) => {
        $(fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let func = stringify!($name);
            let $x = num_arg(vm, a, 0, func)?;
            let $y = num_arg(vm, a, 1, func)?;
            let r: Result<Mpz, GmpError> = $body;
            gmp_result(vm, func, r)
        })*
    };
}

binary! {
    gmp_add => |x, y| Ok(gmp::gmp_add(&x, &y));
    gmp_sub => |x, y| Ok(gmp::gmp_sub(&x, &y));
    gmp_mul => |x, y| Ok(gmp::gmp_mul(&x, &y));
    gmp_gcd => |x, y| Ok(gmp::gmp_gcd(&x, &y));
    gmp_lcm => |x, y| Ok(gmp::gmp_lcm(&x, &y));
    gmp_and => |x, y| Ok(gmp::gmp_and(&x, &y));
    gmp_or => |x, y| Ok(gmp::gmp_or(&x, &y));
    gmp_xor => |x, y| Ok(gmp::gmp_xor(&x, &y));
    gmp_mod => |x, y| gmp::gmp_mod(&x, &y);
    gmp_divexact => |x, y| gmp::gmp_divexact(&x, &y);
}

macro_rules! to_int {
    ($($name:ident => |$x:ident, $y:ident| $body:expr;)*) => {
        $(fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let func = stringify!($name);
            let $x = num_arg(vm, a, 0, func)?;
            let $y = num_arg(vm, a, 1, func)?;
            Ok(Value::Int($body))
        })*
    };
}

to_int! {
    gmp_jacobi => |x, y| gmp::gmp_jacobi(&x, &y);
    gmp_legendre => |x, y| gmp::gmp_legendre(&x, &y);
    gmp_kronecker => |x, y| gmp::gmp_kronecker(&x, &y);
    gmp_cmp => |x, y| gmp::gmp_cmp(&x, &y);
    gmp_hamdist => |x, y| gmp::gmp_hamdist(&x, &y);
}

macro_rules! unary_scalar {
    ($($name:ident => |$x:ident| $body:expr;)*) => {
        $(fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let $x = num_arg(vm, a, 0, stringify!($name))?;
            Ok(Value::from($body))
        })*
    };
}

unary_scalar! {
    gmp_intval => |x| gmp::gmp_intval(&x);
    gmp_sign => |x| gmp::gmp_sign(&x);
    gmp_popcount => |x| gmp::gmp_popcount(&x);
    gmp_perfect_square => |x| gmp::gmp_perfect_square(&x);
    gmp_perfect_power => |x| gmp::gmp_perfect_power(&x);
}

fn gmp_fact(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let x = num_arg(vm, a, 0, "gmp_fact")?;
    // n! has about n·log2(n) bits.
    if let Some(n) = x.to_u64() {
        let n = u128::from(n);
        check_bits(vm, n * (128 - u128::from(n.leading_zeros())))?;
    }
    gmp_result(vm, "gmp_fact", gmp::gmp_fact(&x))
}

/// Estimated size in bits of `x ** e`.
pub(crate) fn pow_bits(x: &Mpz, e: u64) -> u128 {
    let bits = u128::from(x.bit_len());
    if bits <= 1 {
        1
    } else {
        (bits - 1).saturating_mul(u128::from(e)) + 1
    }
}

fn div_with(
    vm: &mut Vm,
    a: &Args,
    func: &str,
    f: fn(&Mpz, &Mpz, i64) -> Result<Mpz, GmpError>,
) -> VmResult<Value> {
    let x = num_arg(vm, a, 0, func)?;
    let y = num_arg(vm, a, 1, func)?;
    let r = f(&x, &y, int_or(a, 2, gmp::GMP_ROUND_ZERO));
    gmp_result(vm, func, r)
}

fn gmp_div_q(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    div_with(vm, a, "gmp_div_q", gmp::gmp_div_q)
}

fn gmp_div(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    div_with(vm, a, "gmp_div", gmp::gmp_div_q)
}

fn gmp_div_r(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    div_with(vm, a, "gmp_div_r", gmp::gmp_div_r)
}

fn gmp_div_qr(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let func = "gmp_div_qr";
    let x = num_arg(vm, a, 0, func)?;
    let y = num_arg(vm, a, 1, func)?;
    let r = gmp::gmp_div_qr(&x, &y, int_or(a, 2, gmp::GMP_ROUND_ZERO));
    pair(vm, func, r)
}

fn gmp_sqrtrem(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let x = num_arg(vm, a, 0, "gmp_sqrtrem")?;
    pair(vm, "gmp_sqrtrem", gmp::gmp_sqrtrem(&x))
}

fn gmp_root(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let x = num_arg(vm, a, 0, "gmp_root")?;
    gmp_result(vm, "gmp_root", gmp::gmp_root(&x, a.int(1)))
}

fn gmp_rootrem(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let x = num_arg(vm, a, 0, "gmp_rootrem")?;
    pair(vm, "gmp_rootrem", gmp::gmp_rootrem(&x, a.int(1)))
}

fn gmp_pow(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let x = num_arg(vm, a, 0, "gmp_pow")?;
    if let Ok(e) = u64::try_from(a.int(1)) {
        check_bits(vm, pow_bits(&x, e))?;
    }
    gmp_result(vm, "gmp_pow", gmp::gmp_pow(&x, a.int(1)))
}

fn gmp_binomial(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let x = num_arg(vm, a, 0, "gmp_binomial")?;
    if let Ok(k) = u64::try_from(a.int(1)) {
        // C(n, k) < n^k.
        let k = u128::from(k.min(x.to_u64().map_or(k, |n| n.saturating_sub(k)).max(1)));
        check_bits(vm, u128::from(x.bit_len()).saturating_mul(k))?;
    }
    gmp_result(vm, "gmp_binomial", gmp::gmp_binomial(&x, a.int(1)))
}

fn powm_with(
    vm: &mut Vm,
    a: &Args,
    func: &str,
    f: fn(&Mpz, &Mpz, &Mpz) -> Result<Mpz, GmpError>,
) -> VmResult<Value> {
    let x = num_arg(vm, a, 0, func)?;
    let e = num_arg(vm, a, 1, func)?;
    let m = num_arg(vm, a, 2, func)?;
    gmp_result(vm, func, f(&x, &e, &m))
}

fn gmp_powm(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    powm_with(vm, a, "gmp_powm", gmp::gmp_powm)
}

fn gmp_powm_sec(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    powm_with(vm, a, "gmp_powm_sec", gmp::gmp_powm_sec)
}

fn gmp_prob_prime(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let x = num_arg(vm, a, 0, "gmp_prob_prime")?;
    Ok(Value::Int(gmp::gmp_prob_prime(&x, int_or(a, 1, 10))))
}

fn gmp_gcdext(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let x = num_arg(vm, a, 0, "gmp_gcdext")?;
    let y = num_arg(vm, a, 1, "gmp_gcdext")?;
    let (g, s, t) = gmp::gmp_gcdext(&x, &y);
    let mut arr = ZArray::new();
    for (k, v) in [("g", g), ("s", s), ("t", t)] {
        let v = new_gmp(vm, v)?;
        arr.make_mut().insert(k, v);
    }
    Ok(Value::Array(arr))
}

fn gmp_invert(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let x = num_arg(vm, a, 0, "gmp_invert")?;
    let y = num_arg(vm, a, 1, "gmp_invert")?;
    match lift(vm, "gmp_invert", gmp::gmp_invert(&x, &y))? {
        Some(m) => new_gmp(vm, m),
        None => Ok(Value::Bool(false)),
    }
}

fn gmp_prevprime(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let x = num_arg(vm, a, 0, "gmp_prevprime")?;
    let (p, definitely) = lift(vm, "gmp_prevprime", gmp::gmp_prevprime(&x))?;
    if a.has(1) {
        a.set_ref(1, Value::Bool(definitely));
    }
    new_gmp(vm, p)
}

fn gmp_random_seed(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let seed = num_arg(vm, a, 0, "gmp_random_seed")?;
    RANDOM.with(|r| r.borrow_mut().seed(&seed, entropy));
    Ok(Value::Null)
}

fn gmp_random_bits(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let bits = a.int(0);
    if let Ok(b) = u32::try_from(bits) {
        check_bits(vm, u128::from(b))?;
    }
    let r = RANDOM.with(|r| r.borrow_mut().bits(bits, entropy));
    gmp_result(vm, "gmp_random_bits", r)
}

fn gmp_random_range(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let min = num_arg(vm, a, 0, "gmp_random_range")?;
    let max = num_arg(vm, a, 1, "gmp_random_range")?;
    let r = RANDOM.with(|r| r.borrow_mut().range(&min, &max, entropy));
    gmp_result(vm, "gmp_random_range", r)
}

/// Applies `f` to the number inside the `GMP $num` argument in place.
fn mutate(
    vm: &mut Vm,
    a: &Args,
    func: &str,
    f: impl FnOnce(&mut Mpz) -> Result<(), GmpError>,
) -> VmResult<Value> {
    let Value::Object(o) = a.at(0) else {
        return Ok(Value::Null);
    };
    if !is_gmp(&o) {
        return Ok(Value::Null);
    }
    let r = php_vm::object::internal::<GmpState, _>(&o, |s| f(&mut s.0)).unwrap_or(Ok(()));
    lift(vm, func, r)?;
    Ok(Value::Null)
}

fn gmp_setbit(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let index = a.int(1);
    if let Ok(i) = u64::try_from(index) {
        if i / 64 < u64::from(u32::MAX >> 1) {
            check_bits(vm, u128::from(i) + 1)?;
        }
    }
    let value = !a.has(2) || a.bool(2);
    mutate(vm, a, "gmp_setbit", |m| gmp::gmp_setbit(m, index, value))
}

fn gmp_clrbit(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let index = a.int(1);
    mutate(vm, a, "gmp_clrbit", |m| gmp::gmp_clrbit(m, index))
}

fn gmp_testbit(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let x = num_arg(vm, a, 0, "gmp_testbit")?;
    lift(vm, "gmp_testbit", gmp::gmp_testbit(&x, a.int(1))).map(Value::Bool)
}

fn gmp_scan0(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let x = num_arg(vm, a, 0, "gmp_scan0")?;
    lift(vm, "gmp_scan0", gmp::gmp_scan0(&x, a.int(1))).map(Value::Int)
}

fn gmp_scan1(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let x = num_arg(vm, a, 0, "gmp_scan1")?;
    lift(vm, "gmp_scan1", gmp::gmp_scan1(&x, a.int(1))).map(Value::Int)
}

/// Every function: stub signature and handler (each run in an uncounted
/// allocation scope, see `uncounted!`).
const FUNCTIONS: &[(&str, NativeFn)] = &[
    (
        "gmp_init(int|string $num, int $base = 0): GMP",
        uncounted!(gmp_init),
    ),
    (
        "gmp_import(string $data, int $word_size = 1, int $flags = GMP_MSW_FIRST | GMP_NATIVE_ENDIAN): GMP",
        uncounted!(gmp_import),
    ),
    (
        "gmp_export(mixed $num, int $word_size = 1, int $flags = GMP_MSW_FIRST | GMP_NATIVE_ENDIAN): string",
        uncounted!(gmp_export),
    ),
    (
        "gmp_intval(mixed $num): int",
        uncounted!(gmp_intval),
    ),
    (
        "gmp_strval(mixed $num, int $base = 10): string",
        uncounted!(gmp_strval),
    ),
    (
        "gmp_add(mixed $num1, mixed $num2): GMP",
        uncounted!(gmp_add),
    ),
    (
        "gmp_sub(mixed $num1, mixed $num2): GMP",
        uncounted!(gmp_sub),
    ),
    (
        "gmp_mul(mixed $num1, mixed $num2): GMP",
        uncounted!(gmp_mul),
    ),
    (
        "gmp_div_qr(mixed $num1, mixed $num2, int $rounding_mode = GMP_ROUND_ZERO): array",
        uncounted!(gmp_div_qr),
    ),
    (
        "gmp_div_q(mixed $num1, mixed $num2, int $rounding_mode = GMP_ROUND_ZERO): GMP",
        uncounted!(gmp_div_q),
    ),
    (
        "gmp_div_r(mixed $num1, mixed $num2, int $rounding_mode = GMP_ROUND_ZERO): GMP",
        uncounted!(gmp_div_r),
    ),
    (
        "gmp_div(mixed $num1, mixed $num2, int $rounding_mode = GMP_ROUND_ZERO): GMP",
        uncounted!(gmp_div),
    ),
    (
        "gmp_mod(mixed $num1, mixed $num2): GMP",
        uncounted!(gmp_mod),
    ),
    (
        "gmp_divexact(mixed $num1, mixed $num2): GMP",
        uncounted!(gmp_divexact),
    ),
    (
        "gmp_neg(mixed $num): GMP",
        uncounted!(gmp_neg),
    ),
    (
        "gmp_abs(mixed $num): GMP",
        uncounted!(gmp_abs),
    ),
    (
        "gmp_fact(mixed $num): GMP",
        uncounted!(gmp_fact),
    ),
    (
        "gmp_sqrt(mixed $num): GMP",
        uncounted!(gmp_sqrt),
    ),
    (
        "gmp_sqrtrem(mixed $num): array",
        uncounted!(gmp_sqrtrem),
    ),
    (
        "gmp_root(mixed $num, int $nth): GMP",
        uncounted!(gmp_root),
    ),
    (
        "gmp_rootrem(mixed $num, int $nth): array",
        uncounted!(gmp_rootrem),
    ),
    (
        "gmp_pow(mixed $num, int $exponent): GMP",
        uncounted!(gmp_pow),
    ),
    (
        "gmp_powm(mixed $num, mixed $exponent, mixed $modulus): GMP",
        uncounted!(gmp_powm),
    ),
    (
        "gmp_powm_sec(mixed $num, mixed $exponent, mixed $modulus): GMP",
        uncounted!(gmp_powm_sec),
    ),
    (
        "gmp_perfect_square(mixed $num): bool",
        uncounted!(gmp_perfect_square),
    ),
    (
        "gmp_perfect_power(mixed $num): bool",
        uncounted!(gmp_perfect_power),
    ),
    (
        "gmp_prob_prime(mixed $num, int $repetitions = 10): int",
        uncounted!(gmp_prob_prime),
    ),
    (
        "gmp_gcd(mixed $num1, mixed $num2): GMP",
        uncounted!(gmp_gcd),
    ),
    (
        "gmp_gcdext(mixed $num1, mixed $num2): array",
        uncounted!(gmp_gcdext),
    ),
    (
        "gmp_lcm(mixed $num1, mixed $num2): GMP",
        uncounted!(gmp_lcm),
    ),
    (
        "gmp_invert(mixed $num1, mixed $num2): GMP|false",
        uncounted!(gmp_invert),
    ),
    (
        "gmp_jacobi(mixed $num1, mixed $num2): int",
        uncounted!(gmp_jacobi),
    ),
    (
        "gmp_legendre(mixed $num1, mixed $num2): int",
        uncounted!(gmp_legendre),
    ),
    (
        "gmp_kronecker(mixed $num1, mixed $num2): int",
        uncounted!(gmp_kronecker),
    ),
    (
        "gmp_cmp(mixed $num1, mixed $num2): int",
        uncounted!(gmp_cmp),
    ),
    (
        "gmp_sign(mixed $num): int",
        uncounted!(gmp_sign),
    ),
    (
        "gmp_random_seed(mixed $seed): void",
        uncounted!(gmp_random_seed),
    ),
    (
        "gmp_random_bits(int $bits): GMP",
        uncounted!(gmp_random_bits),
    ),
    (
        "gmp_random_range(mixed $min, mixed $max): GMP",
        uncounted!(gmp_random_range),
    ),
    (
        "gmp_and(mixed $num1, mixed $num2): GMP",
        uncounted!(gmp_and),
    ),
    (
        "gmp_or(mixed $num1, mixed $num2): GMP",
        uncounted!(gmp_or),
    ),
    (
        "gmp_com(mixed $num): GMP",
        uncounted!(gmp_com),
    ),
    (
        "gmp_xor(mixed $num1, mixed $num2): GMP",
        uncounted!(gmp_xor),
    ),
    (
        "gmp_setbit(GMP $num, int $index, bool $value = true): void",
        uncounted!(gmp_setbit),
    ),
    (
        "gmp_clrbit(GMP $num, int $index): void",
        uncounted!(gmp_clrbit),
    ),
    (
        "gmp_testbit(mixed $num, int $index): bool",
        uncounted!(gmp_testbit),
    ),
    (
        "gmp_scan0(mixed $num1, int $start): int",
        uncounted!(gmp_scan0),
    ),
    (
        "gmp_scan1(mixed $num1, int $start): int",
        uncounted!(gmp_scan1),
    ),
    (
        "gmp_popcount(mixed $num): int",
        uncounted!(gmp_popcount),
    ),
    (
        "gmp_hamdist(mixed $num1, mixed $num2): int",
        uncounted!(gmp_hamdist),
    ),
    (
        "gmp_nextprime(mixed $num): GMP",
        uncounted!(gmp_nextprime),
    ),
    (
        "gmp_prevprime(mixed $num, &$definitely_prime = null): GMP",
        uncounted!(gmp_prevprime),
    ),
    (
        "gmp_binomial(mixed $n, int $k): GMP",
        uncounted!(gmp_binomial),
    ),
];

pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    for &(sig, handler) in FUNCTIONS {
        m.function_static(sig, handler);
    }
}
