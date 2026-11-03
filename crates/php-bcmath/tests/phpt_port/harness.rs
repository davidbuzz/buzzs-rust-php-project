//! A tiny emulation of the PHP features the ext/bcmath `.phpt` scripts use
//! (`echo`, `var_dump`, exceptions, zpp coercions, operators), so each script
//! can be transcribed to Rust and its output compared with the original
//! `--EXPECT--`/`--EXPECTF--` section.

use std::cmp::Ordering;
use std::fmt::Write as _;

use php_bcmath::{BcError, BcMath, BinaryOp, ErrorKind, Number, Operand, RoundingMode};

/// A PHP value as far as these tests need one.
#[derive(Clone, Debug)]
pub enum V {
    S(String),
    I(i64),
    F(f64),
    Null,
    Arr,
    Obj(&'static str),
    N(Number),
}

pub fn s(x: &str) -> V {
    V::S(x.to_string())
}

pub fn i(x: i64) -> V {
    V::I(x)
}

/// `new BcMath\Number($x)` for a known-good literal.
pub fn num(x: impl Into<V>) -> Number {
    let v = x.into();
    let op = match &v {
        V::S(s) => Operand::Str(s.as_bytes()),
        V::I(i) => Operand::Int(*i),
        _ => panic!("bad constructor arg"),
    };
    Number::new(op).expect("valid number")
}

pub fn nv(x: impl Into<V>) -> V {
    V::N(num(x))
}

impl From<&str> for V {
    fn from(x: &str) -> Self {
        s(x)
    }
}

impl From<i64> for V {
    fn from(x: i64) -> Self {
        V::I(x)
    }
}

impl From<Number> for V {
    fn from(x: Number) -> Self {
        V::N(x)
    }
}

impl V {
    /// String conversion (`echo`, interpolation, `(string)`).
    pub fn str(&self) -> String {
        match self {
            V::S(s) => s.clone(),
            V::I(i) => i.to_string(),
            V::F(f) => format!("{f}"),
            V::Null => String::new(),
            V::N(n) => n.value(),
            V::Arr => "Array".into(),
            V::Obj(_) => panic!("object to string"),
        }
    }

    /// `(int)` cast.
    pub fn int(&self) -> i64 {
        match self {
            V::I(i) => *i,
            V::S(s) => s.parse().expect("int string"),
            _ => panic!("int cast"),
        }
    }

    /// `zend_zval_value_name`.
    pub fn type_name(&self) -> &'static str {
        match self {
            V::S(_) => "string",
            V::I(_) => "int",
            V::F(_) => "float",
            V::Null => "null",
            V::Arr => "array",
            V::Obj(c) => c,
            V::N(_) => "BcMath\\Number",
        }
    }
}

/// Output buffer plus bcmath state.
#[derive(Default)]
pub struct Php {
    pub out: String,
    pub bc: BcMath,
}

pub fn class_of(e: &BcError) -> &'static str {
    match e.kind {
        ErrorKind::OutOfMemory => "OutOfMemory",
        k => k.class_name(),
    }
}

impl Php {
    pub fn echo(&mut self, s: impl AsRef<str>) {
        self.out.push_str(s.as_ref());
    }

    pub fn line(&mut self, s: impl AsRef<str>) {
        self.out.push_str(s.as_ref());
        self.out.push('\n');
    }

    /// `catch (Throwable $e) { echo $e::class, ': ', $e->getMessage(), "\n"; }`
    pub fn catch<T>(&mut self, r: Result<T, BcError>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                let _ = writeln!(self.out, "{}: {}", class_of(&e), e.message);
                None
            }
        }
    }

    /// `try { echo f(), "\n"; } catch ...`
    pub fn echo_or_catch(&mut self, r: Result<String, BcError>) {
        if let Some(v) = self.catch(r) {
            self.line(v);
        }
    }

    pub fn dump_str(&mut self, s: &str) {
        let _ = writeln!(self.out, "string({}) \"{s}\"", s.len());
    }

    pub fn dump_int(&mut self, v: i64) {
        let _ = writeln!(self.out, "int({v})");
    }

    pub fn dump_number(&mut self, n: &Number) {
        self.dump_number_indent(n, "");
    }

    pub fn dump_number_indent(&mut self, n: &Number, ind: &str) {
        let (value, scale) = n.debug_info();
        let _ = write!(
            self.out,
            "{ind}object(BcMath\\Number)#1 (2) {{\n{ind}  [\"value\"]=>\n{ind}  string({}) \"{value}\"\n{ind}  [\"scale\"]=>\n{ind}  int({scale})\n{ind}}}\n",
            value.len()
        );
    }

    /// `E_DEPRECATED` as displayed by the CLI.
    pub fn deprecated(&mut self, msg: &str) {
        let _ = writeln!(self.out, "\nDeprecated: {msg} in %FILE% on line %LINE%");
    }

    pub fn warning(&mut self, msg: &str) {
        let _ = writeln!(self.out, "\nWarning: {msg} in %FILE% on line %LINE%");
    }

    /// Weak-mode coercion of a `float` argument to `int`.
    fn float_to_int(&mut self, f: f64) -> i64 {
        if f.fract() != 0.0 {
            self.deprecated(&format!(
                "Implicit conversion from float {f} to int loses precision"
            ));
        }
        // Test values are small; truncation is the PHP semantics.
        #[allow(clippy::cast_possible_truncation)]
        let v = f as i64;
        v
    }

    /// ZPP for a `BcMath\Number|string|int` method parameter.
    pub fn zpp_operand<'a>(
        &mut self,
        func: &str,
        arg: u32,
        name: &str,
        v: &'a V,
    ) -> Result<Operand<'a>, BcError> {
        match v {
            V::S(s) => Ok(Operand::Str(s.as_bytes())),
            V::I(i) => Ok(Operand::Int(*i)),
            V::N(n) => Ok(Operand::Number(n)),
            V::F(f) => Ok(Operand::Int(self.float_to_int(*f))),
            V::Null => {
                self.deprecated(&format!(
                    "{func}(): Passing null to parameter #{arg} (${name}) of type BcMath\\Number|string|int is deprecated"
                ));
                Ok(Operand::Int(0))
            }
            V::Arr | V::Obj(_) => Err(BcError::number_argument_type(
                func,
                arg,
                name,
                v.type_name(),
            )),
        }
    }

    /// ZPP for a `?int $scale` parameter.
    pub fn zpp_scale(&mut self, func: &str, arg: u32, v: &V) -> Result<Option<i64>, BcError> {
        match v {
            V::I(i) => Ok(Some(*i)),
            V::Null => Ok(None),
            V::F(f) => Ok(Some(self.float_to_int(*f))),
            _ => Err(BcError::new(
                ErrorKind::TypeError,
                format!(
                    "{func}(): Argument #{arg} ($scale) must be of type ?int, {} given",
                    v.type_name()
                ),
            )),
        }
    }

    /// `$num->$method($arg[, $scale])` for the binary calc methods.
    pub fn method(
        &mut self,
        n: &Number,
        method: &str,
        arg: &V,
        scale: Option<&V>,
    ) -> Result<Number, BcError> {
        let func = format!("BcMath\\Number::{method}");
        let name = if method == "pow" { "exponent" } else { "num" };
        let op = self.zpp_operand(&func, 1, name, arg)?;
        let scale = match scale {
            Some(v) => self.zpp_scale(&func, 2, v)?,
            None => None,
        };
        match method {
            "add" => n.add(op, scale),
            "sub" => n.sub(op, scale),
            "mul" => n.mul(op, scale),
            "div" => n.div(op, scale),
            "mod" => n.modulo(op, scale),
            "pow" => n.pow(op, scale),
            _ => panic!("method {method}"),
        }
    }

    /// `$a <op> $b` with at least one `BcMath\Number` operand.
    pub fn binop(&mut self, op: BinaryOp, a: &V, b: &V) -> Result<Number, BcError> {
        let sym = op_symbol(op);
        let conv = |php: &mut Php, v: &V| -> Option<V> {
            match v {
                V::F(f) => Some(V::I(php.float_to_int(*f))),
                V::S(_) | V::I(_) | V::N(_) => Some(v.clone()),
                _ => None,
            }
        };
        let (Some(a2), Some(b2)) = (conv(self, a), conv(self, b)) else {
            return Err(BcError::new(
                ErrorKind::TypeError,
                format!(
                    "Unsupported operand types: {} {sym} {}",
                    a.type_name(),
                    b.type_name()
                ),
            ));
        };
        Number::do_operation(op, operand(&a2), operand(&b2))
    }

    /// `$a <=> $b` style comparison through the compare handler.
    pub fn cmp(a: &V, b: &V) -> Option<Ordering> {
        match (a, b) {
            (V::Null, V::N(_)) => Some(Ordering::Less),
            (V::N(_), V::Null) => Some(Ordering::Greater),
            _ => match (try_operand(a), try_operand(b)) {
                (Some(x), Some(y)) => Number::compare_operands(x, y).expect("compare"),
                _ => None,
            },
        }
    }
}

pub fn op_symbol(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::Pow => "**",
    }
}

pub fn operand(v: &V) -> Operand<'_> {
    try_operand(v).expect("operand")
}

pub fn try_operand(v: &V) -> Option<Operand<'_>> {
    match v {
        V::S(s) => Some(Operand::Str(s.as_bytes())),
        V::I(i) => Some(Operand::Int(*i)),
        V::N(n) => Some(Operand::Number(n)),
        _ => None,
    }
}

/// PHP comparison operators on an optional ordering (`None` = uncomparable).
pub fn eq(o: Option<Ordering>) -> bool {
    o == Some(Ordering::Equal)
}
pub fn le(o: Option<Ordering>) -> bool {
    matches!(o, Some(Ordering::Less | Ordering::Equal))
}
pub fn lt(o: Option<Ordering>) -> bool {
    o == Some(Ordering::Less)
}

pub fn tf(b: bool) -> &'static str {
    if b {
        "true"
    } else {
        "false"
    }
}

/// `str_pad($s, $n, ' ', STR_PAD_LEFT)`.
pub fn pad_left(s: &str, n: usize) -> String {
    format!("{s:>n$}")
}

/// `str_pad($s, $n)` (right padding).
pub fn pad_right(s: &str, n: usize) -> String {
    format!("{s:<n$}")
}

pub fn modes() -> [RoundingMode; 8] {
    RoundingMode::ALL
}

/// The `$scale` argument of a bc function in tests (always given).
#[allow(clippy::unnecessary_wraps)]
pub fn sc(v: i64) -> Option<i64> {
    Some(v)
}

/// `bcFUNC($a, $b, $scale)` by name.
pub fn bc_binary(
    bc: BcMath,
    func: &str,
    a: &str,
    b: &str,
    scale: Option<i64>,
) -> Result<String, BcError> {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    match func {
        "bcadd" => bc.bcadd(a, b, scale),
        "bcsub" => bc.bcsub(a, b, scale),
        "bcmul" => bc.bcmul(a, b, scale),
        "bcdiv" => bc.bcdiv(a, b, scale),
        "bcmod" => bc.bcmod(a, b, scale),
        "bcpow" => bc.bcpow(a, b, scale),
        _ => panic!("unknown function {func}"),
    }
}
