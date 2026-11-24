// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Float → string conversion matching PHP (`zend_gcvt`, `smart_str_append_double`).
//!
//! PHP formats floats through `zend_dtoa` in one of two modes:
//!
//! * mode 0 (used when the precision is `-1`, e.g. the default
//!   `serialize_precision`): the shortest digit string that round-trips;
//! * mode 2 with `n` digits (used for `precision`, default 14): the value
//!   correctly rounded (half-even on exact ties) to `n` significant digits with
//!   trailing zeros removed.
//!
//! Mode 0 digits come from a Ryu digit generator ([`ryu_shortest`], the
//! closest shortest candidate with ties to even, like `zend_dtoa`), which
//! yields an integer and an exponent without text formatting. Mode 2
//! digits are rounded from those when that is provably exact, and come
//! from Rust's correctly rounded fixed-precision formatter otherwise.
//!
//! Like php-src (`zend_gcvt` writes into a caller's stack buffer, then
//! `zend_double_to_str` makes one exactly sized `zend_string`), the common
//! cases (at most 17 significant digits) are formatted without heap
//! temporaries: digits and output live in stack buffers, integral values
//! skip the float formatter, and mode 2 with at most 15 digits tries the
//! cheap shortest digits before the exact (bignum-backed) ones.

use crate::string::ZStr;
use std::fmt::{self, Write as _};

mod ryu_tables;

use ryu_tables::{POW5_INV_SPLIT, POW5_SPLIT};

/// Decimal digits of a float as produced by `zend_dtoa`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DtoaDigits {
    /// Significant digits, without leading/trailing zeros (`"0"` for zero).
    pub digits: String,
    /// Position of the decimal point relative to the digits (`decpt`): the
    /// value is `0.DIGITS × 10^decpt`.
    pub decpt: i32,
    /// Whether the value is negative (including `-0.0`).
    pub negative: bool,
}

/// Up to 17 significant digits of a finite, non-zero float in a stack
/// buffer: the `zend_dtoa` result of mode 0 and of mode 2 with at most 17
/// digits.
#[derive(Debug, Clone, Copy)]
struct SmallDigits {
    buf: [u8; 24],
    len: usize,
    decpt: i32,
}

impl SmallDigits {
    fn digits(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// A fixed-capacity byte buffer that `write!` can format into (the
/// `char buf[ZEND_DOUBLE_MAX_LENGTH]` of `zend_double_to_str`).
#[derive(Debug)]
struct StackBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackBuf<N> {
    const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// The bytes as a `String`, each byte one `char` (what pushing
    /// `char::from(byte)` gives; only the decimal point can be non-ASCII).
    fn to_latin1_string(&self) -> String {
        self.as_bytes().iter().map(|&c| char::from(c)).collect()
    }
}

impl<const N: usize> fmt::Write for StackBuf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self
            .len
            .checked_add(s.len())
            .filter(|&e| e <= N)
            .ok_or(fmt::Error)?;
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Where [`emit_gcvt`] writes.
trait GcvtOut {
    fn put(&mut self, b: u8);

    fn put_all(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.put(b);
        }
    }
}

impl GcvtOut for String {
    fn put(&mut self, b: u8) {
        self.push(char::from(b));
    }
}

impl<const N: usize> GcvtOut for StackBuf<N> {
    fn put(&mut self, b: u8) {
        // Callers size the buffer for the output (at most 17 digits); a
        // byte past the end would be a bug, and is dropped rather than
        // panicking.
        debug_assert!(self.len < N, "float output buffer overflow");
        if let Some(slot) = self.buf.get_mut(self.len) {
            *slot = b;
            self.len += 1;
        }
    }
    fn put_all(&mut self, bytes: &[u8]) {
        debug_assert!(self.len + bytes.len() <= N, "float output buffer overflow");
        let end = (self.len + bytes.len()).min(N);
        let take = end - self.len;
        self.buf[self.len..end].copy_from_slice(&bytes[..take]);
        self.len = end;
    }
}

/// Parses Rust's `{:e}` output (`d.ddde±x`) into significant digits with
/// trailing zeros removed and `decpt`.
fn parse_exp_format(s: &[u8]) -> Option<SmallDigits> {
    let mut out = SmallDigits {
        buf: [0; 24],
        len: 0,
        decpt: 0,
    };
    // One pass over the mantissa (`d` or `d.ddd`), up to the `e`.
    let mut rest = s;
    loop {
        let (&c, tail) = rest.split_first()?;
        rest = tail;
        if c == b'e' {
            break;
        }
        if c.is_ascii_digit() {
            *out.buf.get_mut(out.len)? = c;
            out.len += 1;
        }
    }
    let exp = rest;
    while out.len > 1 && out.buf[out.len - 1] == b'0' {
        out.len -= 1;
    }
    if out.len == 0 {
        return None;
    }
    let (neg, exp_digits) = match exp.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, exp),
    };
    if exp_digits.is_empty() {
        return None;
    }
    let mut x: i32 = 0;
    for &c in exp_digits {
        if !c.is_ascii_digit() {
            return None;
        }
        x = x.checked_mul(10)?.checked_add(i32::from(c - b'0'))?;
    }
    let exp = if neg { -x } else { x };
    out.decpt = exp + 1;
    Some(out)
}

/// Shortest round-trip decimal digits of a finite, positive, non-zero
/// float (Ryu, Ulf Adams 2018): `(output, exp10)` with the value's
/// shortest representation `output × 10^exp10`. The candidate closest to
/// the value wins, ties resolved to an even last digit; the interval
/// bounds count as round-tripping when the mantissa is even, as in
/// `zend_dtoa` mode 0.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::too_many_lines
)]
fn ryu_shortest(abs: f64) -> (u64, i32) {
    const MANTISSA_BITS: u32 = 52;
    const BIAS: i32 = 1023;
    const POW5_INV_BITCOUNT: i32 = 125;
    const POW5_BITCOUNT: i32 = 125;

    /// `ceil(log2(5^e))` (1 for `e == 0`), for `0 <= e <= 3528`.
    fn pow5bits(e: i32) -> i32 {
        (((e as u32) * 1_217_359) >> 19) as i32 + 1
    }
    /// `floor(log10(2^e))` for `0 <= e <= 1650`.
    fn log10_pow2(e: i32) -> i32 {
        (((e as u32) * 78_913) >> 18) as i32
    }
    /// `floor(log10(5^e))` for `0 <= e <= 2620`.
    fn log10_pow5(e: i32) -> i32 {
        (((e as u32) * 732_923) >> 20) as i32
    }
    fn pow5_factor(mut v: u64) -> u32 {
        let mut count = 0;
        while v % 5 == 0 {
            v /= 5;
            count += 1;
        }
        count
    }
    fn multiple_of_pow5(v: u64, p: i32) -> bool {
        i64::from(pow5_factor(v)) >= i64::from(p)
    }
    /// `(m × mul) >> j` for a 128-bit `mul` and `j >= 64`.
    fn mul_shift(m: u64, mul: (u64, u64), j: i32) -> u64 {
        let b0 = u128::from(m) * u128::from(mul.0);
        let b2 = u128::from(m) * u128::from(mul.1);
        (((b0 >> 64) + b2) >> (j - 64)) as u64
    }

    let bits = abs.to_bits();
    let ieee_mantissa = bits & ((1u64 << MANTISSA_BITS) - 1);
    let ieee_exponent = ((bits >> MANTISSA_BITS) & 0x7ff) as i32;
    let (e2, m2) = if ieee_exponent == 0 {
        (1 - BIAS - MANTISSA_BITS as i32 - 2, ieee_mantissa)
    } else {
        (
            ieee_exponent - BIAS - MANTISSA_BITS as i32 - 2,
            (1u64 << MANTISSA_BITS) | ieee_mantissa,
        )
    };
    let accept_bounds = m2 & 1 == 0;
    // The value and its interval's bounds, scaled by 4: [mm, mp].
    let mv = 4 * m2;
    let mm_shift = u64::from(ieee_mantissa != 0 || ieee_exponent <= 1);
    let mp_in = mv + 2;
    let mm_in = mv - 1 - mm_shift;

    let mut vm_trailing_zeros = false;
    let mut vr_trailing_zeros = false;
    let (e10, mut vr, mut vp, mut vm);
    if e2 >= 0 {
        let q = log10_pow2(e2) - i32::from(e2 > 3);
        e10 = q;
        let k = POW5_INV_BITCOUNT + pow5bits(q) - 1;
        let i = -e2 + q + k;
        let mul = POW5_INV_SPLIT[q as usize];
        vr = mul_shift(mv, mul, i);
        vp = mul_shift(mp_in, mul, i);
        vm = mul_shift(mm_in, mul, i);
        if q <= 21 {
            if mv % 5 == 0 {
                vr_trailing_zeros = multiple_of_pow5(mv, q);
            } else if accept_bounds {
                vm_trailing_zeros = multiple_of_pow5(mm_in, q);
            } else {
                vp -= u64::from(multiple_of_pow5(mp_in, q));
            }
        }
    } else {
        let q = log10_pow5(-e2) - i32::from(-e2 > 1);
        e10 = q + e2;
        let i = -e2 - q;
        let k = pow5bits(i) - POW5_BITCOUNT;
        let j = q - k;
        let mul = POW5_SPLIT[i as usize];
        vr = mul_shift(mv, mul, j);
        vp = mul_shift(mp_in, mul, j);
        vm = mul_shift(mm_in, mul, j);
        if q <= 1 {
            // mv has at least q trailing zero bits.
            vr_trailing_zeros = true;
            if accept_bounds {
                vm_trailing_zeros = mm_shift == 1;
            } else {
                vp -= 1;
            }
        } else if q < 63 {
            vr_trailing_zeros = mv & ((1u64 << q) - 1) == 0;
        }
    }

    let mut removed = 0i32;
    let output = if vm_trailing_zeros || vr_trailing_zeros {
        let mut last_removed = 0u64;
        while vp / 10 > vm / 10 {
            vm_trailing_zeros &= vm % 10 == 0;
            vr_trailing_zeros &= last_removed == 0;
            last_removed = vr % 10;
            vr /= 10;
            vp /= 10;
            vm /= 10;
            removed += 1;
        }
        if vm_trailing_zeros {
            while vm % 10 == 0 {
                vr_trailing_zeros &= last_removed == 0;
                last_removed = vr % 10;
                vr /= 10;
                vm /= 10;
                removed += 1;
            }
        }
        if vr_trailing_zeros && last_removed == 5 && vr % 2 == 0 {
            // Exactly halfway: round to even.
            last_removed = 4;
        }
        vr + u64::from((vr == vm && (!accept_bounds || !vm_trailing_zeros)) || last_removed >= 5)
    } else {
        let mut round_up = false;
        if vp / 100 > vm / 100 {
            round_up = vr % 100 >= 50;
            vr /= 100;
            vp /= 100;
            vm /= 100;
            removed += 2;
        }
        while vp / 10 > vm / 10 {
            round_up = vr % 10 >= 5;
            vr /= 10;
            vp /= 10;
            vm /= 10;
            removed += 1;
        }
        vr + u64::from(vr == vm || round_up)
    };
    (output, e10 + removed)
}

/// Ten to the power `k` for `k <= 19`.
const POW10: [u64; 20] = {
    let mut t = [1u64; 20];
    let mut i = 1;
    while i < 20 {
        t[i] = t[i - 1] * 10;
        i += 1;
    }
    t
};

/// `"00"` to `"99"`: two decimal digits per lookup.
const DIGIT_PAIRS: &[u8; 200] = b"\
0001020304050607080910111213141516171819\
2021222324252627282930313233343536373839\
4041424344454647484950515253545556575859\
6061626364656667686970717273747576777879\
8081828384858687888990919293949596979899";

/// The shortest round-trip digits of a finite, positive, non-zero float
/// (`zend_dtoa` mode 0), trailing zeros removed.
fn shortest_digits(abs: f64) -> SmallDigits {
    let (mut output, mut exp) = ryu_shortest(abs);
    while output >= 10 && output % 10 == 0 {
        output /= 10;
        exp += 1;
    }
    // Ryu's output has at most 17 digits.
    let len = POW10[1..18].iter().take_while(|&&p| output >= p).count() + 1;
    let mut out = SmallDigits {
        buf: [0; 24],
        len,
        // `len <= 17`: the cast cannot truncate.
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        decpt: len as i32 + exp,
    };
    let mut pos = len;
    while output >= 100 {
        // `output % 100 < 100`: the cast cannot truncate.
        #[allow(clippy::cast_possible_truncation)]
        let pair = 2 * (output % 100) as usize;
        output /= 100;
        out.buf[pos - 2..pos].copy_from_slice(&DIGIT_PAIRS[pair..pair + 2]);
        pos -= 2;
    }
    #[allow(clippy::cast_possible_truncation)]
    let rest = output as u8;
    if rest >= 10 {
        let pair = 2 * usize::from(rest);
        out.buf[pos - 2..pos].copy_from_slice(&DIGIT_PAIRS[pair..pair + 2]);
    } else {
        out.buf[pos - 1] = b'0' + rest;
    }
    out
}

/// `zend_dtoa` digits of a finite, positive, non-zero `abs` without heap
/// allocation: the shortest round-trip digits (`shortest`, mode 0) or the
/// value correctly rounded to `max(ndigits, 1)` digits (mode 2). `None`
/// when more than 17 digits are asked for.
///
/// Two shortcuts precede the exact formatter:
/// * an integral value below 10^15 has its own digits as both the shortest
///   form and, when it has at most `n` digits, the exact `n`-digit form;
/// * for a normal value and `n <= 15`, the shortest digits, when there are
///   at most `n` of them, ARE the `n`-digit rounding: they lie within half
///   an ulp of the value (< 1.2e-16 relative), closer than half a unit in
///   the 15th digit (>= 5e-16 relative), so they are the unique nearest
///   `n`-digit number. This avoids the exact formatter's bignum fallback
///   for short values such as `1.5` at `precision=14`.
/// * otherwise (normal value, `n <= 15`) the shortest digits rounded to
///   `n` digits are the answer unless they lie too close to a half (see
///   [`round_shortest`]); only then does the exact formatter run.
fn dtoa_small(abs: f64, shortest: bool, ndigits: i32) -> Option<SmallDigits> {
    debug_assert!(abs.is_finite() && abs > 0.0);
    let n = if shortest { 17 } else { ndigits.max(1) };
    if n > 17 {
        return None;
    }
    let max_count = usize::try_from(n).ok()?;
    if (1.0..1e15).contains(&abs) && abs.fract() == 0.0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let mut m = abs as u64;
        let mut tmp = [0u8; 16];
        let mut pos = tmp.len();
        while m > 0 {
            pos -= 1;
            #[allow(clippy::cast_possible_truncation)]
            let digit = (m % 10) as u8;
            tmp[pos] = b'0' + digit;
            m /= 10;
        }
        let count = tmp.len() - pos;
        if shortest || count <= max_count {
            let mut end = tmp.len();
            while end - pos > 1 && tmp[end - 1] == b'0' {
                end -= 1;
            }
            let mut out = SmallDigits {
                buf: [0; 24],
                len: end - pos,
                decpt: i32::try_from(count).ok()?,
            };
            out.buf[..end - pos].copy_from_slice(&tmp[pos..end]);
            return Some(out);
        }
    }
    if shortest {
        return Some(shortest_digits(abs));
    }
    if n <= 15 && abs >= f64::MIN_POSITIVE {
        let d = shortest_digits(abs);
        if d.len <= max_count {
            return Some(d);
        } else if let Some(r) = round_shortest(&d, max_count) {
            return Some(r);
        }
    }
    let mut w = StackBuf::<32>::new();
    let prec = max_count - 1;
    write!(w, "{abs:.prec$e}").ok()?;
    parse_exp_format(w.as_bytes())
}

/// Rounds the shortest round-trip digits `d` of a normal value `v` (more
/// than `n` of them) to `n` digits when that provably equals `v`'s own
/// correct rounding; `None` when the dropped digits lie too close to a
/// half (the exact formatter decides then).
///
/// `d` lies within half an ulp of `v`: `|v - d| <= 2^-53 v < 1.1103e-16 *
/// 10^decpt`, i.e. less than `1.1103e-16 * 10^len` units of `d`'s last
/// digit: under 12 units for 17 digits, 2 for 16 and 1 for fewer. With
/// the `k` dropped digits forming `D` and that error bound `M`, `2(D + M)
/// < 10^k` puts `v` strictly below the half between the two `n`-digit
/// neighbours and `2(D - M) > 10^k` strictly above it (and `M < 10^k / 2`
/// keeps `v` within half a unit of them), so the nearest `n`-digit number
/// is the same for `v` and `d` and no tie (half-even) can arise.
fn round_shortest(d: &SmallDigits, n: usize) -> Option<SmallDigits> {
    debug_assert!(d.len > n && d.len <= 17);
    let k = d.len - n;
    let dropped = d.buf[n..d.len]
        .iter()
        .fold(0u64, |acc, &c| acc * 10 + u64::from(c - b'0'));
    let pow = POW10[k];
    let margin: u64 = match d.len {
        17.. => 12,
        16 => 2,
        _ => 1,
    };
    let mut out = *d;
    out.len = n;
    if dropped > margin && 2 * (dropped - margin) > pow {
        // Round up, carrying through trailing nines.
        let mut i = n;
        loop {
            if i == 0 {
                // All nines: 0.999.. x 10^decpt becomes 0.1 x 10^(decpt+1).
                out.buf[0] = b'1';
                out.len = 1;
                out.decpt += 1;
                return Some(out);
            }
            i -= 1;
            if out.buf[i] == b'9' {
                out.len = i;
            } else {
                out.buf[i] += 1;
                out.len = i + 1;
                return Some(out);
            }
        }
    }
    if 2 * (dropped + margin) >= pow {
        return None;
    }
    while out.len > 1 && out.buf[out.len - 1] == b'0' {
        out.len -= 1;
    }
    Some(out)
}

/// The general (heap) digit generation behind [`zend_dtoa`], for any
/// number of digits.
fn dtoa_slow(abs: f64, shortest: bool, ndigits: i32) -> (String, i32) {
    if shortest {
        let d = shortest_digits(abs);
        return (d.digits().iter().map(|&c| char::from(c)).collect(), d.decpt);
    }
    let prec = usize::try_from(ndigits.max(1) - 1).unwrap_or(0);
    let formatted = format!("{abs:.prec$e}");
    let (mantissa, exp) = formatted.split_once('e').unwrap_or((&formatted, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let mut digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    (digits, exp + 1)
}

/// Port of `zend_dtoa` for finite values in mode 0 (`ndigits <= 0`, shortest
/// round-trip) or mode 2 (`ndigits` significant digits).
///
/// # Panics
/// Never for finite input; non-finite input yields digits `"Infinity"`/`"NaN"`
/// with `decpt == 9999` like the C implementation.
#[must_use]
pub fn zend_dtoa(value: f64, mode: u8, ndigits: i32) -> DtoaDigits {
    let negative = value.is_sign_negative();
    if !value.is_finite() {
        return DtoaDigits {
            digits: if value.is_nan() { "NaN" } else { "Infinity" }.to_string(),
            decpt: 9999,
            negative: negative && !value.is_nan(),
        };
    }
    let abs = value.abs();
    if abs == 0.0 {
        return DtoaDigits {
            digits: "0".to_string(),
            decpt: 1,
            negative,
        };
    }
    let shortest = mode == 0 || ndigits <= 0 && mode != 2;
    if let Some(d) = dtoa_small(abs, shortest, ndigits) {
        return DtoaDigits {
            digits: d.digits().iter().map(|&c| char::from(c)).collect(),
            decpt: d.decpt,
            negative,
        };
    }
    let (digits, decpt) = dtoa_slow(abs, shortest, ndigits);
    DtoaDigits {
        digits,
        decpt,
        negative,
    }
}

/// Writes a decimal exponent (`zend_gcvt`'s exponent digits).
fn put_exponent(out: &mut impl GcvtOut, mut e: u32) {
    let mut tmp = [0u8; 10];
    let mut pos = tmp.len();
    loop {
        pos -= 1;
        #[allow(clippy::cast_possible_truncation)]
        let digit = (e % 10) as u8;
        tmp[pos] = b'0' + digit;
        e /= 10;
        if e == 0 {
            break;
        }
    }
    out.put_all(&tmp[pos..]);
}

/// The layout half of `zend_gcvt`: digits and `decpt` to text.
fn emit_gcvt(
    out: &mut impl GcvtOut,
    digits: &[u8],
    decpt: i32,
    negative: bool,
    ndigit: i32,
    dec_point: u8,
    exponent: u8,
) {
    if negative {
        out.put(b'-');
    }
    let mut decpt = decpt;
    if (decpt >= 0 && decpt > ndigit) || decpt < -3 {
        // Exponential format, e.g. 1.2345E+13.
        decpt -= 1;
        let exp_negative = decpt < 0;
        let e = decpt.unsigned_abs();
        out.put(digits[0]);
        out.put(dec_point);
        if digits.len() == 1 {
            out.put(b'0');
        } else {
            out.put_all(&digits[1..]);
        }
        out.put(exponent);
        out.put(if exp_negative { b'-' } else { b'+' });
        put_exponent(out, e);
    } else if decpt < 0 {
        // Standard format 0.000ddd
        out.put(b'0');
        out.put(dec_point);
        for _ in decpt..0 {
            out.put(b'0');
        }
        out.put_all(digits);
    } else {
        // Standard format.
        let decpt_u = usize::try_from(decpt).unwrap_or(0);
        for i in 0..decpt_u {
            out.put(*digits.get(i).unwrap_or(&b'0'));
        }
        if digits.len() > decpt_u {
            if decpt_u == 0 {
                out.put(b'0');
            }
            out.put(dec_point);
            out.put_all(&digits[decpt_u..]);
        }
    }
}

/// `zend_gcvt` into `out` (see [`zend_gcvt`]).
fn gcvt_with(out: &mut impl GcvtOut, value: f64, ndigit: i32, dec_point: u8, exponent: u8) {
    let (mode, ndigit) = if ndigit >= 0 { (2, ndigit) } else { (0, 17) };
    if value.is_nan() {
        out.put_all(b"NAN");
        return;
    }
    if value.is_infinite() {
        out.put_all(if value < 0.0 {
            &b"-INF"[..]
        } else {
            &b"INF"[..]
        });
        return;
    }
    let negative = value.is_sign_negative();
    let abs = value.abs();
    if abs == 0.0 {
        emit_gcvt(out, b"0", 1, negative, ndigit, dec_point, exponent);
        return;
    }
    let shortest = mode == 0;
    if let Some(d) = dtoa_small(abs, shortest, ndigit) {
        emit_gcvt(
            out,
            d.digits(),
            d.decpt,
            negative,
            ndigit,
            dec_point,
            exponent,
        );
        return;
    }
    let (digits, decpt) = dtoa_slow(abs, shortest, ndigit);
    emit_gcvt(
        out,
        digits.as_bytes(),
        decpt,
        negative,
        ndigit,
        dec_point,
        exponent,
    );
}

/// Room for `zend_gcvt` output of at most 17 significant digits (at most
/// 24 bytes in exponential form, 36 in standard form) plus `".0"`.
const SMALL_OUT: usize = 64;

/// Port of `zend_gcvt(value, ndigit, dec_point, exponent_char, buf)`.
///
/// A negative `ndigit` selects mode 0 (shortest representation, `%.*H` with
/// precision `-1`), in which case the exponential-format threshold is 17
/// digits. `ndigit == 0` must be mapped to `1` by the caller, as PHP does.
#[must_use]
pub fn zend_gcvt(value: f64, ndigit: i32, dec_point: u8, exponent: u8) -> String {
    if ndigit <= 17 {
        let mut b = StackBuf::<SMALL_OUT>::new();
        gcvt_with(&mut b, value, ndigit, dec_point, exponent);
        return b.to_latin1_string();
    }
    let mut s = String::new();
    gcvt_with(&mut s, value, ndigit, dec_point, exponent);
    s
}

/// The digit count a `precision`/`serialize_precision` value gives
/// `zend_gcvt`: the setting is a `zend_long`, and php-src passes it on as
/// `(int) EG(precision)` / `(int) PG(serialize_precision)`, which keeps its
/// low 32 bits (`99999999999999999999` saturates to `i64::MAX`, which is
/// -1: the shortest form; `4294967296` is 0, printed as one digit).
/// Every negative count is the shortest form in `zend_gcvt` (mode 0), and
/// no double has more than 1000 significant decimal digits, so the result
/// is clamped to -1..=1000 without changing what is printed.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // C's `(int)` conversion, on purpose
pub fn precision_digits(precision: i64) -> i32 {
    (precision as i32).clamp(-1, 1000)
}

/// `smart_str_append_double(num, precision, zero_fraction)`: formats with
/// `zend_gcvt(num, precision ? precision : 1, '.', 'E')` and optionally appends
/// `".0"` to finite values that have no decimal point.
///
/// This is the formatting used by `var_export` (`serialize_precision`, zero
/// fraction), `print_r`/`echo`-style `precision` formatting (no zero fraction),
/// `serialize` and `json_encode`.
#[must_use]
pub fn format_double(num: f64, precision: i64, zero_fraction: bool) -> String {
    let p = precision_digits(precision);
    let mut s = zend_gcvt(num, if p == 0 { 1 } else { p }, b'.', b'E');
    if zero_fraction && num.is_finite() && !s.contains('.') {
        s.push_str(".0");
    }
    s
}

/// Stack room for the text of a float (`char buf[ZEND_DOUBLE_MAX_LENGTH]`),
/// filled by [`format_double_buf`].
#[derive(Debug)]
pub struct DoubleBuf(StackBuf<SMALL_OUT>);

impl DoubleBuf {
    /// An empty buffer.
    #[must_use]
    pub const fn new() -> Self {
        DoubleBuf(StackBuf::new())
    }
}

impl Default for DoubleBuf {
    fn default() -> Self {
        Self::new()
    }
}

/// [`format_double`] into a stack buffer, without heap allocation:
/// `None` when the precision asks for more than 17 digits (the text may
/// then not fit; use [`format_double`]).
pub fn format_double_buf(
    num: f64,
    precision: i64,
    zero_fraction: bool,
    buf: &mut DoubleBuf,
) -> Option<&[u8]> {
    let p = precision_digits(precision);
    let ndigit = if p == 0 { 1 } else { p };
    if ndigit > 17 {
        return None;
    }
    let b = &mut buf.0;
    b.len = 0;
    gcvt_with(b, num, ndigit, b'.', b'E');
    if zero_fraction && num.is_finite() && !b.as_bytes().contains(&b'.') {
        b.put_all(b".0");
    }
    Some(b.as_bytes())
}

/// [`format_double`] straight into a [`ZStr`] (`zend_double_to_str`:
/// `zend_gcvt` into a stack buffer, then one exactly sized string), with
/// no heap temporaries for precisions up to 17.
#[must_use]
pub fn format_double_zstr(num: f64, precision: i64, zero_fraction: bool) -> ZStr {
    // The text is ASCII: flagged `IS_STR_VALID_UTF8` like
    // `zend_double_to_str`'s result.
    let mut buf = DoubleBuf::new();
    match format_double_buf(num, precision, zero_fraction, &mut buf) {
        Some(text) => ZStr::from_ascii(text),
        None => ZStr::from_ascii(format_double(num, precision, zero_fraction).as_bytes()),
    }
}

/// `%.*H` formatting as used by `var_dump` / `debug_zval_dump`
/// (`php_printf("%.*H", serialize_precision, d)`).
///
/// Unlike [`format_double`], NaN/±Inf are printed by the printf engine as
/// `NAN`, `INF` and `-INF`, and a precision of 0 is treated as 1.
#[must_use]
pub fn format_double_h(num: f64, precision: i64) -> String {
    if num.is_nan() {
        return "NAN".to_string();
    }
    if num.is_infinite() {
        return if num > 0.0 { "INF" } else { "-INF" }.to_string();
    }
    let p = precision_digits(precision);
    zend_gcvt(num, if p == 0 { 1 } else { p }, b'.', b'E')
}

/// `zend_double_to_str` without the NaN warning: the string conversion of a
/// float under the given `precision` ini value (default 14).
#[must_use]
pub fn double_to_string(num: f64, precision: i64) -> String {
    format_double(num, precision, false)
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn shortest() {
        let cases: &[(f64, &str)] = &[
            (0.1 + 0.2, "0.30000000000000004"),
            (1e15, "1000000000000000"),
            (0.0, "0"),
            (-0.0, "-0"),
            (1.5, "1.5"),
            (100_000.0, "100000"),
            (1e17, "1.0E+17"),
            (123_456_789_012_345_678.0, "1.2345678901234568E+17"),
            (0.0001, "0.0001"),
            (0.00001, "1.0E-5"),
            (1.5e300, "1.5E+300"),
            (-1.25e-10, "-1.25E-10"),
        ];
        for &(v, want) in cases {
            assert_eq!(format_double_h(v, -1), want, "{v}");
        }
        assert_eq!(format_double_h(1e16, -1), "10000000000000000");
        assert_eq!(format_double_h(f64::NAN, -1), "NAN");
        assert_eq!(format_double_h(f64::NEG_INFINITY, -1), "-INF");
    }

    /// `precision`/`serialize_precision` reach `zend_gcvt` as C's `(int)`
    /// of the `zend_long` setting; values from PHP 8.5
    /// (`php -n -d precision=V -r 'echo 1/3;'`).
    #[test]
    fn precision_settings_convert_like_c_int() {
        let third = 1.0 / 3.0;
        for (precision, want) in [
            (i64::MAX, "0.3333333333333333"),
            (4_294_967_295, "0.3333333333333333"),
            (4_294_967_296, "0.3"),
            (4_294_967_297, "0.3"),
            (2_147_483_648, "0.3333333333333333"),
            (17, "0.33333333333333331"),
            (-1, "0.3333333333333333"),
        ] {
            assert_eq!(format_double(third, precision, false), want, "{precision}");
        }
        assert_eq!(format_double_h(0.1, i64::MAX), "0.1");
        assert_eq!(format_double_h(0.1, 17), "0.10000000000000001");
    }

    #[test]
    fn precision_14() {
        let cases: &[(f64, &str)] = &[
            (1e15, "1.0E+15"),
            (1e14, "1.0E+14"),
            (1e13, "10000000000000"),
            (0.1 + 0.2, "0.3"),
            (123_456_789_012_345.67, "1.2345678901235E+14"),
            (-0.0, "-0"),
            (1e100, "1.0E+100"),
            (0.00001, "1.0E-5"),
            (1.0 / 3.0, "0.33333333333333"),
            (f64::INFINITY, "INF"),
            (f64::NEG_INFINITY, "-INF"),
            (f64::NAN, "NAN"),
            (7.0, "7"),
        ];
        for &(v, want) in cases {
            assert_eq!(double_to_string(v, 14), want, "{v}");
        }
    }

    #[test]
    fn half_even_ties() {
        assert_eq!(double_to_string(0.125, 2), "0.12");
        assert_eq!(double_to_string(0.375, 2), "0.38");
        assert_eq!(double_to_string(0.25, 1), "0.2");
        assert_eq!(double_to_string(2.5, 1), "2");
        assert_eq!(double_to_string(3.5, 1), "4");
        assert_eq!(double_to_string(0.75, 1), "0.8");
        assert_eq!(double_to_string(2.5e-5, 2), "2.5E-5");
        assert_eq!(double_to_string(0.1, 17), "0.10000000000000001");
        assert_eq!(double_to_string(1.5, 0), "2");
    }

    #[test]
    fn zero_fraction() {
        assert_eq!(format_double(1.0, -1, true), "1.0");
        assert_eq!(format_double(0.1, -1, true), "0.1");
        assert_eq!(format_double(1e20, -1, true), "1.0E+20");
        assert_eq!(format_double(f64::INFINITY, -1, true), "INF");
        assert_eq!(format_double(-0.0, -1, true), "-0.0");
    }

    /// Values for the fast-path cross-checks: edge cases plus a
    /// deterministic pseudo-random sample of bit patterns and of short
    /// decimals.
    fn sample_values() -> Vec<f64> {
        let mut vals = vec![
            1.5,
            0.1,
            0.5,
            7.0,
            -7.0,
            1e14,
            1e15,
            1e15 - 1.0,
            999_999_999_999_999.0,
            123_456_789.0,
            1_200_000.0,
            0.1 + 0.2,
            1.0 / 3.0,
            2.5,
            0.125,
            0.375,
            1e-300,
            5e-324,
            f64::MIN_POSITIVE,
            f64::MAX,
            9_007_199_254_740_993.0,
            2.5e-5,
            99.995,
            0.000_123_45,
        ];
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for i in 0..20_000u32 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            vals.push(f64::from_bits(x));
            #[allow(clippy::cast_precision_loss)]
            let short = (x % 1_000_000) as f64 / f64::from(10u32.pow(i % 7));
            vals.push(short);
        }
        vals
    }

    #[test]
    fn small_digits_match_exact_digits() {
        for v in sample_values() {
            if !v.is_finite() || v == 0.0 {
                continue;
            }
            let a = v.abs();
            for nd in [-1, 0, 1, 2, 3, 6, 10, 14, 15, 16, 17] {
                let shortest = nd < 0;
                let small = dtoa_small(a, shortest, nd).expect("at most 17 digits");
                let (digits, decpt) = dtoa_slow(a, shortest, nd);
                assert_eq!(
                    (small.digits(), small.decpt),
                    (digits.as_bytes(), decpt),
                    "{v:e} ndigits {nd}"
                );
            }
            assert!(dtoa_small(a, false, 18).is_none());
        }
    }

    #[test]
    fn zstr_and_string_formatting_agree() {
        for v in sample_values().into_iter().chain([
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            0.0,
            -0.0,
        ]) {
            for p in [-1, 0, 1, 5, 14, 17, 18, 40] {
                for zf in [false, true] {
                    let s = format_double(v, p, zf);
                    let z = format_double_zstr(v, p, zf);
                    assert_eq!(z.as_bytes(), s.as_bytes(), "{v:e} precision {p}");
                }
            }
        }
        let mut buf = DoubleBuf::new();
        assert_eq!(
            format_double_buf(-1.5, 14, false, &mut buf),
            Some(&b"-1.5"[..])
        );
        assert_eq!(
            format_double_buf(2.0, -1, true, &mut buf),
            Some(&b"2.0"[..])
        );
        assert_eq!(format_double_buf(0.1, 18, false, &mut buf), None);
        assert_eq!(format_double_zstr(1.5, 14, false), "1.5");
        assert_eq!(format_double_zstr(-2.0, 14, false), "-2");
        assert_eq!(format_double_zstr(1e25, 14, false), "1.0E+25");
        assert_eq!(format_double_zstr(3.0, -1, true), "3.0");
        assert_eq!(zend_gcvt(1.5, 2, b',', b'e'), "1,5");
        assert_eq!(zend_gcvt(0.1, 20, b'.', b'E'), "0.10000000000000000555");
        let d = zend_dtoa(1.5, 2, 14);
        assert_eq!((d.digits.as_str(), d.decpt, d.negative), ("15", 1, false));
        let d = zend_dtoa(-0.1, 0, 0);
        assert_eq!((d.digits.as_str(), d.decpt, d.negative), ("1", 0, true));
    }

    /// `(string)` at `precision=14` and `5` where the shortest digits are
    /// longer than the precision (rounded from them, or by the exact
    /// formatter near a half). Expectations from `/usr/bin/php8.5 -n`.
    #[test]
    fn precision_rounding_of_long_shortest_digits() {
        let cases: [(f64, &str, &str); 9] = [
            (1.0 / 3.0, "0.33333333333333", "0.33333"),
            (2.0 / 3.0, "0.66666666666667", "0.66667"),
            (-2.0 / 3.0, "-0.66666666666667", "-0.66667"),
            (0.1 + 0.2, "0.3", "0.3"),
            (1e15 / 7.0, "1.4285714285714E+14", "1.4286E+14"),
            (99_999_999_999_999.95, "1.0E+14", "1.0E+14"),
            (123_456.789_012_345_67, "123456.78901235", "1.2346E+5"),
            (1.000_000_000_000_000_2, "1", "1"),
            (3.000_000_000_000_000_5e-6, "3.0E-6", "3.0E-6"),
        ];
        for (v, p14, p5) in cases {
            assert_eq!(double_to_string(v, 14), p14, "{v:e} at 14");
            assert_eq!(double_to_string(v, 5), p5, "{v:e} at 5");
        }
    }

    /// The rounding of the shortest digits agrees with the exact
    /// formatter for every precision up to 16 over a spread of values.
    #[test]
    #[allow(clippy::many_single_char_names)]
    fn rounded_shortest_digits_match_the_exact_formatter() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut checked = 0;
        let mut declined = 0;
        for i in 0..40_000u32 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let v = match i % 5 {
                0 => f64::from_bits((x >> 2) | 0x3000_0000_0000_0000),
                1 => f64::from(i) * 0.1 + 1.37,
                2 => f64::from(i) / 7.0,
                3 => f64::from_bits(x),
                _ => 1.0 / f64::from(i + 1),
            };
            if !v.is_normal() {
                continue;
            }
            let abs = v.abs();
            let d = shortest_digits(abs);
            for n in 1..=16usize {
                if d.len <= n {
                    continue;
                }
                let Some(r) = round_shortest(&d, n) else {
                    declined += 1;
                    continue;
                };
                let prec = n - 1;
                let exact = format!("{abs:.prec$e}");
                let e = parse_exp_format(exact.as_bytes()).unwrap();
                assert_eq!(
                    (r.digits(), r.decpt),
                    (e.digits(), e.decpt),
                    "{abs:e} to {n} digits"
                );
                checked += 1;
            }
        }
        assert!(checked > 200_000, "{checked}");
        assert!(declined < checked / 10, "{declined} of {checked}");
    }

    /// The previous shortest-digit path: Rust's `{:e}` text, re-parsed.
    /// It differs from `zend_dtoa` mode 0 only on exact ties between two
    /// shortest candidates, which Rust rounds up and PHP (and Ryu) to even.
    fn std_shortest(abs: f64) -> SmallDigits {
        parse_exp_format(format!("{abs:e}").as_bytes()).unwrap()
    }

    #[test]
    fn ryu_digits_match_the_std_formatter() {
        let mut vals = sample_values();
        for e in 0..2047u64 {
            for m in [0u64, 1, 2, 3, 0x000F_FFFF_FFFF_FFFF, 0x0008_0000_0000_0000] {
                vals.push(f64::from_bits((e << 52) | m));
            }
        }
        for i in 1..50_000u32 {
            vals.push(f64::from(i) / 1000.0);
            vals.push(1.0 / f64::from(i));
        }
        let mut x: u64 = 0x0123_4567_89AB_CDEF;
        for _ in 0..200_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            vals.push(f64::from_bits(x));
        }
        for v in vals {
            let abs = v.abs();
            if !abs.is_finite() || abs == 0.0 {
                continue;
            }
            let ours = shortest_digits(abs);
            let theirs = std_shortest(abs);
            if (ours.digits(), ours.decpt) == (theirs.digits(), theirs.decpt) {
                continue;
            }
            // A tie: same length and position, ours even, and the exact
            // value halfway between the two.
            let len = ours.len;
            assert_eq!((len, ours.decpt), (theirs.len, theirs.decpt), "{abs:e}");
            assert_eq!((ours.buf[len - 1] - b'0') % 2, 0, "{abs:e}");
            let exact = format!("{abs:.60e}");
            let mantissa: String = exact
                .split('e')
                .next()
                .unwrap()
                .chars()
                .filter(char::is_ascii_digit)
                .collect();
            let tail = &mantissa[len..];
            assert!(
                tail.starts_with('5') && tail[1..].bytes().all(|c| c == b'0'),
                "{abs:e}: {exact}"
            );
        }
    }

    /// Shortest-digit ties round to even like `zend_dtoa` mode 0.
    /// Expectations from `/usr/bin/php8.5 -n`.
    #[test]
    #[allow(clippy::excessive_precision)]
    fn shortest_ties_round_to_even() {
        assert_eq!(format_double_h(2f64.powi(-25), -1), "2.9802322387695312E-8");
        assert_eq!(
            format_double_h(562_949_953_421_312.25, -1),
            "562949953421312.2"
        );
        assert_eq!(
            format_double_h(1_125_899_906_842_624.2, -1),
            "1125899906842624.2"
        );
        assert_eq!(
            format_double(981_446_413_237_567.25, -1, true),
            "981446413237567.2"
        );
        assert_eq!(
            double_to_string(562_949_953_421_312.25, 17),
            "562949953421312.25"
        );
        assert_eq!(format_double_h(2f64.powi(-26), -1), "1.4901161193847656E-8");
        assert_eq!(format_double_h(2f64.powi(-60), -1), "8.673617379884035E-19");
    }
}
