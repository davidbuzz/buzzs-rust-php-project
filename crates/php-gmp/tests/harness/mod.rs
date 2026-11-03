//! A tiny dispatcher that evaluates `gmp_*` calls the way the VM would
//! (argument conversion in order, then the function), rendering results in a
//! compact form so `.phpt` expectations can be transcribed directly.

#![allow(dead_code)]

use php_gmp::php::{self, GmpError, GmpRandom, Num, StrOrInt};
use php_gmp::Mpz;

/// A PHP value passed to a GMP function.
#[derive(Debug, Clone)]
pub enum V {
    /// int
    I(i64),
    /// string
    S(&'static str),
    /// raw byte string
    B(Vec<u8>),
    /// GMP object
    G(Mpz),
    /// any other type, by `zend_zval_value_name()`
    T(&'static str),
    /// bool
    Bool(bool),
}

pub use V::{Bool, B, G, I, S, T};

/// `gmp_init()` shorthand.
pub fn g(s: &str) -> V {
    G(Mpz::parse(s.as_bytes(), 10).expect("valid literal"))
}

fn num(v: &V, pos: u32) -> Result<std::borrow::Cow<'_, Mpz>, GmpError> {
    match v {
        G(m) => php::arg(Num::Gmp(m), pos),
        I(i) => php::arg(Num::Int(*i), pos),
        S(s) => php::arg(Num::Str(s.as_bytes()), pos),
        B(b) => php::arg(Num::Str(b), pos),
        Bool(b) => php::arg(Num::Int(i64::from(*b)), pos),
        T(t) => Err(GmpError::arg_type(pos, t)),
    }
}

fn int(v: &V) -> i64 {
    match v {
        I(i) => *i,
        Bool(b) => i64::from(*b),
        other => panic!("expected int, got {other:?}"),
    }
}

fn err(func: &str, e: &GmpError) -> String {
    format!("{}: {}", e.class.name(), e.message(func))
}

/// Calls `func` with `args`, returning a rendering of the result:
/// GMP objects as their decimal value, ints as `int(n)`, bools as
/// `bool(b)`, strings as `"..."`, pairs as `[a, b]`, errors as
/// `Class: message`.
#[allow(clippy::too_many_lines)]
pub fn call(func: &str, args: &[V]) -> String {
    let r: Result<String, GmpError> = (|| {
        let a = |i: usize| num(&args[i], u32::try_from(i + 1).unwrap_or(0));
        let opt = |i: usize, d: i64| args.get(i).map_or(d, int);
        let z = |m: Mpz| m.to_string();
        let pair = |(x, y): (Mpz, Mpz)| format!("[{x}, {y}]");
        Ok(match func {
            "gmp_init" | "GMP::__construct" => {
                let n = match &args[0] {
                    I(i) => StrOrInt::Int(*i),
                    S(s) => StrOrInt::Str(s.as_bytes()),
                    other => panic!("bad init arg {other:?}"),
                };
                z(php::gmp_init(n, opt(1, 0))?)
            }
            "gmp_strval" => {
                let n = a(0)?;
                format!("\"{}\"", php::gmp_strval(&n, opt(1, 10))?)
            }
            "gmp_intval" => format!("int({})", php::gmp_intval(&*a(0)?)),
            "gmp_add" => {
                let (x, y) = (a(0)?, a(1)?);
                z(php::gmp_add(&x, &y))
            }
            "gmp_sub" => {
                let (x, y) = (a(0)?, a(1)?);
                z(php::gmp_sub(&x, &y))
            }
            "gmp_mul" => {
                let (x, y) = (a(0)?, a(1)?);
                z(php::gmp_mul(&x, &y))
            }
            "gmp_div_qr" => {
                let (x, y) = (a(0)?, a(1)?);
                pair(php::gmp_div_qr(&x, &y, opt(2, 0))?)
            }
            "gmp_div_q" | "gmp_div" => {
                let (x, y) = (a(0)?, a(1)?);
                z(php::gmp_div_q(&x, &y, opt(2, 0))?)
            }
            "gmp_div_r" => {
                let (x, y) = (a(0)?, a(1)?);
                z(php::gmp_div_r(&x, &y, opt(2, 0))?)
            }
            "gmp_mod" => {
                let (x, y) = (a(0)?, a(1)?);
                z(php::gmp_mod(&x, &y)?)
            }
            "gmp_divexact" => {
                let (x, y) = (a(0)?, a(1)?);
                z(php::gmp_divexact(&x, &y)?)
            }
            "gmp_neg" => z(php::gmp_neg(&*a(0)?)),
            "gmp_abs" => z(php::gmp_abs(&*a(0)?)),
            "gmp_com" => z(php::gmp_com(&*a(0)?)),
            "gmp_nextprime" => z(php::gmp_nextprime(&*a(0)?)),
            "gmp_prevprime" => {
                let (p, d) = php::gmp_prevprime(&*a(0)?)?;
                format!("{p} definitely={d}")
            }
            "gmp_fact" => z(php::gmp_fact(&*a(0)?)?),
            "gmp_binomial" => {
                let x = a(0)?;
                z(php::gmp_binomial(&x, int(&args[1]))?)
            }
            "gmp_pow" => {
                let x = a(0)?;
                z(php::gmp_pow(&x, int(&args[1]))?)
            }
            "gmp_powm" => {
                let (x, y, m) = (a(0)?, a(1)?, a(2)?);
                z(php::gmp_powm(&x, &y, &m)?)
            }
            "gmp_powm_sec" => {
                let (x, y, m) = (a(0)?, a(1)?, a(2)?);
                z(php::gmp_powm_sec(&x, &y, &m)?)
            }
            "gmp_sqrt" => z(php::gmp_sqrt(&*a(0)?)?),
            "gmp_sqrtrem" => pair(php::gmp_sqrtrem(&*a(0)?)?),
            "gmp_root" => {
                let x = a(0)?;
                z(php::gmp_root(&x, int(&args[1]))?)
            }
            "gmp_rootrem" => {
                let x = a(0)?;
                pair(php::gmp_rootrem(&x, int(&args[1]))?)
            }
            "gmp_perfect_square" => format!("bool({})", php::gmp_perfect_square(&*a(0)?)),
            "gmp_perfect_power" => format!("bool({})", php::gmp_perfect_power(&*a(0)?)),
            "gmp_prob_prime" => {
                let x = a(0)?;
                format!("int({})", php::gmp_prob_prime(&x, opt(1, 10)))
            }
            "gmp_gcd" => {
                let (x, y) = (a(0)?, a(1)?);
                z(php::gmp_gcd(&x, &y))
            }
            "gmp_gcdext" => {
                let (x, y) = (a(0)?, a(1)?);
                let (g, s, t) = php::gmp_gcdext(&x, &y);
                format!("g={g} s={s} t={t}")
            }
            "gmp_lcm" => {
                let (x, y) = (a(0)?, a(1)?);
                z(php::gmp_lcm(&x, &y))
            }
            "gmp_invert" => {
                let (x, y) = (a(0)?, a(1)?);
                php::gmp_invert(&x, &y)?.map_or_else(|| "bool(false)".to_string(), z)
            }
            "gmp_jacobi" | "gmp_legendre" | "gmp_kronecker" => {
                let (x, y) = (a(0)?, a(1)?);
                let r = match func {
                    "gmp_jacobi" => php::gmp_jacobi(&x, &y),
                    "gmp_legendre" => php::gmp_legendre(&x, &y),
                    _ => php::gmp_kronecker(&x, &y),
                };
                format!("int({r})")
            }
            "gmp_cmp" => {
                let (x, y) = (a(0)?, a(1)?);
                format!("int({})", php::gmp_cmp(&x, &y))
            }
            "gmp_sign" => format!("int({})", php::gmp_sign(&*a(0)?)),
            "gmp_and" => {
                let (x, y) = (a(0)?, a(1)?);
                z(php::gmp_and(&x, &y))
            }
            "gmp_or" => {
                let (x, y) = (a(0)?, a(1)?);
                z(php::gmp_or(&x, &y))
            }
            "gmp_xor" => {
                let (x, y) = (a(0)?, a(1)?);
                z(php::gmp_xor(&x, &y))
            }
            "gmp_testbit" => {
                let x = a(0)?;
                format!("bool({})", php::gmp_testbit(&x, int(&args[1]))?)
            }
            "gmp_scan0" => {
                let x = a(0)?;
                format!("int({})", php::gmp_scan0(&x, int(&args[1]))?)
            }
            "gmp_scan1" => {
                let x = a(0)?;
                format!("int({})", php::gmp_scan1(&x, int(&args[1]))?)
            }
            "gmp_popcount" => format!("int({})", php::gmp_popcount(&*a(0)?)),
            "gmp_hamdist" => {
                let (x, y) = (a(0)?, a(1)?);
                format!("int({})", php::gmp_hamdist(&x, &y))
            }
            "gmp_export" => {
                let x = a(0)?;
                let bytes =
                    php::gmp_export(&x, opt(1, 1), opt(2, php::DEFAULT_IMPORT_EXPORT_FLAGS))?;
                format!(
                    "hex({})",
                    bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
                )
            }
            "gmp_import" => {
                let data: &[u8] = match &args[0] {
                    S(s) => s.as_bytes(),
                    B(b) => b,
                    other => panic!("bad import data {other:?}"),
                };
                z(php::gmp_import(
                    data,
                    opt(1, 1),
                    opt(2, php::DEFAULT_IMPORT_EXPORT_FLAGS),
                )?)
            }
            other => panic!("unsupported function {other}"),
        })
    })();
    r.unwrap_or_else(|e| err(func, &e))
}

/// `gmp_setbit` / `gmp_clrbit` on a value, rendering the new value or the error.
pub fn setbit(func: &str, n: &mut Mpz, index: i64, value: bool) -> String {
    let r = if func == "gmp_clrbit" {
        php::gmp_clrbit(n, index)
    } else {
        php::gmp_setbit(n, index, value)
    };
    match r {
        Ok(()) => n.to_string(),
        Err(e) => err(func, &e),
    }
}

/// Runs a seeded random sequence like `gmp_random_seed` + calls.
pub fn random() -> GmpRandom {
    GmpRandom::new()
}
