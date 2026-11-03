//! PHP-level semantics of the `ext/gmp` functions, the `GMP` class and its
//! operator overloading (port of `ext/gmp/gmp.c`).
//!
//! The VM converts each `GMP|int|string` argument with [`arg`] (in argument
//! order, so that errors surface in the same order as PHP's ZPP), then calls
//! the function taking `&Mpz`.  Values of other types are reported by the VM
//! with [`GmpError::arg_type`] / [`GmpError::operand_type`] after its own
//! weak-mode scalar conversion (`zend_parse_arg_long_weak`).

use crate::mpz::{Endian, Mpz, Round, WordOrder};
use crate::random::MtState;
use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;

/// `GMP_ROUND_ZERO`.
pub const GMP_ROUND_ZERO: i64 = 0;
/// `GMP_ROUND_PLUSINF`.
pub const GMP_ROUND_PLUSINF: i64 = 1;
/// `GMP_ROUND_MINUSINF`.
pub const GMP_ROUND_MINUSINF: i64 = 2;
/// `GMP_MSW_FIRST`.
pub const GMP_MSW_FIRST: i64 = 1 << 0;
/// `GMP_LSW_FIRST`.
pub const GMP_LSW_FIRST: i64 = 1 << 1;
/// `GMP_LITTLE_ENDIAN`.
pub const GMP_LITTLE_ENDIAN: i64 = 1 << 2;
/// `GMP_BIG_ENDIAN`.
pub const GMP_BIG_ENDIAN: i64 = 1 << 3;
/// `GMP_NATIVE_ENDIAN`.
pub const GMP_NATIVE_ENDIAN: i64 = 1 << 4;
/// `GMP_VERSION`: the GMP library version this port mirrors.
pub const GMP_VERSION: &str = "6.3.0";
/// Integer constants registered by the extension.
pub const INT_CONSTANTS: [(&str, i64); 8] = [
    ("GMP_ROUND_ZERO", GMP_ROUND_ZERO),
    ("GMP_ROUND_PLUSINF", GMP_ROUND_PLUSINF),
    ("GMP_ROUND_MINUSINF", GMP_ROUND_MINUSINF),
    ("GMP_MSW_FIRST", GMP_MSW_FIRST),
    ("GMP_LSW_FIRST", GMP_LSW_FIRST),
    ("GMP_LITTLE_ENDIAN", GMP_LITTLE_ENDIAN),
    ("GMP_BIG_ENDIAN", GMP_BIG_ENDIAN),
    ("GMP_NATIVE_ENDIAN", GMP_NATIVE_ENDIAN),
];

const GMP_MAX_BASE: i64 = 62;
const INT_MAX: i64 = i32::MAX as i64;
const GMP_NUMB_BITS: i64 = 64;

/// The exception class PHP throws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// `TypeError`.
    TypeError,
    /// `ValueError`.
    ValueError,
    /// `DivisionByZeroError`.
    DivisionByZeroError,
    /// `Exception` (unserialization failures).
    Exception,
}

impl ErrorClass {
    /// PHP class name.
    pub fn name(self) -> &'static str {
        match self {
            Self::TypeError => "TypeError",
            Self::ValueError => "ValueError",
            Self::DivisionByZeroError => "DivisionByZeroError",
            Self::Exception => "Exception",
        }
    }
}

/// An exception thrown by a GMP function or operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmpError {
    /// Exception class.
    pub class: ErrorClass,
    /// `Some(n)`: an argument error (`"<func>(): Argument #n ($name) "`
    /// prefix); `None`: a plain message.
    pub arg: Option<u32>,
    /// Message (tail for argument errors).
    pub text: String,
}

impl GmpError {
    fn plain(class: ErrorClass, text: impl Into<String>) -> Self {
        Self {
            class,
            arg: None,
            text: text.into(),
        }
    }

    fn argument(class: ErrorClass, arg: u32, text: impl Into<String>) -> Self {
        Self {
            class,
            arg: Some(arg),
            text: text.into(),
        }
    }

    fn value_arg(arg: u32, text: impl Into<String>) -> Self {
        Self::argument(ErrorClass::ValueError, arg, text)
    }

    /// `TypeError` for an argument of an unsupported type:
    /// `must be of type GMP|string|int, <given> given`.
    pub fn arg_type(arg: u32, given: &str) -> Self {
        Self::argument(
            ErrorClass::TypeError,
            arg,
            format!("must be of type GMP|string|int, {given} given"),
        )
    }

    /// `TypeError` for an operator operand of an unsupported type.
    pub fn operand_type(given: &str) -> Self {
        Self::plain(
            ErrorClass::TypeError,
            format!("Number must be of type GMP|string|int, {given} given"),
        )
    }

    /// `ValueError: Number is not an integer string` (operator context).
    pub fn not_integer_string() -> Self {
        Self::plain(ErrorClass::ValueError, "Number is not an integer string")
    }

    /// `TypeError: Unsupported operand types: <t1> <op> <t2>` (shift and
    /// power operators).
    pub fn unsupported_operands(t1: &str, op: BinaryOp, t2: &str) -> Self {
        Self::plain(
            ErrorClass::TypeError,
            format!("Unsupported operand types: {t1} {} {t2}", op.sigil()),
        )
    }

    /// The full exception message as PHP reports it from `func`
    /// (e.g. `"gmp_add"` or `"GMP::__construct"`).
    pub fn message(&self, func: &str) -> String {
        match self.arg {
            None => self.text.clone(),
            Some(n) => format!(
                "{func}(): Argument #{n} (${}) {}",
                param_name(func, n),
                self.text
            ),
        }
    }
}

impl fmt::Display for GmpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.class.name(), self.text)
    }
}

impl std::error::Error for GmpError {}

/// Parameter names from `gmp.stub.php`.
pub fn param_name(func: &str, arg: u32) -> &'static str {
    let names: &[&str] = match func.strip_prefix("gmp_").unwrap_or(func) {
        "init" | "GMP::__construct" | "strval" => &["num", "base"],
        "import" => &["data", "word_size", "flags"],
        "export" => &["num", "word_size", "flags"],
        "add" | "sub" | "mul" | "mod" | "divexact" | "gcd" | "gcdext" | "lcm" | "invert"
        | "jacobi" | "legendre" | "kronecker" | "cmp" | "and" | "or" | "xor" | "hamdist" => {
            &["num1", "num2"]
        }
        "div_qr" | "div_q" | "div_r" | "div" => &["num1", "num2", "rounding_mode"],
        "root" | "rootrem" => &["num", "nth"],
        "pow" => &["num", "exponent"],
        "powm" | "powm_sec" => &["num", "exponent", "modulus"],
        "prob_prime" => &["num", "repetitions"],
        "random_seed" => &["seed"],
        "random_bits" => &["bits"],
        "random_range" => &["min", "max"],
        "setbit" => &["num", "index", "value"],
        "clrbit" | "testbit" => &["num", "index"],
        "scan0" | "scan1" => &["num1", "start"],
        "prevprime" => &["num", "definitely_prime"],
        "binomial" => &["n", "k"],
        "GMP::__unserialize" => &["data"],
        _ => &["num"],
    };
    usize::try_from(arg)
        .ok()
        .and_then(|i| names.get(i.wrapping_sub(1)))
        .copied()
        .unwrap_or("num")
}

/// A `GMP|int|string` argument as classified by the VM.
#[derive(Debug, Clone, Copy)]
pub enum Num<'a> {
    /// A `GMP` instance.
    Gmp(&'a Mpz),
    /// An int (or a scalar the VM converted to int in weak mode).
    Int(i64),
    /// A string (parsed with base auto-detection).
    Str(&'a [u8]),
}

/// `convert_zstr_to_gmp`: PHP's string parsing on top of `mpz_set_str`.
/// `arg_pos == 0` selects the operator-context message.
pub fn parse_str(s: &[u8], base: i64, arg_pos: u32) -> Result<Mpz, GmpError> {
    let err = || {
        if arg_pos == 0 {
            GmpError::not_integer_string()
        } else {
            GmpError::value_arg(arg_pos, "is not an integer string")
        }
    };
    if s.contains(&0) {
        return Err(err());
    }
    let start = s
        .iter()
        .position(|c| !crate::mpz::is_c_space(*c))
        .unwrap_or(s.len());
    let mut num = &s[start..];
    let mut base = base;
    if num.len() >= 2 && num[0] == b'0' {
        let c = num[1];
        let new_base = match c {
            b'x' | b'X' if base == 0 || base == 16 => Some(16),
            b'o' | b'O' if base == 0 || base == 8 => Some(8),
            b'b' | b'B' if base == 0 || base == 2 => Some(2),
            _ => None,
        };
        if let Some(b) = new_base {
            base = b;
            num = &num[2..];
        }
    }
    let base = u32::try_from(base).map_err(|_| err())?;
    Mpz::parse(num, base).ok_or_else(err)
}

/// Converts a function argument (`GMP_Z_PARAM_INTO_MPZ_PTR`).
pub fn arg(n: Num<'_>, pos: u32) -> Result<Cow<'_, Mpz>, GmpError> {
    match n {
        Num::Gmp(m) => Ok(Cow::Borrowed(m)),
        Num::Int(i) => Ok(Cow::Owned(Mpz::from(i))),
        Num::Str(s) => parse_str(s, 0, pos).map(Cow::Owned),
    }
}

/// Converts an operator operand (operator-context error messages).
pub fn operand(n: Num<'_>) -> Result<Cow<'_, Mpz>, GmpError> {
    arg(n, 0)
}

fn verify_base(base: i64, arg_num: u32) -> Result<(), GmpError> {
    if base != 0 && !(2..=GMP_MAX_BASE).contains(&base) {
        return Err(GmpError::value_arg(
            arg_num,
            format!("must be 0 or between 2 and {GMP_MAX_BASE}"),
        ));
    }
    Ok(())
}

/// The `int|string` argument of `gmp_init()` / `GMP::__construct()`.
#[derive(Debug, Clone, Copy)]
pub enum StrOrInt<'a> {
    /// An int.
    Int(i64),
    /// A string.
    Str(&'a [u8]),
}

/// `gmp_init(int|string $num, int $base = 0)` (also `GMP::__construct`).
pub fn gmp_init(num: StrOrInt<'_>, base: i64) -> Result<Mpz, GmpError> {
    verify_base(base, 2)?;
    match num {
        StrOrInt::Int(i) => Ok(Mpz::from(i)),
        StrOrInt::Str(s) => parse_str(s, base, 1),
    }
}

fn import_export_options(size: i64, options: i64) -> Result<(WordOrder, Endian), GmpError> {
    if size < 1 {
        return Err(GmpError::value_arg(2, "must be greater than or equal to 1"));
    }
    let order = match options & (GMP_LSW_FIRST | GMP_MSW_FIRST) {
        GMP_LSW_FIRST => WordOrder::LeastSignificantFirst,
        GMP_MSW_FIRST | 0 => WordOrder::MostSignificantFirst,
        _ => {
            return Err(GmpError::value_arg(
                3,
                "cannot use multiple word order options",
            ))
        }
    };
    let endian = match options & (GMP_LITTLE_ENDIAN | GMP_BIG_ENDIAN | GMP_NATIVE_ENDIAN) {
        GMP_LITTLE_ENDIAN => Endian::Little,
        GMP_BIG_ENDIAN => Endian::Big,
        GMP_NATIVE_ENDIAN | 0 => Endian::Native,
        _ => return Err(GmpError::value_arg(3, "cannot use multiple endian options")),
    };
    Ok((order, endian))
}

/// Default `$flags` of `gmp_import()` / `gmp_export()`.
pub const DEFAULT_IMPORT_EXPORT_FLAGS: i64 = GMP_MSW_FIRST | GMP_NATIVE_ENDIAN;

/// `gmp_import(string $data, int $word_size = 1, int $flags = ...)`.
pub fn gmp_import(data: &[u8], word_size: i64, flags: i64) -> Result<Mpz, GmpError> {
    let (order, endian) = import_export_options(word_size, flags)?;
    let size = usize::try_from(word_size).unwrap_or(usize::MAX);
    if data.len() % size != 0 {
        return Err(GmpError::value_arg(
            1,
            "must be a multiple of argument #2 ($word_size)",
        ));
    }
    Ok(Mpz::import(data, size, order, endian))
}

/// `gmp_export(GMP|int|string $num, int $word_size = 1, int $flags = ...)`.
pub fn gmp_export(num: &Mpz, word_size: i64, flags: i64) -> Result<Vec<u8>, GmpError> {
    let (order, endian) = import_export_options(word_size, flags)?;
    if num.is_zero() {
        return Ok(Vec::new());
    }
    let size_in_base_2 = num.bit_len();
    let size = word_size.unsigned_abs();
    if word_size > i64::MAX / 4 || size_in_base_2 > u64::MAX - size * 8 + 1 {
        return Err(GmpError::value_arg(
            2,
            "is too large for argument #1 ($num)",
        ));
    }
    let size = usize::try_from(size)
        .map_err(|_| GmpError::value_arg(2, "is too large for argument #1 ($num)"))?;
    Ok(num.export(size, order, endian))
}

/// `gmp_intval()`: `mpz_get_si` (low bits with the sign applied).
pub fn gmp_intval(num: &Mpz) -> i64 {
    num.get_si()
}

/// `gmp_strval(GMP|int|string $num, int $base = 10)`.
pub fn gmp_strval(num: &Mpz, base: i64) -> Result<String, GmpError> {
    if (-1..=1).contains(&base) || base > GMP_MAX_BASE || base < -36 {
        return Err(GmpError::value_arg(
            2,
            format!("must be between 2 and {GMP_MAX_BASE}, or -2 and -36"),
        ));
    }
    Ok(num.to_str_radix(i32::try_from(base).unwrap_or(10)))
}

/// `gmp_add()`.
pub fn gmp_add(a: &Mpz, b: &Mpz) -> Mpz {
    a + b
}

/// `gmp_sub()`.
pub fn gmp_sub(a: &Mpz, b: &Mpz) -> Mpz {
    a - b
}

/// `gmp_mul()`.
pub fn gmp_mul(a: &Mpz, b: &Mpz) -> Mpz {
    a * b
}

fn division_by_zero_arg() -> GmpError {
    GmpError::argument(ErrorClass::DivisionByZeroError, 2, "Division by zero")
}

fn rounding(mode: i64) -> Result<Round, GmpError> {
    match mode {
        GMP_ROUND_ZERO => Ok(Round::Zero),
        GMP_ROUND_PLUSINF => Ok(Round::PlusInf),
        GMP_ROUND_MINUSINF => Ok(Round::MinusInf),
        _ => Err(GmpError::value_arg(
            3,
            "must be one of GMP_ROUND_ZERO, GMP_ROUND_PLUSINF, or GMP_ROUND_MINUSINF",
        )),
    }
}

/// `gmp_div_qr()`: `[quotient, remainder]`.
pub fn gmp_div_qr(a: &Mpz, b: &Mpz, rounding_mode: i64) -> Result<(Mpz, Mpz), GmpError> {
    if b.is_zero() {
        return Err(division_by_zero_arg());
    }
    Ok(a.div_qr(b, rounding(rounding_mode)?))
}

/// `gmp_div_q()` (and its alias `gmp_div()`).
pub fn gmp_div_q(a: &Mpz, b: &Mpz, rounding_mode: i64) -> Result<Mpz, GmpError> {
    gmp_div_qr(a, b, rounding_mode).map(|(q, _)| q)
}

/// `gmp_div_r()`.
pub fn gmp_div_r(a: &Mpz, b: &Mpz, rounding_mode: i64) -> Result<Mpz, GmpError> {
    gmp_div_qr(a, b, rounding_mode).map(|(_, r)| r)
}

/// `gmp_mod()`: non-negative remainder.
pub fn gmp_mod(a: &Mpz, b: &Mpz) -> Result<Mpz, GmpError> {
    if b.is_zero() {
        return Err(GmpError::argument(
            ErrorClass::DivisionByZeroError,
            2,
            "Modulo by zero",
        ));
    }
    Ok(a.modulo(b))
}

/// `gmp_divexact()`.
pub fn gmp_divexact(a: &Mpz, b: &Mpz) -> Result<Mpz, GmpError> {
    if b.is_zero() {
        return Err(division_by_zero_arg());
    }
    Ok(a.divexact(b))
}

/// `gmp_neg()`.
pub fn gmp_neg(a: &Mpz) -> Mpz {
    -a
}

/// `gmp_abs()`.
pub fn gmp_abs(a: &Mpz) -> Mpz {
    a.abs()
}

/// `gmp_com()`.
pub fn gmp_com(a: &Mpz) -> Mpz {
    a.com()
}

/// `gmp_nextprime()`.
pub fn gmp_nextprime(a: &Mpz) -> Mpz {
    a.next_prime()
}

/// `gmp_prevprime(GMP|int|string $num, &$definitely_prime = null)`:
/// the prime and the value for `$definitely_prime`.
pub fn gmp_prevprime(a: &Mpz) -> Result<(Mpz, bool), GmpError> {
    match a.prev_prime() {
        None => Err(GmpError::value_arg(1, "must be greater than 2")),
        Some((p, r)) => Ok((p, r == 2)),
    }
}

fn ulong_range(arg: u32) -> GmpError {
    GmpError::value_arg(arg, format!("must be between 0 and {}", u64::MAX))
}

/// `gmp_fact()`.
pub fn gmp_fact(a: &Mpz) -> Result<Mpz, GmpError> {
    let n = a.to_u64().ok_or_else(|| ulong_range(1))?;
    Ok(Mpz::factorial(n))
}

/// `gmp_binomial(GMP|int|string $n, int $k)`.
pub fn gmp_binomial(n: &Mpz, k: i64) -> Result<Mpz, GmpError> {
    let k = u64::try_from(k).map_err(|_| ulong_range(2))?;
    Ok(n.binomial(k))
}

/// `gmp_pow(GMP|int|string $num, int $exponent)`.
pub fn gmp_pow(base: &Mpz, exponent: i64) -> Result<Mpz, GmpError> {
    let e = u64::try_from(exponent).map_err(|_| ulong_range(2))?;
    Ok(base.pow(e))
}

/// `gmp_powm()`.
pub fn gmp_powm(base: &Mpz, exponent: &Mpz, modulus: &Mpz) -> Result<Mpz, GmpError> {
    if exponent.is_negative() {
        return Err(GmpError::value_arg(2, "must be greater than or equal to 0"));
    }
    if modulus.is_zero() {
        return Err(GmpError::argument(
            ErrorClass::DivisionByZeroError,
            3,
            "Modulo by zero",
        ));
    }
    Ok(base.powm(exponent, modulus))
}

/// `gmp_powm_sec()`.
pub fn gmp_powm_sec(base: &Mpz, exponent: &Mpz, modulus: &Mpz) -> Result<Mpz, GmpError> {
    if exponent.sign() <= 0 {
        return Err(GmpError::value_arg(2, "must be greater than 0"));
    }
    if !modulus.is_odd() {
        if modulus.is_zero() {
            return Err(GmpError::argument(
                ErrorClass::DivisionByZeroError,
                3,
                "Modulo by zero",
            ));
        }
        return Err(GmpError::value_arg(3, "must be odd"));
    }
    Ok(base.powm(exponent, modulus))
}

fn non_negative(a: &Mpz) -> Result<(), GmpError> {
    if a.is_negative() {
        Err(GmpError::value_arg(1, "must be greater than or equal to 0"))
    } else {
        Ok(())
    }
}

/// `gmp_sqrt()`.
pub fn gmp_sqrt(a: &Mpz) -> Result<Mpz, GmpError> {
    non_negative(a)?;
    Ok(a.sqrt())
}

/// `gmp_sqrtrem()`: `[root, remainder]`.
pub fn gmp_sqrtrem(a: &Mpz) -> Result<(Mpz, Mpz), GmpError> {
    non_negative(a)?;
    Ok(a.sqrtrem())
}

fn check_nth(a: &Mpz, nth: i64) -> Result<u64, GmpError> {
    if nth <= 0 {
        return Err(GmpError::value_arg(
            2,
            format!("must be between 1 and {}", u64::MAX),
        ));
    }
    if nth % 2 == 0 && a.is_negative() {
        return Err(GmpError::value_arg(
            2,
            "must be odd if argument #1 ($num) is negative",
        ));
    }
    Ok(nth.unsigned_abs())
}

/// `gmp_root(GMP|int|string $num, int $nth)`.
pub fn gmp_root(a: &Mpz, nth: i64) -> Result<Mpz, GmpError> {
    let n = check_nth(a, nth)?;
    Ok(a.root(n))
}

/// `gmp_rootrem(GMP|int|string $num, int $nth)`: `[root, remainder]`.
pub fn gmp_rootrem(a: &Mpz, nth: i64) -> Result<(Mpz, Mpz), GmpError> {
    let n = check_nth(a, nth)?;
    Ok(a.rootrem(n))
}

/// `gmp_perfect_square()`.
pub fn gmp_perfect_square(a: &Mpz) -> bool {
    a.is_perfect_square()
}

/// `gmp_perfect_power()`.
pub fn gmp_perfect_power(a: &Mpz) -> bool {
    a.is_perfect_power()
}

/// `gmp_prob_prime(GMP|int|string $num, int $repetitions = 10)`: 0, 1 or 2.
pub fn gmp_prob_prime(a: &Mpz, repetitions: i64) -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    let reps = repetitions as i32;
    i64::from(a.probab_prime(reps))
}

/// `gmp_gcd()`.
pub fn gmp_gcd(a: &Mpz, b: &Mpz) -> Mpz {
    a.gcd(b)
}

/// `gmp_gcdext()`: `["g" => g, "s" => s, "t" => t]`.
pub fn gmp_gcdext(a: &Mpz, b: &Mpz) -> (Mpz, Mpz, Mpz) {
    a.gcdext(b)
}

/// `gmp_lcm()`.
pub fn gmp_lcm(a: &Mpz, b: &Mpz) -> Mpz {
    a.lcm(b)
}

/// `gmp_invert()`: `None` is PHP `false`.
pub fn gmp_invert(a: &Mpz, b: &Mpz) -> Result<Option<Mpz>, GmpError> {
    if b.is_zero() {
        return Err(GmpError::plain(
            ErrorClass::DivisionByZeroError,
            "Division by zero",
        ));
    }
    Ok(a.invert(b))
}

/// `gmp_jacobi()`.
pub fn gmp_jacobi(a: &Mpz, b: &Mpz) -> i64 {
    i64::from(a.kronecker(b))
}

/// `gmp_legendre()`.
pub fn gmp_legendre(a: &Mpz, b: &Mpz) -> i64 {
    i64::from(a.kronecker(b))
}

/// `gmp_kronecker()`.
pub fn gmp_kronecker(a: &Mpz, b: &Mpz) -> i64 {
    i64::from(a.kronecker(b))
}

/// `gmp_cmp()`: -1, 0 or 1.
pub fn gmp_cmp(a: &Mpz, b: &Mpz) -> i64 {
    match a.cmp(b) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

/// `gmp_sign()`.
pub fn gmp_sign(a: &Mpz) -> i64 {
    i64::from(a.sign())
}

/// `gmp_and()`.
pub fn gmp_and(a: &Mpz, b: &Mpz) -> Mpz {
    a.and(b)
}

/// `gmp_or()`.
pub fn gmp_or(a: &Mpz, b: &Mpz) -> Mpz {
    a.or(b)
}

/// `gmp_xor()`.
pub fn gmp_xor(a: &Mpz, b: &Mpz) -> Mpz {
    a.xor(b)
}

fn bit_index(index: i64) -> Result<u64, GmpError> {
    if index >= 0 && index / GMP_NUMB_BITS < INT_MAX {
        Ok(index.unsigned_abs())
    } else {
        Err(GmpError::value_arg(
            2,
            format!("must be between 0 and {INT_MAX} * {GMP_NUMB_BITS}"),
        ))
    }
}

/// `gmp_setbit(GMP $num, int $index, bool $value = true)`: mutates `num`.
pub fn gmp_setbit(num: &mut Mpz, index: i64, value: bool) -> Result<(), GmpError> {
    let i = bit_index(index)?;
    if value {
        num.set_bit(i);
    } else {
        num.clear_bit(i);
    }
    Ok(())
}

/// `gmp_clrbit(GMP $num, int $index)`: mutates `num`.
pub fn gmp_clrbit(num: &mut Mpz, index: i64) -> Result<(), GmpError> {
    let i = bit_index(index)?;
    num.clear_bit(i);
    Ok(())
}

/// `gmp_testbit()`.
pub fn gmp_testbit(num: &Mpz, index: i64) -> Result<bool, GmpError> {
    Ok(num.test_bit(bit_index(index)?))
}

/// Converts an `mp_bitcnt_t` result to a PHP int (`ULONG_MAX` becomes -1).
#[allow(clippy::cast_possible_wrap)]
fn bitcnt(v: u64) -> i64 {
    v as i64
}

/// `gmp_scan0()`.
pub fn gmp_scan0(num: &Mpz, start: i64) -> Result<i64, GmpError> {
    Ok(bitcnt(num.scan0(bit_index(start)?)))
}

/// `gmp_scan1()`.
pub fn gmp_scan1(num: &Mpz, start: i64) -> Result<i64, GmpError> {
    Ok(bitcnt(num.scan1(bit_index(start)?)))
}

/// `gmp_popcount()` (-1 for negative numbers).
pub fn gmp_popcount(num: &Mpz) -> i64 {
    bitcnt(num.popcount())
}

/// `gmp_hamdist()` (-1 when the signs differ).
pub fn gmp_hamdist(a: &Mpz, b: &Mpz) -> i64 {
    bitcnt(a.hamdist(b))
}

/// Per-request random state (`GMPG(rand_state)`), lazily seeded from the
/// caller-supplied entropy like `gmp_init_random()`.
#[derive(Debug, Default, Clone)]
pub struct GmpRandom {
    state: Option<MtState>,
}

impl GmpRandom {
    /// An uninitialized generator.
    pub fn new() -> Self {
        Self::default()
    }

    fn init(&mut self, entropy: impl FnOnce() -> u64) -> &mut MtState {
        self.state
            .get_or_insert_with(|| MtState::from_u64(entropy()))
    }

    /// `gmp_random_seed()`.
    pub fn seed(&mut self, seed: &Mpz, entropy: impl FnOnce() -> u64) {
        self.init(entropy).seed(seed);
    }

    /// `gmp_random_bits(int $bits)`.
    pub fn bits(&mut self, bits: i64, entropy: impl FnOnce() -> u64) -> Result<Mpz, GmpError> {
        if bits <= 0 || bits > INT_MAX {
            return Err(GmpError::value_arg(
                1,
                format!("must be between 1 and {INT_MAX}"),
            ));
        }
        Ok(self.init(entropy).urandomb(bits.unsigned_abs()))
    }

    /// `gmp_random_range()`.
    pub fn range(
        &mut self,
        min: &Mpz,
        max: &Mpz,
        entropy: impl FnOnce() -> u64,
    ) -> Result<Mpz, GmpError> {
        let st = self.init(entropy);
        if max <= min {
            return Err(GmpError::value_arg(
                1,
                "must be less than argument #2 ($max)",
            ));
        }
        let range = &(max - min) + &Mpz::from(1);
        Ok(&st.urandomm(&range) + min)
    }

    /// Forgets the state (`RSHUTDOWN`).
    pub fn reset(&mut self) {
        self.state = None;
    }
}

/// Overloaded binary operators of the `GMP` class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/` (truncating)
    Div,
    /// `%` (`mpz_mod`)
    Mod,
    /// `**`
    Pow,
    /// `<<`
    Shl,
    /// `>>` (floor)
    Shr,
    /// `|`
    BitOr,
    /// `&`
    BitAnd,
    /// `^`
    BitXor,
}

impl BinaryOp {
    /// Operator sigil as used in error messages.
    pub fn sigil(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Mod => "%",
            Self::Pow => "**",
            Self::Shl => "<<",
            Self::Shr => ">>",
            Self::BitOr => "|",
            Self::BitAnd => "&",
            Self::BitXor => "^",
        }
    }

    /// Whether the right operand is an unsigned count (`**`, `<<`, `>>`),
    /// converted with [`shift_amount`] instead of [`operand`].
    pub fn takes_count(self) -> bool {
        matches!(self, Self::Pow | Self::Shl | Self::Shr)
    }
}

/// `binop_operator_helper`: both operands already converted with
/// [`operand`] (left first).
///
/// # Panics
/// Panics if called with a count operator (see [`BinaryOp::takes_count`]).
pub fn binary_op(op: BinaryOp, a: &Mpz, b: &Mpz) -> Result<Mpz, GmpError> {
    Ok(match op {
        BinaryOp::Add => a + b,
        BinaryOp::Sub => a - b,
        BinaryOp::Mul => a * b,
        BinaryOp::Div => {
            if b.is_zero() {
                return Err(GmpError::plain(
                    ErrorClass::DivisionByZeroError,
                    "Division by zero",
                ));
            }
            a.div_q(b, Round::Zero)
        }
        BinaryOp::Mod => {
            if b.is_zero() {
                return Err(GmpError::plain(
                    ErrorClass::DivisionByZeroError,
                    "Modulo by zero",
                ));
            }
            a.modulo(b)
        }
        BinaryOp::BitOr => a.or(b),
        BinaryOp::BitAnd => a.and(b),
        BinaryOp::BitXor => a.xor(b),
        BinaryOp::Pow | BinaryOp::Shl | BinaryOp::Shr => {
            panic!("count operator passed to binary_op")
        }
    })
}

/// The right operand of `**`, `<<` and `>>` after the VM's classification:
/// ints, GMP objects, and floats/numeric strings already converted to int
/// (non-integer strings throw [`GmpError::not_integer_string`]; other types
/// throw [`GmpError::unsupported_operands`]).
#[derive(Debug, Clone, Copy)]
pub enum CountOperand<'a> {
    /// An int.
    Int(i64),
    /// A GMP instance.
    Gmp(&'a Mpz),
}

/// Validates the count operand (checked before the left operand is
/// converted, as in `shift_operator_helper`).
pub fn shift_amount(op: BinaryOp, amount: CountOperand<'_>) -> Result<u64, GmpError> {
    let range_error = || {
        GmpError::plain(
            ErrorClass::ValueError,
            format!(
                "{} must be between 0 and {}",
                if op == BinaryOp::Pow {
                    "Exponent"
                } else {
                    "Shift"
                },
                u64::MAX
            ),
        )
    };
    match amount {
        CountOperand::Int(i) => u64::try_from(i).map_err(|_| range_error()),
        CountOperand::Gmp(m) => m.to_u64().ok_or_else(range_error),
    }
}

/// Applies `**`, `<<` or `>>` with a validated count.
pub fn count_op(op: BinaryOp, a: &Mpz, count: u64) -> Mpz {
    match op {
        BinaryOp::Pow => a.pow(count),
        BinaryOp::Shl => a.shl(count),
        BinaryOp::Shr => a.shr_floor(count),
        _ => panic!("not a count operator"),
    }
}

/// `~$gmp`.
pub fn bitwise_not(a: &Mpz) -> Mpz {
    a.com()
}

/// `gmp_compare`: three-way comparison of two converted operands.
pub fn compare(a: &Mpz, b: &Mpz) -> i32 {
    match a.cmp(b) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

/// Result of the `_IS_NUMBER` cast.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Number {
    /// Fits in a PHP int.
    Int(i64),
    /// Converted with `mpz_get_d`.
    Float(f64),
}

/// `(string)$gmp`.
pub fn cast_string(a: &Mpz) -> String {
    a.to_str_radix(10)
}

/// `(int)$gmp`: `mpz_get_si`.
pub fn cast_int(a: &Mpz) -> i64 {
    a.get_si()
}

/// `(float)$gmp`: `mpz_get_d` (truncation).
pub fn cast_float(a: &Mpz) -> f64 {
    a.to_f64()
}

/// `(bool)$gmp`.
pub fn cast_bool(a: &Mpz) -> bool {
    !a.is_zero()
}

/// `_IS_NUMBER` cast (arithmetic with non-GMP-aware code, e.g. `array_sum`).
pub fn cast_number(a: &Mpz) -> Number {
    match a.to_i64() {
        Some(i) => Number::Int(i),
        None => Number::Float(a.to_f64()),
    }
}

/// Value of the `"num"` entry shown by `var_dump()` / `print_r()`.
pub fn debug_num(a: &Mpz) -> String {
    a.to_str_radix(10)
}

/// `GMP::__serialize()` element 0: the number in base 16.
pub fn serialize_number(a: &Mpz) -> String {
    a.to_str_radix(16)
}

/// `GMP::__unserialize()` element 0 (`None` if missing or not a string).
pub fn unserialize_number(data: Option<&[u8]>) -> Result<Mpz, GmpError> {
    data.and_then(|s| parse_str(s, 16, 0).ok())
        .ok_or_else(unserialize_number_error)
}

/// `Exception: Could not unserialize number`.
pub fn unserialize_number_error() -> GmpError {
    GmpError::plain(ErrorClass::Exception, "Could not unserialize number")
}

/// `Exception: Could not unserialize properties`.
pub fn unserialize_properties_error() -> GmpError {
    GmpError::plain(ErrorClass::Exception, "Could not unserialize properties")
}

/// The legacy `C:3:"GMP":...` payload: the first value must be a string in
/// base 10 (`None` if it is missing or not a string).
pub fn unserialize_legacy_number(data: Option<&[u8]>) -> Result<Mpz, GmpError> {
    data.and_then(|s| parse_str(s, 10, 0).ok())
        .ok_or_else(unserialize_number_error)
}

/// The legacy (`Serializable`-style) payload `gmp_serialize` writes: the
/// serialized decimal string followed by the serialized property table
/// (supplied by the VM, e.g. `a:0:{}`).
pub fn serialize_legacy(a: &Mpz, serialized_props: &str) -> String {
    let s = a.to_str_radix(10);
    format!("s:{}:\"{s}\";{serialized_props}", s.len())
}
