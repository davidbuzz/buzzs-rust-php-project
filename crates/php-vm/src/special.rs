// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Handlers of the opcodes that `zend_try_compile_special_func()` compiles
//! builtin calls to (ref: `Zend/zend_compile.c` `zend_compile_func_strlen`,
//! `_count`, `_typecheck`, `_gettype`, `_defined`, `_in_array`,
//! `_array_key_exists`; `Zend/zend_vm_def.h` `ZEND_STRLEN`, `ZEND_COUNT`,
//! `ZEND_TYPE_CHECK`, `ZEND_GET_TYPE`, `ZEND_DEFINED`, `ZEND_IN_ARRAY`,
//! `ZEND_ARRAY_KEY_EXISTS`; `Zend/zend_execute.c` `zend_quick_check_constant`,
//! `zend_array_key_exists_fast`, `zend_array_key_exists_error`).
//!
//! The opcodes run in the caller's frame: no builtin frame is pushed, so
//! a `TypeError` they raise has no `strlen()` line in its trace, exactly as
//! in php-src. The common operand types are handled inline by the run
//! loop; the rest comes here.
//!
//! Also here: frameless internal calls (ref: `Zend/zend_frameless_function.h`,
//! `ZEND_FRAMELESS_ICALL_n` in `Zend/zend_vm_def.h`), which call a handler
//! without pushing its call frame.

use crate::call::forget_unallocated;
use crate::compiler::SPECIAL_BUILTINS;
use crate::func::{flags, Args, Builtin, Func};
use crate::object;
use crate::opcode::{FramelessCall, Op, Operand, PlaceId};
use crate::typecheck::given_type;
use crate::vm::{PendingCall, Vm, VmResult};
use php_runtime::{Value, ZArray, ZStr};
use php_stdstr::transform::TrimMode;
use std::rc::Rc;

impl Vm {
    /// [`crate::compiler::CompileOptions::special_builtins`] for the
    /// current function table: the special builtins registered as
    /// (non-deprecated) internal functions. A disabled function redeclared
    /// in user code keeps its calls, and so does everything under
    /// [`Vm::compile_no_builtins`] (`ZEND_COMPILE_NO_BUILTINS`).
    pub(crate) fn special_builtins(&self) -> u32 {
        if self.compile_no_builtins {
            return 0;
        }
        let mut mask = 0;
        for (i, name) in SPECIAL_BUILTINS.iter().enumerate() {
            let internal = matches!(
                self.functions.get(name.as_bytes()),
                Some(Func::Builtin(b)) if b.flags & (flags::DEPRECATED | flags::NODISCARD) == 0
            );
            if internal {
                mask |= 1 << i;
            }
        }
        mask
    }

    /// `ZEND_STRLEN` for an operand that is not a string: weak mode
    /// coerces scalars and `Stringable` objects (`zend_parse_arg_str_weak`)
    /// and deprecates `null`; anything else is a `TypeError`.
    #[cold]
    #[inline(never)]
    pub(crate) fn op_strlen_slow(&mut self, v: Value) -> VmResult<Value> {
        let mut v = match v {
            Value::Reference(r) => r.get(),
            other => other,
        };
        if let Value::Str(s) = &v {
            return Ok(Value::Int(i64::try_from(s.len()).unwrap_or(i64::MAX)));
        }
        if !self.frame().code.strict_types {
            if v.is_null() {
                self.deprecated(
                    "strlen(): Passing null to parameter #1 ($string) of type string is deprecated",
                )?;
                return Ok(Value::Int(0));
            }
            // Leaves `v` as it was when it cannot be converted.
            if self.weak_scalar_coerce(crate::types::mask::STRING, &mut v)? {
                if let Value::Str(s) = &v {
                    return Ok(Value::Int(i64::try_from(s.len()).unwrap_or(i64::MAX)));
                }
            }
        }
        Err(self.make_throw(
            "TypeError",
            &format!(
                "strlen(): Argument #1 ($string) must be of type string, {} given",
                given_type(&v)
            ),
        ))
    }

    /// `ZEND_COUNT` for an operand that is not an array. Objects run the
    /// `count()` builtin (`count_elements` handlers and `Countable`);
    /// anything else is a `TypeError`.
    #[cold]
    #[inline(never)]
    pub(crate) fn op_count_slow(&mut self, v: Value, sizeof: bool) -> VmResult<Value> {
        let name = if sizeof { "sizeof" } else { "count" };
        let v = match v {
            Value::Reference(r) => r.get(),
            other => other,
        };
        match v {
            Value::Array(a) => Ok(Value::Int(i64::try_from(a.len()).unwrap_or(i64::MAX))),
            v @ Value::Object(_) => self.special_func_fallback(name, v),
            v => Err(self.make_throw(
                "TypeError",
                &format!(
                    "{name}(): Argument #1 ($value) must be of type Countable|array, {} given",
                    given_type(&v)
                ),
            )),
        }
    }

    /// `ZEND_GET_CLASS`: the class name of object `a`, or with no operand
    /// the running function's scope (deprecated).
    #[cold]
    #[inline(never)]
    pub(crate) fn op_get_class(&mut self, a: Operand) -> VmResult<Value> {
        if matches!(a, Operand::Unused) {
            let Some(scope) = self.frame().scope.clone() else {
                return Err(self.make_throw(
                    "Error",
                    "get_class() without arguments must be called from within a class",
                ));
            };
            self.deprecated("Calling get_class() without arguments is deprecated")?;
            return Ok(Value::Str(scope.name.clone()));
        }
        match self.read(a)? {
            Value::Object(o) => Ok(Value::Str(object::class_of(&o).name.clone())),
            v => Err(self.make_throw(
                "TypeError",
                &format!(
                    "get_class(): Argument #1 ($object) must be of type object, {} given",
                    given_type(&v)
                ),
            )),
        }
    }

    /// `ZEND_GET_CALLED_CLASS`: the running function's called scope.
    #[cold]
    #[inline(never)]
    pub(crate) fn op_get_called_class(&mut self) -> VmResult<Value> {
        match self.frame().called.clone() {
            Some(called) => Ok(Value::Str(called.name.clone())),
            None => Err(self.make_throw(
                "Error",
                "get_called_class() must be called from within a class",
            )),
        }
    }

    /// `ZEND_DEFINED`: whether the constant named by literal `name` exists.
    /// Like `zend_quick_check_constant`, only the exact name is looked up:
    /// `true`/`false`/`null` were resolved at compile time, and
    /// `__COMPILER_HALT_OFFSET__` is registered under a mangled name.
    /// The answer is kept in the op array's run-time cache
    /// ([`Vm::defined_hit`], as php-src's `CACHED_PTR`), which the tape's
    /// row for the op reads without coming here.
    #[inline]
    pub(crate) fn op_defined(&mut self, name: u32) -> VmResult<bool> {
        if let Some(r) = self.defined_hit(name) {
            return Ok(r);
        }
        if let Value::Str(n) = &self.frame().code.literals[name as usize] {
            // `zend_quick_check_constant()`: the name as written, then the
            // literal with its namespace part lowercased; the table keeps
            // a `const`'s declared spelling, so a namespaced miss compares
            // the namespace ignoring case (one lookup with `constant()`).
            let r = self.find_global_constant(n.as_bytes()).is_some();
            self.defined_fill(name, n, r);
            return Ok(r);
        }
        self.op_defined_slow(name)
    }

    /// `defined()` of a number literal (`defined(1.5)`): the name is
    /// `zval_get_string` of the literal, formatted with the `precision`
    /// setting in effect.
    #[cold]
    #[inline(never)]
    fn op_defined_slow(&mut self, name: u32) -> VmResult<bool> {
        let v = self.lit(name);
        let n = self.to_zstr(&v)?;
        Ok(self.constants.contains_key(&n))
    }

    /// `ZEND_IN_ARRAY`: whether `needle` is one of the keys of the literal
    /// `set`. A string needle is looked up as a string key and, in strict
    /// mode, an integer as an integer key; in loose mode `null` and `false`
    /// equal only the empty string, and any other needle is compared with
    /// `==` against each key in order.
    pub(crate) fn op_in_array(&mut self, needle: &Value, set: u32, strict: bool) -> VmResult<bool> {
        let keys = {
            let Value::Array(keys) = &self.frame().code.literals[set as usize] else {
                unreachable!("the haystack literal of ZEND_IN_ARRAY is an array");
            };
            match needle {
                Value::Str(s) => return Ok(keys.get_str(s.as_bytes()).is_some()),
                Value::Int(i) if strict => return Ok(keys.get_int(*i).is_some()),
                _ if strict => return Ok(false),
                Value::Null | Value::Bool(false) => return Ok(keys.get_str(b"").is_some()),
                _ => keys.clone(),
            }
        };
        self.in_array_compare(needle, &keys)
    }

    /// The loose-comparison loop of `ZEND_IN_ARRAY`: `zend_compare` of the
    /// needle with each string key, in order, until the first match.
    #[cold]
    #[inline(never)]
    fn in_array_compare(&mut self, needle: &Value, keys: &ZArray) -> VmResult<bool> {
        for key in keys.keys() {
            let key = key.to_value();
            if php_runtime::ops::is_equal(needle, &key, self).map_err(|e| self.rt_throw(e))? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `array_key_exists($key, $GLOBALS)` at its `ZEND_FETCH_GLOBALS`: when
    /// the op after the read of `$GLOBALS` (place `place`, into temporary
    /// `dst`) is the `ZEND_ARRAY_KEY_EXISTS` that consumes it and its key is
    /// a string or an int, the answer is the symbol table's
    /// (`Vm::fetch_global_by_key_optional`: a variable that is set, null
    /// included) and both ops are done: the caller skips the second. The
    /// copy of the whole table php-src makes for the read
    /// (`zend_proptable_to_symtable`) is what MediaWiki's
    /// `GlobalVarConfig::has()` paid on every call. `false`, nothing
    /// changed, for anything else (the key's diagnostics are the op's).
    pub(crate) fn globals_key_exists_ahead(
        &mut self,
        place: PlaceId,
        dst: u32,
        next: Option<&Op>,
    ) -> bool {
        let Some(&Op::KeyExists {
            key,
            array: Operand::Tmp(t),
            dst: result,
        }) = next
        else {
            return false;
        };
        if t != dst {
            return false;
        }
        let f = self.frame();
        let bare_globals = f.code.places.get(place as usize).is_some_and(|p| {
            matches!(p.base, crate::opcode::PlaceBase::GlobalsArray) && p.segs.is_empty()
        });
        if !bare_globals {
            return false;
        }
        let k = match key {
            Operand::Cv(cv) => f
                .cvs
                .get(cv as usize)
                .and_then(crate::vm::CvSlot::get)
                .cloned(),
            Operand::Tmp(x) | Operand::TmpKeep(x) => f.tmps.get(x as usize).cloned(),
            Operand::Lit(l) => f.code.literals.get(l as usize).cloned(),
            Operand::Unused => None,
        };
        let Some(k @ (Value::Str(_) | Value::Int(_))) = k else {
            return false;
        };
        let found = self.fetch_global_by_key_optional(&k).is_some();
        if let Operand::Tmp(x) = key {
            self.set_tmp(x, Value::Null);
        }
        self.set_dst(result, Value::Bool(found));
        true
    }

    /// `ZEND_ARRAY_KEY_EXISTS`: both operands are read without the
    /// undefined-variable warning first (`GET_OP*_ZVAL_PTR_UNDEF`); the
    /// warnings come from the lookup or the type error, in php-src's order.
    pub(crate) fn op_key_exists(&mut self, key: Operand, array: Operand) -> VmResult<bool> {
        let k = self.read_quiet(key)?;
        let subject = self.read_quiet(array)?;
        match subject {
            Some(Value::Array(a)) => match k {
                Some(Value::Str(s)) => Ok(a.get_sym(s.as_bytes()).is_some()),
                Some(Value::Int(i)) => Ok(a.get_int(i).is_some()),
                k => self.key_exists_slow(key, k, &a),
            },
            subject => self.key_exists_error(key, k.is_none(), array, subject.as_ref()),
        }
    }

    /// `zend_array_key_exists_fast` for the key types the fast path leaves
    /// out: an undefined variable (warning, then `""`), `null` (deprecated,
    /// then `""`), floats (converted with their diagnostics while the array
    /// is kept alive), booleans, resources (warning) and arrays or objects
    /// (`zend_illegal_array_offset_access`).
    #[cold]
    #[inline(never)]
    fn key_exists_slow(
        &mut self,
        key_op: Operand,
        key: Option<Value>,
        a: &ZArray,
    ) -> VmResult<bool> {
        match key {
            None => {
                self.warn_undefined_cv(key_op)?;
                Ok(a.get_str(b"").is_some())
            }
            Some(v @ (Value::Array(_) | Value::Object(_))) => Err(self.make_throw(
                "TypeError",
                &format!("Cannot access offset of type {} on array", given_type(&v)),
            )),
            Some(v @ Value::Float(_)) => {
                // php-src addrefs the array while the conversion warns: a
                // handler that drops the last other owner makes the lookup
                // fail (`GC_DELREF` reaching zero destroys the array).
                let had_owner = a.refcount() > 1;
                let found = php_stdarr::basic::array_key_exists(self, "array_key_exists", &v, a)
                    .map_err(|e| self.rt_throw(e))?;
                Ok(found && !(had_owner && a.refcount() == 1))
            }
            Some(v) => php_stdarr::basic::array_key_exists(self, "array_key_exists", &v, a)
                .map_err(|e| self.rt_throw(e)),
        }
    }

    /// `zend_array_key_exists_error`: the undefined-variable warnings for
    /// the key and then the subject, then the `TypeError` (which an
    /// exception thrown by a warning handler pre-empts).
    #[cold]
    #[inline(never)]
    fn key_exists_error(
        &mut self,
        key_op: Operand,
        key_undefined: bool,
        array_op: Operand,
        subject: Option<&Value>,
    ) -> VmResult<bool> {
        if key_undefined {
            self.warn_undefined_cv(key_op)?;
        }
        if subject.is_none() {
            self.warn_undefined_cv(array_op)?;
        }
        let given = subject.map_or_else(|| "null".to_string(), given_type);
        Err(self.make_throw(
            "TypeError",
            &format!(
                "array_key_exists(): Argument #2 ($array) must be of type array, {given} given"
            ),
        ))
    }

    /// Reads an operand without the undefined-variable warning; `None` is
    /// an undefined variable (`GET_OP1_ZVAL_PTR_UNDEF`).
    fn read_quiet(&mut self, op: Operand) -> VmResult<Option<Value>> {
        match op {
            Operand::Cv(cv) => Ok(self.read_cv_defined(cv)),
            other => self.read(other).map(Some),
        }
    }

    /// `ZVAL_UNDEFINED_OP*()` for a compiled-variable operand.
    fn warn_undefined_cv(&mut self, op: Operand) -> VmResult<()> {
        if let Operand::Cv(cv) = op {
            let name = self.frame().code.cv_names[cv as usize].clone();
            self.warning(&crate::exec::undefined_var_msg(&name))?;
        }
        Ok(())
    }

    /// Runs special builtin `name` as a regular internal call with
    /// argument `v` (the operand kinds an opcode leaves to the builtin).
    /// Like the opcode, the call is hidden from observers.
    #[cold]
    #[inline(never)]
    fn special_func_fallback(&mut self, name: &str, v: Value) -> VmResult<Value> {
        let Some(Func::Builtin(b)) = self.functions.get(name.as_bytes()).cloned() else {
            return Err(self.make_throw("Error", &format!("Call to undefined function {name}()")));
        };
        let mut call = PendingCall::new(Func::Builtin(b.clone()));
        call.args.push(Some(v));
        call.direct = true;
        let strict = self.frame().code.strict_types;
        if self.observer.enabled {
            self.observer.quiet_builtin = true;
        }
        self.call_builtin(&b, call, strict)
    }
}

/// The frameless-call classes, kept in bits 27..=31 of a builtin
/// function's flags ([`crate::func::flags::FRAMELESS`]): php-src's
/// frameless functions (`ZEND_FRAMELESS_FUNCTION` in ext/standard) plus
/// `abs()`, `chr()`, `ord()` and the math functions. Each class names the
/// argument shapes for which its handler has no diagnostic, exception,
/// callback, frame inspection or immediate memory-limit fatal (see
/// [`frameless_args`]); for those shapes no frame can be observed, so
/// whether php-src has a frameless variant does not matter.
mod fl {
    /// `abs(int|float)`.
    pub(super) const ABS: u32 = 1;
    /// `dechex(int)`.
    pub(super) const DECHEX: u32 = 2;
    /// `strtolower(string)`, `strtoupper(string)`.
    pub(super) const CASE: u32 = 3;
    /// `trim()`, `ltrim()`, `rtrim()` of a string, with a character list
    /// without a `..` range (`php_charmask()` warns about invalid ones).
    pub(super) const TRIM: u32 = 4;
    /// `dirname(string[, int])` with at least one level (else a
    /// `ValueError`).
    pub(super) const DIRNAME: u32 = 5;
    /// `is_numeric()` of a scalar.
    pub(super) const IS_NUMERIC: u32 = 6;
    /// `ord()` of a one-byte string (master deprecates other lengths).
    pub(super) const ORD: u32 = 7;
    /// `chr()` of 0..=255 (master deprecates other values).
    pub(super) const CHR: u32 = 8;
    /// `strpos(string, string[, int])` with an offset inside the haystack
    /// (else a `ValueError`).
    pub(super) const STRPOS: u32 = 9;
    /// `str_contains()`, `str_starts_with()`, `str_ends_with()`.
    pub(super) const STR_TEST: u32 = 10;
    /// `max()`, `min()` of two scalars.
    pub(super) const MIN_MAX: u32 = 11;
    /// `substr(string, int[, ?int])`.
    pub(super) const SUBSTR: u32 = 12;
    /// `strstr(string, string[, bool])`.
    pub(super) const STRSTR: u32 = 13;
    /// `in_array()` of a scalar in an array, strict, or loose over a short
    /// haystack without objects or resources (whose comparisons can warn
    /// or call `__toString()`).
    pub(super) const IN_ARRAY: u32 = 14;
    /// `str_replace(string, string, string)` with a replacement no longer
    /// than the search string (a growing result first reserves its memory,
    /// which can fail with the memory-limit fatal).
    pub(super) const STR_REPLACE: u32 = 15;
    /// `strtr(string, string, string)`.
    pub(super) const STRTR: u32 = 16;
    /// The one-argument float functions (`sqrt()`, `sin()`, `exp()`, ...,
    /// `floor()`, `ceil()`, `is_nan()`) of an int or a float: zpp widens an
    /// int silently in both modes (`zend_parse_arg_double_slow`), and the
    /// handlers read it as a float either way.
    pub(super) const FLOAT: u32 = 17;
    /// The two-argument float functions (`atan2()`, `hypot()`, `fmod()`,
    /// `fdiv()`, `fpow()`) of ints or floats.
    pub(super) const FLOAT2: u32 = 18;
    /// `log()` of an int or a float without a base (a base can be invalid).
    pub(super) const LOG: u32 = 19;
    /// `round(int|float[, int])` without a mode (a mode can be invalid).
    pub(super) const ROUND: u32 = 20;
    /// `intdiv(int, int)` neither by zero nor of `PHP_INT_MIN` by -1.
    pub(super) const INTDIV: u32 = 21;
    /// `pi()`.
    pub(super) const PI: u32 = 22;
    /// The type tests (`is_int()`, `is_string()`, `is_null()`, ...) of any
    /// value: answered without a handler ([`crate::call::fast_eval`]).
    /// php-src calls them with frames; nothing they do can see one.
    pub(super) const TYPE_TEST: u32 = 23;
    /// `strlen()` of a string (another type is coerced or rejected).
    pub(super) const STRLEN: u32 = 24;
    /// `count()`, `sizeof()` of an array (a `Countable` object calls its
    /// `count()`).
    pub(super) const COUNT: u32 = 25;
    /// `array_key_exists()` (`key_exists()`) of a string or int key in an
    /// array (a float or `null` key can warn or deprecate, another type
    /// throws).
    pub(super) const KEY_EXISTS: u32 = 26;
    /// `get_class()`, `spl_object_id()` of an object (anything else is
    /// refused by the parameter's type): answered without a handler
    /// ([`crate::call::fast_eval`]), like the type tests.
    pub(super) const OBJECT_ARG: u32 = 27;
}

/// The frameless-call class bits ([`flags::FRAMELESS`]) of the builtin
/// function named `lc` (lower case), set when it is registered. Aliases
/// copy them with the other flags.
pub(crate) fn frameless_flags(lc: &[u8]) -> u32 {
    let class = match lc {
        b"abs" => fl::ABS,
        b"dechex" => fl::DECHEX,
        b"strtolower" | b"strtoupper" => fl::CASE,
        b"trim" | b"ltrim" | b"rtrim" => fl::TRIM,
        b"dirname" => fl::DIRNAME,
        b"is_numeric" => fl::IS_NUMERIC,
        b"ord" => fl::ORD,
        b"chr" => fl::CHR,
        b"strpos" => fl::STRPOS,
        b"str_contains" | b"str_starts_with" | b"str_ends_with" => fl::STR_TEST,
        b"max" | b"min" => fl::MIN_MAX,
        b"substr" => fl::SUBSTR,
        b"strstr" => fl::STRSTR,
        b"in_array" => fl::IN_ARRAY,
        b"str_replace" => fl::STR_REPLACE,
        b"strtr" => fl::STRTR,
        b"sqrt" | b"exp" | b"expm1" | b"log10" | b"log1p" | b"sin" | b"cos" | b"tan" | b"asin"
        | b"acos" | b"atan" | b"sinh" | b"cosh" | b"tanh" | b"asinh" | b"acosh" | b"atanh"
        | b"deg2rad" | b"rad2deg" | b"floor" | b"ceil" | b"is_nan" | b"is_finite"
        | b"is_infinite" => fl::FLOAT,
        b"atan2" | b"hypot" | b"fmod" | b"fdiv" | b"fpow" => fl::FLOAT2,
        b"log" => fl::LOG,
        b"round" => fl::ROUND,
        b"intdiv" => fl::INTDIV,
        b"pi" => fl::PI,
        b"is_int" | b"is_integer" | b"is_long" | b"is_string" | b"is_array" | b"is_bool"
        | b"is_null" | b"is_float" | b"is_double" | b"is_object" | b"is_resource"
        | b"is_scalar" | b"is_iterable" | b"is_countable" | b"gettype" => fl::TYPE_TEST,
        b"strlen" => fl::STRLEN,
        // (`array_is_list()`: of an array only, as `count()`.)
        b"count" | b"sizeof" | b"array_is_list" => fl::COUNT,
        b"get_class" | b"spl_object_id" => fl::OBJECT_ARG,
        b"array_key_exists" | b"key_exists" => fl::KEY_EXISTS,
        _ => return 0,
    };
    class << flags::FRAMELESS_SHIFT
}

/// The leaf classes ([`leaf_id`]): builtins outside php-src's frameless
/// list that two framework test suites call most through a frame they
/// have no use for (11.9M of 15.9M framed builtin calls).
pub(crate) mod leaf {
    pub(crate) const NONE: u8 = 0;
    /// `defined()` of a name without `::` (a class constant looks its
    /// class up, which may autoload).
    pub(crate) const DEFINED: u8 = 1;
    /// `microtime()`, `hrtime()`: no argument or a bool.
    pub(crate) const CLOCK: u8 = 2;
    /// `time()`.
    pub(crate) const NO_ARGS: u8 = 3;
    /// `explode()` with a separator that is not empty (its `ValueError`).
    pub(crate) const EXPLODE: u8 = 4;
    /// `implode(string, array)` of scalars and nulls (an array element
    /// warns, an object's `__toString()` runs).
    pub(crate) const IMPLODE: u8 = 5;
    /// `class_uses()`, `class_parents()`, `class_implements()` of an
    /// object or of the name of a loaded class (another name autoloads).
    pub(crate) const CLASS_INFO: u8 = 6;
    /// `method_exists()` of an object, or of the name of a loaded class.
    pub(crate) const METHOD_EXISTS: u8 = 7;
    /// `class_exists()`, `interface_exists()`, `trait_exists()`,
    /// `enum_exists()` of a loaded name, or without autoloading.
    pub(crate) const CLASS_EXISTS: u8 = 8;
    /// `assert(true)`.
    pub(crate) const ASSERT: u8 = 9;
    /// `mb_strlen()` in the default encoding (also given as null) or UTF-8
    /// (another name can be invalid).
    pub(crate) const MB_STRLEN: u8 = 10;
    /// `mb_substr()`, likewise.
    pub(crate) const MB_SUBSTR: u8 = 11;
    /// `mb_strpos()`, `mb_strrpos()` of two strings (an offset can be out
    /// of range).
    pub(crate) const MB_STRPOS: u8 = 12;
    /// `array_values()`, `array_key_first()`, `array_key_last()`,
    /// `array_first()`, `array_last()`, `array_keys()` of one array.
    pub(crate) const ARRAY1: u8 = 13;
    /// `array_reverse(array[, bool])`.
    pub(crate) const ARRAY_BOOL: u8 = 14;
    /// `array_merge()` of arrays.
    pub(crate) const ARRAYS: u8 = 15;
    /// `array_slice(array, int[, ?int[, bool]])`.
    pub(crate) const ARRAY_SLICE: u8 = 16;
    /// `strcspn()`, `strspn()`, `strcmp()`, `strcasecmp()` of two strings.
    pub(crate) const STR2: u8 = 17;
    /// `preg_quote(string[, ?string])`.
    pub(crate) const PREG_QUOTE: u8 = 18;
    /// `ucfirst()`, `lcfirst()`, `crc32()`, `function_exists()` of a
    /// string.
    pub(crate) const STR1: u8 = 19;
    /// `preg_match(string, string)` (no `$matches`: by reference) of a
    /// pattern the PCRE extension has compiled ([`crate::ext::preg_leaf`]).
    pub(crate) const PREG_MATCH: u8 = 20;
    /// `preg_replace(string, string, string)`, likewise.
    pub(crate) const PREG_REPLACE: u8 = 21;
}

/// The [`leaf`] class of the builtin function named `lc` (lower case).
pub(crate) fn leaf_id(lc: &[u8]) -> u8 {
    match lc {
        b"defined" => leaf::DEFINED,
        b"microtime" | b"hrtime" => leaf::CLOCK,
        b"time" => leaf::NO_ARGS,
        b"explode" => leaf::EXPLODE,
        b"implode" | b"join" => leaf::IMPLODE,
        b"class_uses" | b"class_parents" | b"class_implements" => leaf::CLASS_INFO,
        b"method_exists" => leaf::METHOD_EXISTS,
        b"class_exists" | b"interface_exists" | b"trait_exists" | b"enum_exists" => {
            leaf::CLASS_EXISTS
        }
        b"assert" => leaf::ASSERT,
        b"mb_strlen" => leaf::MB_STRLEN,
        b"mb_substr" => leaf::MB_SUBSTR,
        b"mb_strpos" | b"mb_strrpos" => leaf::MB_STRPOS,
        b"array_values" | b"array_key_first" | b"array_key_last" | b"array_first"
        | b"array_last" | b"array_keys" => leaf::ARRAY1,
        b"array_reverse" => leaf::ARRAY_BOOL,
        b"array_merge" => leaf::ARRAYS,
        b"array_slice" => leaf::ARRAY_SLICE,
        b"strcspn" | b"strspn" | b"strcmp" | b"strcasecmp" => leaf::STR2,
        b"preg_quote" => leaf::PREG_QUOTE,
        b"ucfirst" | b"lcfirst" | b"crc32" | b"function_exists" => leaf::STR1,
        b"preg_match" => leaf::PREG_MATCH,
        b"preg_replace" => leaf::PREG_REPLACE,
        _ => leaf::NONE,
    }
}

/// Whether `enc` names UTF-8 as the mbstring functions take it.
fn is_utf8_name(enc: &[u8]) -> bool {
    enc.eq_ignore_ascii_case(b"utf-8") || enc.eq_ignore_ascii_case(b"utf8")
}

impl Vm {
    /// Whether the arguments `args` have a shape of leaf class `id`
    /// ([`leaf`]): the handler then reports nothing, throws nothing, runs
    /// no PHP code and looks at no frame, so that calling it without its
    /// own shows nowhere. Every value is matched by its kind (a reference
    /// or a value its parameter would coerce is the framed call's).
    #[allow(clippy::unnested_or_patterns)] // one row per class and shape
    pub(crate) fn leaf_args(&self, id: u8, args: &[&Value]) -> bool {
        use Value::{Array, Bool, Float, Int, Null, Object, Str};
        let loaded = |name: &ZStr| self.find_class(name.as_bytes()).is_some();
        match (id, args) {
            (leaf::DEFINED, [Str(name)]) => {
                crate::call::find_double_colon(name.as_bytes()).is_none()
            }
            (leaf::CLOCK, [] | [Bool(_)])
            | (leaf::NO_ARGS, [])
            | (leaf::CLASS_INFO, [Object(_)] | [Object(_), Bool(_)])
            | (leaf::METHOD_EXISTS, [Object(_), Str(_)])
            | (leaf::CLASS_EXISTS, [Str(_), Bool(false)])
            | (leaf::ASSERT, [Bool(true)] | [Bool(true), _])
            // (A null encoding is the default one.)
            | (leaf::MB_STRLEN, [Str(_)] | [Str(_), Null])
            | (
                leaf::MB_SUBSTR,
                [Str(_), Int(_)] | [Str(_), Int(_), Int(_) | Null] | [Str(_), Int(_), Int(_) | Null, Null],
            )
            | (leaf::MB_STRPOS | leaf::STR2, [Str(_), Str(_)])
            | (leaf::ARRAY1, [Array(_)])
            | (leaf::ARRAY_BOOL, [Array(_)] | [Array(_), Bool(_)])
            | (
                leaf::ARRAY_SLICE,
                [Array(_), Int(_)]
                | [Array(_), Int(_), Int(_) | Null]
                | [Array(_), Int(_), Int(_) | Null, Bool(_)],
            )
            | (leaf::PREG_QUOTE, [Str(_)] | [Str(_), Str(_) | Null])
            | (leaf::STR1, [Str(_)]) => true,
            (leaf::EXPLODE, [Str(sep), Str(_)]) | (leaf::EXPLODE, [Str(sep), Str(_), Int(_)]) => {
                !sep.is_empty()
            }
            (leaf::IMPLODE, [Str(_), Array(pieces)]) => pieces
                .values()
                .all(|v| matches!(v, Null | Bool(_) | Int(_) | Float(_) | Str(_))),
            (leaf::CLASS_INFO, [Str(name)] | [Str(name), Bool(_)])
            | (leaf::METHOD_EXISTS, [Str(name), Str(_)])
            | (leaf::CLASS_EXISTS, [Str(name)] | [Str(name), Bool(true)]) => loaded(name),
            (leaf::MB_STRLEN, [Str(_), Str(enc)])
            | (leaf::MB_SUBSTR, [Str(_), Int(_), Int(_) | Null, Str(enc)]) => {
                is_utf8_name(enc.as_bytes())
            }
            (leaf::ARRAYS, [_, ..]) => args.iter().all(|a| matches!(a, Array(_))),
            (leaf::PREG_MATCH, [Str(pattern), Str(_)])
            | (leaf::PREG_REPLACE, [Str(pattern), Str(_), Str(_)]) => {
                crate::ext::preg_leaf(pattern)
            }
            _ => false,
        }
    }
}

/// Whether `call` of builtin `b` runs frameless. php-src's frameless
/// internal calls (`ZEND_FRAMELESS_ICALL_n`) skip the call frame and
/// synthesise one from the opline when the function reports an error
/// (`zend_fetch_debug_backtrace`, `zend_active_function_ex`). Here the
/// choice is made per call instead: a plain call ([`plain_call`]) of a
/// function with a frameless class runs without a frame only for
/// arguments with which nothing can see the frame ([`frameless_args`]:
/// accepted types, so zpp coerces nothing, and values the handler cannot
/// report about). Every other call, including each one that could warn or
/// throw, pushes the real builtin frame, which is the frame php-src would
/// synthesise: traces still show `#0 file(line): strpos(...)`.
#[inline]
pub(crate) fn frameless_call(b: &Builtin, call: &PendingCall) -> bool {
    let class = (b.flags & flags::FRAMELESS) >> flags::FRAMELESS_SHIFT;
    class != 0
        && b.flags & (flags::DEPRECATED | flags::NODISCARD) == 0
        && plain_call(call)
        && frameless_args(class, &call.args)
}

/// A call by name with positional arguments only and nothing bound to it:
/// what `ZEND_INIT_FCALL` and the `SEND` ops make of `f($a, $b)`.
/// Always inline: a few field tests, made out of line (~30 instructions
/// a frameless call) when merely `#[inline]`.
#[inline(always)]
#[allow(clippy::inline_always)]
fn plain_call(call: &PendingCall) -> bool {
    !call.has_named
        && !call.dynamic
        && !call.is_ctor
        && call.named.is_empty()
        && call.magic_name.is_none()
        && call.this.is_none()
        && call.scope.is_none()
        && call.called.is_none()
        && call.closure.is_none()
        && call.pfa.is_none()
}

/// Whether the handler of frameless class `class` ([`fl`]) runs for
/// `args` (as sent: by value, all passed) without anything that could
/// observe its frame: the values already have the declared types (no
/// coercion, deprecation or `TypeError`), and none of the class's error
/// conditions holds.
pub(crate) fn frameless_args(class: u32, args: &[Option<Value>]) -> bool {
    let mut refs: [Option<&Value>; 3] = [None; 3];
    let Some(sent) = refs.get_mut(..args.len()) else {
        return false;
    };
    for (r, a) in sent.iter_mut().zip(args) {
        *r = a.as_ref();
    }
    frameless_refs(class, sent)
}

/// [`frameless_args`] of arguments read straight from their operands
/// ([`Vm::frameless_icall_op`]). The borrows are built at fixed positions
/// (a loop over a zipped iterator into a zeroed array cost ~48
/// instructions a call).
#[inline]
fn frameless_values(class: u32, args: &[Value]) -> bool {
    let refs = [args.first(), args.get(1), args.get(2)];
    refs.get(..args.len())
        .is_some_and(|sent| frameless_refs(class, sent))
}

/// Marks the `n` arguments of a frameless call (at most three) as passed,
/// in an emptied record's flag vector: a push each, where
/// `Vec::resize` was an out-of-line call (~37 instructions a call).
#[inline(always)]
#[allow(clippy::inline_always)]
// twice, in the frameless calls
// A push each, not `extend`/`resize`: those are the out-of-line call this
// replaces.
#[allow(clippy::same_item_push)]
fn push_passed(passed: &mut Vec<bool>, n: usize) {
    for _ in 0..n {
        passed.push(true);
    }
}

/// [`frameless_args`] of borrowed arguments (`None`: a position not
/// sent, which no class takes).
fn frameless_refs(class: u32, args: &[Option<&Value>]) -> bool {
    use Value::{Array, Bool, Float, Int, Null, Str};
    match (class, args) {
        (fl::ABS | fl::FLOAT | fl::LOG | fl::ROUND, [Some(Int(_) | Float(_))])
        | (fl::FLOAT2, [Some(Int(_) | Float(_)), Some(Int(_) | Float(_))])
        | (fl::ROUND, [Some(Int(_) | Float(_)), Some(Int(_))])
        | (fl::PI, [])
        | (fl::DECHEX, [Some(Int(_))])
        | (fl::CASE | fl::TRIM | fl::DIRNAME | fl::STRLEN, [Some(Str(_))])
        | (fl::STRPOS | fl::STR_TEST | fl::STRSTR, [Some(Str(_)), Some(Str(_))])
        | (fl::STRSTR, [Some(Str(_)), Some(Str(_)), Some(Bool(_))])
        | (
            fl::SUBSTR,
            [Some(Str(_)), Some(Int(_))] | [Some(Str(_)), Some(Int(_)), Some(Int(_) | Null)],
        )
        | (fl::STRTR, [Some(Str(_)), Some(Str(_)), Some(Str(_))])
        | (fl::TYPE_TEST, [Some(_)])
        | (fl::OBJECT_ARG, [Some(Value::Object(_))])
        | (fl::COUNT, [Some(Array(_))])
        | (fl::KEY_EXISTS, [Some(Str(_) | Int(_)), Some(Array(_))]) => true,
        (fl::TRIM, [Some(Str(_)), Some(Str(chars))]) => !has_char_range(chars.as_bytes()),
        (fl::DIRNAME, [Some(Str(_)), Some(Int(levels))]) => *levels >= 1,
        (fl::IS_NUMERIC, [Some(v)]) => is_scalar(v),
        (fl::ORD, [Some(Str(s))]) => s.len() == 1,
        (fl::CHR, [Some(Int(c))]) => (0..=255).contains(c),
        (fl::STRPOS, [Some(Str(haystack)), Some(Str(_)), Some(Int(offset))]) => {
            offset_within(haystack.len(), *offset)
        }
        (fl::MIN_MAX, [Some(a), Some(b)]) => is_scalar(a) && is_scalar(b),
        (fl::IN_ARRAY, [Some(needle), Some(Array(haystack))]) => {
            is_scalar(needle) && loose_comparisons_silent(haystack)
        }
        (fl::IN_ARRAY, [Some(needle), Some(Array(haystack)), Some(Bool(strict))]) => {
            is_scalar(needle) && (*strict || loose_comparisons_silent(haystack))
        }
        (fl::STR_REPLACE, [Some(Str(search)), Some(Str(replace)), Some(Str(_))]) => {
            replace.len() <= search.len()
        }
        (fl::INTDIV, [Some(Int(dividend)), Some(Int(divisor))]) => {
            *divisor != 0 && !(*dividend == i64::MIN && *divisor == -1)
        }
        _ => false,
    }
}

/// [`frameless_args`] of two or three arguments read in place: the same
/// shapes, without the arguments copied into slots first.
fn frameless_args_ref(class: u32, args: &[&Value]) -> bool {
    use Value::{Array, Bool, Float, Int, Null, Str};
    match (class, args) {
        (fl::FLOAT2, [Int(_) | Float(_), Int(_) | Float(_)])
        | (fl::ROUND, [Int(_) | Float(_), Int(_)])
        | (fl::STRPOS | fl::STR_TEST | fl::STRSTR, [Str(_), Str(_)])
        | (fl::STRSTR, [Str(_), Str(_), Bool(_)])
        | (fl::SUBSTR, [Str(_), Int(_)] | [Str(_), Int(_), Int(_) | Null])
        | (fl::STRTR, [Str(_), Str(_), Str(_)])
        | (fl::KEY_EXISTS, [Str(_) | Int(_), Array(_)]) => true,
        (fl::TRIM, [Str(_), Str(chars)]) => !has_char_range(chars.as_bytes()),
        (fl::DIRNAME, [Str(_), Int(levels)]) => *levels >= 1,
        (fl::STRPOS, [Str(haystack), Str(_), Int(offset)]) => {
            offset_within(haystack.len(), *offset)
        }
        (fl::MIN_MAX, [a, b]) => is_scalar(a) && is_scalar(b),
        (fl::IN_ARRAY, [needle, Array(haystack)]) => {
            is_scalar(needle) && loose_comparisons_silent(haystack)
        }
        (fl::IN_ARRAY, [needle, Array(haystack), Bool(strict)]) => {
            is_scalar(needle) && (*strict || loose_comparisons_silent(haystack))
        }
        (fl::STR_REPLACE, [Str(search), Str(replace), Str(_)]) => replace.len() <= search.len(),
        (fl::INTDIV, [Int(dividend), Int(divisor)]) => {
            *divisor != 0 && !(*dividend == i64::MIN && *divisor == -1)
        }
        _ => false,
    }
}

/// The frameless classes whose one-argument call [`frameless_inline`]
/// answers without a handler.
#[inline]
fn inline_class(class: u32) -> bool {
    matches!(
        class,
        fl::TYPE_TEST
            | fl::OBJECT_ARG
            | fl::STRLEN
            | fl::COUNT
            | fl::CASE
            | fl::ABS
            | fl::ORD
            | fl::CHR
            | fl::DECHEX
            | fl::FLOAT
            | fl::LOG
            | fl::ROUND
    )
}

/// Whether a call of frameless class `class` with `argc` arguments can
/// ever run in place (the classes added for namespaced calls take exactly
/// one): calls that cannot keep their own ops at compile time, which the
/// tape prepares (`count($a, COUNT_RECURSIVE)`).
pub(crate) fn frameless_arity(lc: &[u8], argc: usize) -> bool {
    match frameless_flags(lc) >> flags::FRAMELESS_SHIFT {
        0 => false,
        fl::TYPE_TEST | fl::OBJECT_ARG | fl::STRLEN | fl::COUNT => argc == 1,
        fl::KEY_EXISTS => argc == 2,
        _ => true,
    }
}

/// [`frameless_args`] of one argument, read in place (`[Some(v)]`): the
/// same shapes, without the argument copied into a slot first.
#[inline]
fn frameless_arg1(class: u32, v: &Value) -> bool {
    use Value::{Array, Float, Int, Str};
    match (class, v) {
        (fl::ABS | fl::FLOAT | fl::LOG | fl::ROUND, Int(_) | Float(_))
        | (fl::DECHEX, Int(_))
        | (fl::CASE | fl::TRIM | fl::DIRNAME | fl::STRLEN, Str(_))
        | (fl::TYPE_TEST, _)
        | (fl::OBJECT_ARG, Value::Object(_))
        | (fl::COUNT, Array(_)) => true,
        (fl::IS_NUMERIC, v) => is_scalar(v),
        (fl::ORD, Str(s)) => s.len() == 1,
        (fl::CHR, Int(c)) => (0..=255).contains(c),
        _ => false,
    }
}

/// Whether `v` is null or a scalar: a value whose comparisons and
/// conversions run no PHP code and report nothing.
#[inline]
fn is_scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_)
    )
}

/// Whether `php_charmask()` reads a `..` range in the character list
/// `chars` (and may warn that it is invalid).
fn has_char_range(chars: &[u8]) -> bool {
    chars.windows(2).any(|w| w == b"..")
}

/// Whether `offset` is inside a haystack of `len` bytes as `strpos()`
/// requires (`-len <= offset <= len`).
fn offset_within(len: usize, offset: i64) -> bool {
    let len = i64::try_from(len).unwrap_or(i64::MAX);
    (-len..=len).contains(&offset)
}

/// The longest haystack a loose frameless `in_array()` checks up front;
/// longer ones take the framed call rather than being walked twice.
const LOOSE_SCAN_MAX: usize = 32;

/// Whether the loose comparisons (`==`) of a scalar with each element of
/// `haystack` are silent: no element is an object (conversion warnings,
/// `__toString()`) or a resource, also behind a reference.
fn loose_comparisons_silent(haystack: &ZArray) -> bool {
    let silent = |v: &Value| !matches!(v, Value::Object(_) | Value::Resource(_));
    haystack.len() <= LOOSE_SCAN_MAX
        && haystack.values().all(|v| match v {
            Value::Reference(r) => r.try_borrow().is_some_and(|inner| silent(&inner)),
            v => silent(v),
        })
}

impl Vm {
    /// A [`frameless_call`]: the handler runs on the caller's frame with
    /// the arguments moved out of the pending call (and the declared
    /// defaults of omitted trailing parameters, as for a framed call).
    /// The call is a [`plain_call`], so only the function handle (moved
    /// into the handler's arguments) and the argument buffer (back to the
    /// pool) are left to release.
    pub(crate) fn call_builtin_frameless(
        &mut self,
        b: &Rc<Builtin>,
        call: PendingCall,
    ) -> VmResult<Value> {
        if self.magic_call_name.is_some() {
            self.magic_call_name = None;
        }
        let PendingCall {
            func,
            args: mut sent,
            named,
            ..
        } = call;
        forget_unallocated(named);
        let handle = match func {
            Func::Builtin(handle) => handle,
            Func::User(_) => b.clone(),
        };
        let nparams = b.params.len();
        let mut values = self.call_bufs.values.pop().unwrap_or_default();
        values.reserve(nparams);
        let mut passed = self.call_bufs.passed.pop().unwrap_or_default();
        passed.reserve(nparams);
        for arg in &mut sent {
            values.push(arg.take().unwrap_or_default());
            passed.push(true);
        }
        while values.len() < nparams {
            let i = values.len();
            match self.builtin_default(b, i) {
                Some(d) if i >= b.required as usize => {
                    values.push(d);
                    passed.push(false);
                }
                _ => break,
            }
        }
        let mut args = Args {
            values,
            passed,
            named: Vec::new(),
            this: None,
            called: None,
            func: Some(handle),
        };
        let mut r = (b.handler)(self, &mut args);
        // `ZEND_VM_FCALL_INTERRUPT_CHECK`, after the call.
        if r.is_ok() {
            if let Err(e) = self.check_interrupt() {
                r = Err(e);
            }
        }
        let Args {
            values,
            passed,
            named,
            ..
        } = args;
        forget_unallocated(named);
        self.put_val_vec(values);
        self.put_bool_vec(passed);
        self.put_arg_vec(sent);
        r
    }
}

/// The result of a one-argument frameless call that needs no handler:
/// `abs()` of an int other than `PHP_INT_MIN` or of a float, `ord()` of a
/// one-byte string, `chr()` of 0..=255 (the shared one-character string,
/// `ZSTR_CHAR`, as the handler returns it), and the one-argument math
/// functions of classes `FLOAT`, `LOG` and `ROUND` (function `name`; see
/// [`crate::call::float_builtin_eval`]), `strtolower()` and `strtoupper()`
/// of a string, and the type tests, `strlen()` and `count()`
/// ([`crate::call::fast_eval`]). `arg` already has the shape
/// [`frameless_args`] accepts for `class`; `None` for every other call.
#[inline]
fn frameless_inline(name: &[u8], class: u32, arg: Option<&Value>) -> Option<Value> {
    match (class, arg?) {
        (fl::ABS, Value::Int(i)) if *i != i64::MIN => Some(Value::Int(i.abs())),
        (fl::ABS, Value::Float(x)) => Some(Value::Float(x.abs())),
        (fl::ORD, Value::Str(s)) => s.as_bytes().first().map(|&c| Value::Int(i64::from(c))),
        (fl::CHR, Value::Int(c)) => u8::try_from(*c)
            .ok()
            .map(|c| Value::Str(ZStr::from_byte(c))),
        (fl::DECHEX, Value::Int(i)) => Some(Value::Str(dechex_str(*i))),
        (fl::FLOAT | fl::LOG | fl::ROUND, v) => crate::call::float_builtin_eval(name, v),
        (fl::TYPE_TEST | fl::OBJECT_ARG | fl::STRLEN | fl::COUNT, v) => {
            crate::call::fast_eval(crate::call::fast_eval_id(name), v)
        }
        (fl::TRIM | fl::CASE, v @ Value::Str(_)) => quick_eval(quick_id(name)?.0, v),
        _ => None,
    }
}

/// `dechex()` of `num` as its handler builds it
/// (`_php_math_longtobase(num, 16)`: the number read as unsigned, in
/// lower-case digits), without the handler's argument and result vectors.
#[inline(never)] // inside `quick_eval`, every `sqrt()` and `abs()` saved its registers
fn dechex_str(num: i64) -> ZStr {
    let mut value = u64::from_ne_bytes(num.to_ne_bytes());
    // A digit for every four bits up to the highest one set, one for 0
    // (`_php_math_longtobase_pwr2`'s length from `zend_ulong_nlz`).
    let digits = (67 - (value | 1).leading_zeros()) / 4;
    // The digits are asked for from the last (the lowest) to the first.
    ZStr::from_fn(digits as usize, |_| {
        let digit = b"0123456789abcdef"[usize::from((value & 15) as u8)];
        value >>= 4;
        digit
    })
}

/// [`quick_id`] of `abs()`: the ids of the math functions
/// ([`crate::call::ff`]) are below these.
pub(crate) const QUICK_ABS: u8 = 0x40;
/// [`quick_id`] of `ord()`.
pub(crate) const QUICK_ORD: u8 = 0x41;
/// [`quick_id`] of `chr()`.
pub(crate) const QUICK_CHR: u8 = 0x42;
/// [`quick_id`] of `dechex()`.
pub(crate) const QUICK_DECHEX: u8 = 0x43;
/// [`quick_id`] of `trim()` of one string (the default character list).
pub(crate) const QUICK_TRIM: u8 = 0x44;
/// [`quick_id`] of `ltrim()` of one string.
pub(crate) const QUICK_LTRIM: u8 = 0x45;
/// [`quick_id`] of `rtrim()` of one string.
pub(crate) const QUICK_RTRIM: u8 = 0x46;
/// [`quick_id`] of `strtolower()`.
pub(crate) const QUICK_LOWER: u8 = 0x47;
/// [`quick_id`] of `strtoupper()`.
pub(crate) const QUICK_UPPER: u8 = 0x48;
/// The first [`quick_id`] of the type tests, `strlen()` and `count()`:
/// this plus their [`crate::call::fe`] id, answered by
/// [`crate::call::fast_eval`] as [`frameless_inline`] answers them.
pub(crate) const QUICK_FAST: u8 = 0x60;
const _: () = assert!(crate::call::ff::COUNT <= QUICK_ABS);
const _: () = assert!(QUICK_UPPER < QUICK_STRPOS);
const _: () = assert!(QUICK_KEY_EXISTS < QUICK_FAST);
const _: () = assert!(QUICK_FAST + crate::call::fe::LAST < 0x7f);

/// The id of the builtin named `lc` (lower case) when its one-argument
/// call needs no handler ([`frameless_inline`]), with its frameless class:
/// what a tape instruction carries in place of the name (`exec/tape/strs.rs`).
/// `None` for every other function.
pub(crate) fn quick_id(lc: &[u8]) -> Option<(u8, u32)> {
    let class = frameless_flags(lc) >> flags::FRAMELESS_SHIFT;
    let id = match class {
        fl::ABS => QUICK_ABS,
        fl::ORD => QUICK_ORD,
        fl::CHR => QUICK_CHR,
        fl::DECHEX => QUICK_DECHEX,
        fl::FLOAT | fl::LOG | fl::ROUND => crate::call::float_fn_id(lc)?,
        fl::TRIM => match lc {
            b"trim" => QUICK_TRIM,
            b"ltrim" => QUICK_LTRIM,
            b"rtrim" => QUICK_RTRIM,
            _ => return None,
        },
        fl::CASE => match lc {
            b"strtolower" => QUICK_LOWER,
            b"strtoupper" => QUICK_UPPER,
            _ => return None,
        },
        _ => return None,
    };
    Some((id, class))
}

/// The tape's [`quick_id_n`] of `is_null()`, `is_array()`, ...,
/// `strlen()` and `count()` (namespaced calls of these are frameless
/// calls; a global one compiles to its own op): [`QUICK_FAST`] plus the
/// [`crate::call::fe`] id. Not [`quick_id`]'s, which the optimizer also
/// evaluates constant calls with (php-src's sccp does not fold these).
fn quick_fast_id(lc: &[u8]) -> Option<(u8, u32)> {
    let class = frameless_flags(lc) >> flags::FRAMELESS_SHIFT;
    if !matches!(
        class,
        fl::TYPE_TEST | fl::OBJECT_ARG | fl::STRLEN | fl::COUNT
    ) {
        return None;
    }
    match crate::call::fast_eval_id(lc) {
        crate::call::fe::NONE => None,
        f => Some((QUICK_FAST + f, class)),
    }
}

/// Whether the flags of a builtin name frameless class `class` and nothing
/// a call must report first: the test [`Vm::frameless_icall_op`] makes of
/// its call site's cached function.
#[inline]
pub(crate) fn frameless_class_is(func_flags: u32, class: u32) -> bool {
    func_flags & (flags::FRAMELESS | flags::DEPRECATED | flags::NODISCARD)
        == class << flags::FRAMELESS_SHIFT
}

/// [`frameless_inline`] of the function with id `id` ([`quick_id`]) for
/// the argument `v`, shape test included ([`frameless_args`] of one
/// argument for the id's class): the result, or `None` when the call
/// needs its handler (another type, `abs(PHP_INT_MIN)`, an `ord()` of a
/// string that is not one byte, a `chr()` outside 0..=255).
#[inline]
pub(crate) fn quick_eval(id: u8, v: &Value) -> Option<Value> {
    match (id, v) {
        (QUICK_ABS, Value::Int(i)) if *i != i64::MIN => Some(Value::Int(i.abs())),
        (QUICK_ABS, Value::Float(x)) => Some(Value::Float(x.abs())),
        (QUICK_ORD, Value::Str(s)) if s.len() == 1 => {
            s.as_bytes().first().map(|&c| Value::Int(i64::from(c)))
        }
        (QUICK_CHR, Value::Int(c)) => u8::try_from(*c)
            .ok()
            .map(|c| Value::Str(ZStr::from_byte(c))),
        (QUICK_DECHEX, Value::Int(i)) => Some(Value::Str(dechex_str(*i))),
        (QUICK_TRIM, Value::Str(s)) => Some(Value::Str(trim_default(s, TrimMode::Both))),
        (QUICK_LTRIM, Value::Str(s)) => Some(Value::Str(trim_default(s, TrimMode::Left))),
        (QUICK_RTRIM, Value::Str(s)) => Some(Value::Str(trim_default(s, TrimMode::Right))),
        (QUICK_LOWER, Value::Str(s)) => Some(Value::Str(case_quick(s, false))),
        (QUICK_UPPER, Value::Str(s)) => Some(Value::Str(case_quick(s, true))),
        (
            QUICK_ABS | QUICK_ORD | QUICK_CHR | QUICK_DECHEX | QUICK_TRIM | QUICK_LTRIM
            | QUICK_RTRIM | QUICK_LOWER | QUICK_UPPER,
            _,
        ) => None,
        // (A reference is the handler's: the tests answer of its value.)
        (id, v) if (QUICK_FAST..=QUICK_FAST + crate::call::fe::LAST).contains(&id) => {
            if matches!(v, Value::Reference(_)) {
                return None;
            }
            crate::call::fast_eval(id - QUICK_FAST, v)
        }
        (id, v) => crate::call::float_fn_eval(id, v),
    }
}

/// `strtoupper()` (`upper`) or `strtolower()` of `s`, as the handlers
/// return it (`zend_string_toupper` / `zend_string_tolower`: the string
/// itself when no byte changes). Out of line, as [`trim_default`].
#[inline(never)]
fn case_quick(s: &ZStr, upper: bool) -> ZStr {
    if upper {
        s.to_ascii_uppercase()
    } else {
        s.to_ascii_lowercase()
    }
}

/// `trim()`, `ltrim()` or `rtrim()` of `s` with the default character
/// list, as the handler (`php_trim_int()`) returns it: the string itself
/// when nothing is trimmed, the shared empty string when everything is,
/// else a copy of what is left. Out of line: the other quick functions'
/// paths keep their registers.
#[inline(never)]
fn trim_default(s: &ZStr, mode: TrimMode) -> ZStr {
    // The default list reports nothing (no `..` range to check).
    let mut notices = php_stdstr::error::Notices::new();
    let r = php_stdstr::transform::trim(s.as_bytes(), None, mode, &mut notices);
    if r.len() == s.len() {
        s.clone()
    } else if r.is_empty() {
        ZStr::empty()
    } else {
        ZStr::from(r)
    }
}

/// `trim()`, `ltrim()` or `rtrim()` of `s` with the character list
/// `chars`, which holds no `..` range (the handler then reports nothing),
/// as the handler returns it: see [`trim_default`].
#[inline(never)]
fn trim_chars(s: &ZStr, chars: &ZStr, mode: TrimMode) -> ZStr {
    let mut notices = php_stdstr::error::Notices::new();
    let r = php_stdstr::transform::trim(s.as_bytes(), Some(chars.as_bytes()), mode, &mut notices);
    if r.len() == s.len() {
        s.clone()
    } else if r.is_empty() {
        ZStr::empty()
    } else {
        ZStr::from(r)
    }
}

/// [`quick_id_n`] of `strpos()` with two or three arguments.
pub(crate) const QUICK_STRPOS: u8 = 0x50;
/// [`quick_id_n`] of `substr()` with two or three arguments.
pub(crate) const QUICK_SUBSTR: u8 = 0x51;
/// [`quick_id_n`] of `max()` with two arguments.
pub(crate) const QUICK_MAX: u8 = 0x52;
/// [`quick_id_n`] of `min()` with two arguments.
pub(crate) const QUICK_MIN: u8 = 0x53;
/// [`quick_id_n`] of `in_array()` with three arguments.
pub(crate) const QUICK_IN_ARRAY: u8 = 0x54;
/// [`quick_id_n`] of `str_contains()`.
pub(crate) const QUICK_STR_CONTAINS: u8 = 0x55;
/// [`quick_id_n`] of `str_starts_with()`.
pub(crate) const QUICK_STR_STARTS: u8 = 0x56;
/// [`quick_id_n`] of `str_ends_with()`.
pub(crate) const QUICK_STR_ENDS: u8 = 0x57;
/// [`quick_id_n`] of `array_key_exists()`.
pub(crate) const QUICK_KEY_EXISTS: u8 = 0x58;

/// The id of the builtin named `lc` (lower case) when its call with `argc`
/// arguments can be answered without its handler ([`quick_eval_n`]), with
/// its frameless class: what a tape instruction carries in place of the
/// name (`exec/tape/strs.rs`). `None` for every other call.
pub(crate) fn quick_id_n(lc: &[u8], argc: usize) -> Option<(u8, u32)> {
    if argc == 1 {
        return quick_id(lc).or_else(|| quick_fast_id(lc));
    }
    let id = match (lc, argc) {
        (b"strpos", 2 | 3) => QUICK_STRPOS,
        (b"substr", 2 | 3) => QUICK_SUBSTR,
        (b"max", 2) => QUICK_MAX,
        (b"min", 2) => QUICK_MIN,
        (b"in_array", 2 | 3) => QUICK_IN_ARRAY,
        (b"str_contains", 2) => QUICK_STR_CONTAINS,
        (b"str_starts_with", 2) => QUICK_STR_STARTS,
        (b"str_ends_with", 2) => QUICK_STR_ENDS,
        (b"array_key_exists", 2) => QUICK_KEY_EXISTS,
        // (Two arguments: a character list, `quick_eval_2`.)
        (b"trim", 2) => QUICK_TRIM,
        (b"ltrim", 2) => QUICK_LTRIM,
        (b"rtrim", 2) => QUICK_RTRIM,
        _ => return None,
    };
    Some((id, frameless_flags(lc) >> flags::FRAMELESS_SHIFT))
}

/// Whether php-src itself compiles the call of the builtin named `lc` with
/// `argc` arguments to a `ZEND_FRAMELESS_ICALL_n` (its
/// `ZEND_FRAMELESS_FUNCTION`s of that arity): the arguments are then
/// compiled as expressions before the one op, which reads a variable among
/// them when it runs. The other functions with a frameless class here are
/// regular calls in php-src, whose `SEND`s copy each argument where it
/// stands (`compiler::specialise::spread_frameless_call_at`).
pub(crate) fn defers_like_php_src(lc: &[u8], argc: usize) -> bool {
    match frameless_flags(lc) >> flags::FRAMELESS_SHIFT {
        fl::DECHEX | fl::CASE | fl::IS_NUMERIC => argc == 1,
        fl::TRIM | fl::DIRNAME => argc == 1 || argc == 2,
        fl::STR_TEST | fl::MIN_MAX => argc == 2,
        fl::STRPOS | fl::SUBSTR | fl::STRSTR | fl::IN_ARRAY | fl::STRTR => argc == 2 || argc == 3,
        fl::STR_REPLACE => argc == 3,
        _ => false,
    }
}

/// What `strpos()` returns for a search result.
#[inline]
fn quick_pos(found: Option<i64>) -> Value {
    found.map_or(Value::Bool(false), Value::Int)
}

/// What `substr()` returns: `RETURN_STR_COPY(str)` for the whole string,
/// else `RETURN_STRINGL_FAST` (the shared empty and one-byte strings).
#[inline]
fn quick_substr(s: &ZStr, offset: i64, length: Option<i64>) -> Value {
    let part = php_stdstr::search::substr(s.as_bytes(), offset, length);
    Value::Str(if part.len() == s.len() {
        s.clone()
    } else {
        ZStr::init_fast(part)
    })
}

/// The result of the call of the function with id `id` ([`quick_id_n`])
/// with the two arguments `a` and `b`, as its handler computes it for
/// arguments of the types it is declared with, shape test included
/// ([`frameless_args`] for the id's class, of which this takes the common
/// part): `None` when the call needs its handler (another type, numbers of
/// mixed types for `max()` and `min()`).
pub(crate) fn quick_eval_2(id: u8, a: &Value, b: &Value) -> Option<Value> {
    use php_stdstr::search;
    use Value::{Bool, Float, Int, Str};
    Some(match (id, a, b) {
        (QUICK_STRPOS, Str(h), Str(n)) => {
            quick_pos(search::strpos(h.as_bytes(), n.as_bytes(), 0).ok()?)
        }
        (QUICK_SUBSTR, Str(s), Int(offset)) => quick_substr(s, *offset, None),
        // `ZEND_FRAMELESS_FUNCTION(max, 2)` / `(min, 2)` of two ints or two
        // floats: `max` keeps the first of two equal values, `min` the
        // second, and a NaN on either side gives the second.
        (QUICK_MAX, Int(l), Int(r)) => Int(if l >= r { *l } else { *r }),
        (QUICK_MIN, Int(l), Int(r)) => Int(if l < r { *l } else { *r }),
        (QUICK_MAX, Float(l), Float(r)) => Float(if l >= r { *l } else { *r }),
        (QUICK_MIN, Float(l), Float(r)) => Float(if l < r { *l } else { *r }),
        (QUICK_STR_CONTAINS, Str(h), Str(n)) => {
            Bool(search::str_contains(h.as_bytes(), n.as_bytes()))
        }
        (QUICK_STR_STARTS, Str(h), Str(n)) => {
            Bool(search::str_starts_with(h.as_bytes(), n.as_bytes()))
        }
        (QUICK_STR_ENDS, Str(h), Str(n)) => Bool(search::str_ends_with(h.as_bytes(), n.as_bytes())),
        (QUICK_IN_ARRAY, needle, Value::Array(haystack)) => Bool(in_array_loose(needle, haystack)?),
        // (As `frameless_many`'s: a string key by the symbol table's rules,
        // `"1"` finding the int key.)
        (QUICK_KEY_EXISTS, Str(k), Value::Array(a)) => Bool(a.get_sym_zstr(k).is_some()),
        (QUICK_KEY_EXISTS, Int(k), Value::Array(a)) => Bool(a.get_int(*k).is_some()),
        (QUICK_TRIM, Str(v), Str(chars)) if !has_char_range(chars.as_bytes()) => {
            Str(trim_chars(v, chars, TrimMode::Both))
        }
        (QUICK_LTRIM, Str(v), Str(chars)) if !has_char_range(chars.as_bytes()) => {
            Str(trim_chars(v, chars, TrimMode::Left))
        }
        (QUICK_RTRIM, Str(v), Str(chars)) if !has_char_range(chars.as_bytes()) => {
            Str(trim_chars(v, chars, TrimMode::Right))
        }
        _ => return None,
    })
}

/// [`quick_eval_2`] for three arguments: `None` as there, and for an offset
/// outside the haystack (the handler's `ValueError`).
pub(crate) fn quick_eval_3(id: u8, first: &Value, second: &Value, third: &Value) -> Option<Value> {
    use php_stdstr::search;
    use Value::{Array, Bool, Int, Null, Str};
    Some(match (id, first, second, third) {
        (QUICK_STRPOS, Str(h), Str(n), Int(offset)) => {
            quick_pos(search::strpos(h.as_bytes(), n.as_bytes(), *offset).ok()?)
        }
        (QUICK_SUBSTR, Str(s), Int(offset), Null) => quick_substr(s, *offset, None),
        (QUICK_SUBSTR, Str(s), Int(offset), Int(length)) => quick_substr(s, *offset, Some(*length)),
        (QUICK_IN_ARRAY, needle, Array(haystack), Bool(true)) => {
            Bool(in_array_identical(needle, haystack)?)
        }
        (QUICK_IN_ARRAY, needle, Array(haystack), Bool(false)) => {
            Bool(in_array_loose(needle, haystack)?)
        }
        _ => return None,
    })
}

/// [`quick_eval_2`] or [`quick_eval_3`] by the number of arguments (one
/// argument: [`quick_eval`]): what the tests compare with the handlers.
#[cfg(test)]
pub(crate) fn quick_eval_n(id: u8, args: &[&Value]) -> Option<Value> {
    match args {
        [a, b] => quick_eval_2(id, a, b),
        [a, b, c] => quick_eval_3(id, a, b, c),
        _ => None,
    }
}

/// A strict `in_array()` of a null, bool, int, float or string needle: the
/// identity test of each element (behind a reference: the referenced
/// value), which for these types is a comparison of the same type's values
/// (`fast_is_identical_function`). `None` for any other needle and for a
/// reference being written.
///
/// Out of line, with a loop of its own for a string needle: inlined into
/// the tape's `frameless_quick_n`, whose other paths (the one-argument
/// trim and case conversions among them) keep many values in registers,
/// the loop saved and restored about a dozen of them around each
/// element's `memcmp` call (in_array('d', $five, true): 467 instructions
/// before those paths were added, 506 after).
#[inline(never)]
fn in_array_identical(needle: &Value, haystack: &ZArray) -> Option<bool> {
    let same = |v: &Value| match (needle, v) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        #[allow(clippy::float_cmp)] // `Z_DVAL_P(op1) == Z_DVAL_P(op2)`
        (Value::Float(a), Value::Float(b)) => a == b,
        _ => false,
    };
    match needle {
        Value::Str(n) => return in_array_str(n, haystack),
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) => {}
        _ => return None,
    }
    for v in haystack.values() {
        let hit = match v {
            Value::Reference(r) => same(&*r.try_borrow()?),
            v => same(v),
        };
        if hit {
            return Some(true);
        }
    }
    Some(false)
}

/// A loose `in_array()` (`==`) where no element needs a conversion: a
/// string needle among strings (`zend_fast_equal_strings`: the same
/// string, else equal bytes when either starts above `'9'`, so neither can
/// be numeric, else the numeric comparison of `zendi_smart_str_equals`),
/// an int needle among ints. `None` for any other needle or element
/// (references included): the handler compares those.
fn in_array_loose(needle: &Value, haystack: &ZArray) -> Option<bool> {
    match needle {
        Value::Str(n) => {
            let x = n.as_bytes();
            let x_text = x.first().is_some_and(|&c| c > b'9');
            for v in haystack.values() {
                let Value::Str(s) = v else {
                    return None;
                };
                if ZStr::ptr_eq(n, s) {
                    return Some(true);
                }
                let y = s.as_bytes();
                let equal = if x_text || y.first().is_some_and(|&c| c > b'9') {
                    x == y
                } else {
                    php_runtime::ops::smart_streq(x, y)
                };
                if equal {
                    return Some(true);
                }
            }
            Some(false)
        }
        Value::Int(i) => {
            for v in haystack.values() {
                let Value::Int(j) = v else {
                    return None;
                };
                if i == j {
                    return Some(true);
                }
            }
            Some(false)
        }
        _ => None,
    }
}

/// [`in_array_identical`] of the string `needle`: `zend_string_equals`
/// of each string element (the same string first, then the length, then
/// the bytes).
fn in_array_str(needle: &ZStr, haystack: &ZArray) -> Option<bool> {
    let n = needle.as_bytes();
    let eq = |s: &ZStr| ZStr::ptr_eq(s, needle) || s.as_bytes() == n;
    for v in haystack.values() {
        let hit = match v {
            Value::Str(s) => eq(s),
            Value::Reference(r) => matches!(&*r.try_borrow()?, Value::Str(s) if eq(s)),
            _ => false,
        };
        if hit {
            return Some(true);
        }
    }
    Some(false)
}

impl Vm {
    /// `ZEND_FRAMELESS_ICALL_n` / `ZEND_DO_ICALL` of the pending call when it
    /// is a [`frameless_call`] whose arguments are all in the call's inline
    /// buffer: the call is read where it lies in the caller's `calls`
    /// (not moved out, as [`Vm::call_builtin`] takes it, three times), its
    /// arguments move into the handler's `Args`, and the rest of it is
    /// released before the handler runs, as [`Vm::call_builtin_frameless`]
    /// releases it. `abs()`, `ord()` and `chr()` of one argument need no
    /// handler at all ([`frameless_inline`]). Each step is the one the
    /// general path takes, in the same order: the handler, the interrupt
    /// check after it, the buffers back to their pools.
    ///
    /// Returns `None`, having changed nothing, for any other pending call
    /// and while observers are enabled (they see every internal call).
    /// `dst` is the call's result temporary (`pending_call_dst`).
    #[inline]
    #[allow(clippy::too_many_lines)] // one call's steps in the handler's order
    pub(crate) fn frameless_icall_in_place(&mut self, dst: Option<u32>) -> Option<VmResult<Value>> {
        if self.observer.enabled {
            return None;
        }
        let frame = self.frames.last_mut()?;
        let call = frame.calls.last_mut()?;
        let Func::Builtin(b) = &call.func else {
            return None;
        };
        let class = (b.flags & flags::FRAMELESS) >> flags::FRAMELESS_SHIFT;
        if class == 0 || b.flags & (flags::DEPRECATED | flags::NODISCARD) != 0 {
            return None;
        }
        let n = usize::from(call.inline_count);
        let sent = call.inline_args.get(..n)?;
        if !call.args.is_empty() || !plain_call(call) || !frameless_args(class, sent) {
            return None;
        }
        // `in_array()`, `array_key_exists()` and the `str_contains()`
        // family: answered by
        // [`Vm::frameless_two`], the pending call popped as the handler's
        // call would pop it (its argument buffer back to the pool).
        if matches!(class, fl::IN_ARRAY | fl::STR_TEST | fl::KEY_EXISTS) && (2..=3).contains(&n) {
            let b = Rc::clone(b);
            let mut sent: [Option<Value>; 3] = [None, None, None];
            for (slot, arg) in sent.iter_mut().zip(call.inline_args[..n].iter_mut()) {
                *slot = arg.take();
            }
            let PendingCall { args: spare, .. } = frame.calls.pop()?;
            let refs = [sent[0].as_ref(), sent[1].as_ref(), sent[2].as_ref()];
            let Some(r) = self.frameless_two(b.name.as_bytes(), class, &refs[..n]) else {
                unreachable!("frameless_two answers the arguments frameless_args accepts")
            };
            drop(sent);
            drop(b);
            let r = r.and_then(|v| self.check_interrupt().map(|()| v));
            if spare.capacity() > 0 {
                self.put_arg_vec(spare);
            }
            return Some(r);
        }
        let quick = if n == 1 {
            frameless_inline(b.name.as_bytes(), class, sent[0].as_ref())
        } else {
            None
        };
        let (mut values, mut passed) = if quick.is_none() {
            let nparams = b.params.len();
            let mut values = self.call_bufs.values.pop().unwrap_or_default();
            values.reserve(nparams.max(n));
            let mut passed = self.call_bufs.passed.pop().unwrap_or_default();
            passed.reserve(nparams.max(n));
            for arg in &mut call.inline_args[..n] {
                values.push(arg.take().unwrap_or_default());
                passed.push(true);
            }
            (values, passed)
        } else {
            (Vec::new(), Vec::new())
        };
        // An inline result's argument (the others moved into `values`):
        // every inline slot is empty from here on.
        let held = if quick.is_some() {
            call.inline_args[0].take()
        } else {
            None
        };
        // The call is popped and taken apart field by field rather than
        // dropped where it lies (`truncate`), whose drop glue made an
        // out-of-line `drop_in_place` call per field (~140 instructions a
        // call) for fields that hold nothing: a [`plain_call`] has no
        // `$this`, scopes, closure, `__call` name, partial application or
        // named arguments, and its inline arguments were all taken above.
        // What it owns is its function handle (moved into the handler's
        // `Args`, where the clone of it was) and its argument buffer
        // (empty: back to the pool). Nothing is leaked: the forgotten
        // fields are `None` or unallocated.
        let PendingCall {
            func,
            args: spare,
            named,
            inline_args,
            this,
            scope,
            called,
            closure,
            magic_name,
            pfa,
            ..
        } = frame.calls.pop().expect("pending call");
        debug_assert!(
            this.is_none()
                && scope.is_none()
                && called.is_none()
                && closure.is_none()
                && magic_name.is_none()
                && pfa.is_none()
                && named.is_empty()
                && spare.is_empty()
                && inline_args.iter().all(Option::is_none)
        );
        std::mem::forget((this, scope, called, closure, magic_name, pfa, inline_args));
        forget_unallocated(named);
        let spare = if spare.capacity() > 0 {
            Some(spare)
        } else {
            std::mem::forget(spare);
            None
        };
        let b = match func {
            Func::Builtin(b) => b,
            // The pending call's function was matched as a builtin above.
            Func::User(_) => unreachable!("frameless call of a user function"),
        };
        if let Some(v) = quick {
            // What the old in-place drop released: the function handle and
            // the argument (a scalar or a string: no side effect).
            drop(b);
            drop(held);
            // `fast_builtin_call`'s order: the interrupt check, then the
            // argument buffer back to its pool.
            let r = self.check_interrupt().map(|()| v);
            if let Some(args) = spare {
                self.put_arg_vec(args);
            }
            return Some(r);
        }
        self.pending_call_dst = dst;
        if self.magic_call_name.is_some() {
            self.magic_call_name = None;
        }
        // The declared defaults of omitted trailing parameters, as for a
        // framed call ([`Vm::builtin_default`]); a literal default, the
        // common case, is copied here without the out-of-line call. The
        // signature is read once (each `b.field` goes through its
        // `OnceCell`).
        let sig = b.signature();
        let (nparams, required) = (sig.params.len(), sig.required as usize);
        while values.len() < nparams {
            let i = values.len();
            if let (true, Some(Some(d))) = (i >= required, sig.defaults.get(i)) {
                values.push(crate::call::clone_value(d));
                passed.push(false);
                continue;
            }
            match self.builtin_default(&b, i) {
                Some(d) if i >= required => {
                    values.push(d);
                    passed.push(false);
                }
                _ => break,
            }
        }
        let handler = b.handler;
        let mut args = Args {
            values,
            passed,
            named: Vec::new(),
            this: None,
            called: None,
            func: Some(b),
        };
        let mut r = handler(self, &mut args);
        // `ZEND_VM_FCALL_INTERRUPT_CHECK`, after the call.
        if r.is_ok() {
            if let Err(e) = self.check_interrupt() {
                r = Err(e);
            }
        }
        let Args {
            values,
            passed,
            named,
            ..
        } = args;
        forget_unallocated(named);
        self.put_val_vec(values);
        self.put_bool_vec(passed);
        if let Some(args) = spare {
            self.put_arg_vec(args);
        }
        Some(r)
    }
}

impl Vm {
    /// `ZEND_FRAMELESS_ICALL_n` of a call the tape prepared
    /// (`exec/tape/calls.rs`): builtin `b` has a frameless class and its
    /// `n` arguments were sent into the argument buffer of the pooled frame
    /// above the current one. When they have shapes [`frameless_args`]
    /// accepts, the handler runs on the caller's frame with the arguments
    /// moved out of that buffer (which is left empty, as a pooled frame's
    /// is): no frame is pushed. Each step is the one
    /// [`Vm::frameless_icall_op`] takes for the same call, in the same
    /// order: `caller_ip` saved as the resume point (`SAVE_OPLINE()`), the
    /// inline result or the handler with the declared defaults of omitted
    /// parameters, the interrupt check, the buffers back to their pools,
    /// the result stored in `dst`.
    ///
    /// Returns `None`, having changed nothing, for any other call (the
    /// tape then makes it with the builtin's frame). `calls_len` is the
    /// number of pending calls the caller had when the call was prepared:
    /// the call is its newest.
    #[allow(clippy::too_many_lines)] // one call's steps in the handler's order
    pub(crate) fn frameless_icall_prepared(
        &mut self,
        b: &Rc<Builtin>,
        n: usize,
        calls_len: usize,
        dst: Option<u32>,
        caller_ip: usize,
    ) -> Option<VmResult<()>> {
        let class = (b.flags & flags::FRAMELESS) >> flags::FRAMELESS_SHIFT;
        if class == 0 || b.flags & (flags::DEPRECATED | flags::NODISCARD) != 0 {
            return None;
        }
        let live = self.frames.live;
        let (callers, pool) = self.frames.vec.split_at_mut(live);
        let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
            return None;
        };
        if caller.calls.len() != calls_len
            || frame.args.len() != n
            || !frameless_args(class, &frame.args)
        {
            return None;
        }
        caller.ip = caller_ip;
        let quick = if n == 1 {
            frameless_inline(b.name.as_bytes(), class, frame.args[0].as_ref())
        } else {
            None
        };
        if let Some(v) = quick {
            // The argument: a number or a string (no side effect).
            frame.args.clear();
            if let Err(e) = self.check_interrupt() {
                return Some(Err(e));
            }
            self.put_call_result(dst, v);
            return Some(Ok(()));
        }
        // `in_array()`, `array_key_exists()` and the `str_contains()`
        // family, whose accepted
        // arguments [`Vm::frameless_two`] always answers: taken out of the
        // frame, as the handler's call would leave it.
        if matches!(class, fl::IN_ARRAY | fl::STR_TEST | fl::KEY_EXISTS) && (2..=3).contains(&n) {
            let mut sent: [Option<Value>; 3] = [None, None, None];
            for (slot, arg) in sent.iter_mut().zip(frame.args.drain(..)) {
                *slot = arg;
            }
            let refs = [sent[0].as_ref(), sent[1].as_ref(), sent[2].as_ref()];
            let Some(r) = self.frameless_two(b.name.as_bytes(), class, &refs[..n]) else {
                unreachable!("frameless_two answers the arguments frameless_args accepts")
            };
            drop(sent);
            let v = match r {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if let Err(e) = self.check_interrupt() {
                return Some(Err(e));
            }
            self.put_call_result(dst, v);
            return Some(Ok(()));
        }
        self.frameless_handler_call(b, n, dst, live)
    }

    /// The call of leaf builtin `b` (`leaf_id`) whose `n` arguments were
    /// sent into the argument buffer of the pooled frame above the current
    /// one (an argument computed between the call's start and its sends,
    /// `assert($x instanceof C)`, which `calls::leaf_now` cannot read
    /// ahead): when they have a shape of its leaf class
    /// ([`Vm::leaf_args`]: the handler then reports nothing, throws
    /// nothing, runs no PHP code and looks at no frame), the handler runs
    /// on the caller's frame as a frameless function's does
    /// ([`Vm::frameless_handler_call`]), `caller_ip` saved as the resume
    /// point. `None`, nothing changed, for any other call.
    pub(crate) fn leaf_prepared(
        &mut self,
        b: &Rc<Builtin>,
        n: usize,
        calls_len: usize,
        dst: Option<u32>,
        caller_ip: usize,
    ) -> Option<VmResult<()>> {
        const MAX: usize = 4;
        let id = b.leaf_id();
        if id == leaf::NONE
            || b.flags & (flags::DEPRECATED | flags::NODISCARD) != 0
            || self.observer.enabled
            || n > MAX
        {
            return None;
        }
        let live = self.frames.live;
        {
            let caller = self.frames.vec.get(live.checked_sub(1)?)?;
            let frame = self.frames.vec.get(live)?;
            if caller.calls.len() != calls_len || frame.args.len() != n {
                return None;
            }
            let mut sent: [&Value; MAX] = [&Value::Null; MAX];
            for (slot, arg) in sent.iter_mut().zip(&frame.args) {
                *slot = arg.as_ref()?;
            }
            if !self.leaf_args(id, &sent[..n]) {
                return None;
            }
        }
        if let Some(caller) = self.frames.vec.get_mut(live - 1) {
            caller.ip = caller_ip;
        }
        self.frameless_handler_call(b, n, dst, live)
    }

    /// The handler of builtin `b` called on the caller's frame with the
    /// `n` arguments sent into the argument buffer of the pooled frame at
    /// `live` (moved out of it: the buffer is left empty) and the declared
    /// defaults of the omitted trailing parameters; then the interrupt
    /// check, the buffers back to their pools and the result stored in
    /// `dst`. The caller has saved the resume point and settled that the
    /// handler cannot see that it has no frame.
    pub(crate) fn frameless_handler_call(
        &mut self,
        b: &Rc<Builtin>,
        n: usize,
        dst: Option<u32>,
        live: usize,
    ) -> Option<VmResult<()>> {
        // The arguments move from the frame's buffer into a pooled
        // record taken whole for the call (`CallBufs::take_args`), the
        // length stored once, and the emptied buffer is cut without a
        // `Drain`.
        let mut args = self.call_bufs.take_args();
        args.values.reserve(b.signature().params.len().max(n));
        {
            let sent = &mut self.frames.vec[live].args;
            args.values
                .extend(sent.iter_mut().map(|arg| arg.take().unwrap_or_default()));
            sent.clear();
        }
        Some(self.frameless_handler_run(b, args, dst))
    }

    /// [`Vm::frameless_handler_call`] with the sent arguments already in
    /// `args.values` (a record of `CallBufs::take_args`).
    pub(crate) fn frameless_handler_run(
        &mut self,
        b: &Rc<Builtin>,
        mut args: Box<crate::func::Args>,
        dst: Option<u32>,
    ) -> VmResult<()> {
        // The declared defaults of omitted trailing parameters, read as
        // in [`Vm::frameless_icall_in_place`].
        let sig = b.signature();
        let (nparams, required) = (sig.params.len(), sig.required as usize);
        push_passed(&mut args.passed, args.values.len());
        self.pending_call_dst = dst;
        if self.magic_call_name.is_some() {
            self.magic_call_name = None;
        }
        while args.values.len() < nparams {
            let i = args.values.len();
            if let (true, Some(Some(d))) = (i >= required, sig.defaults.get(i)) {
                args.values.push(crate::call::clone_value(d));
                args.passed.push(false);
                continue;
            }
            match self.builtin_default(b, i) {
                Some(d) if i >= required => {
                    args.values.push(d);
                    args.passed.push(false);
                }
                _ => break,
            }
        }
        let handler = b.handler;
        args.func = Some(Rc::clone(b));
        let mut r = handler(self, &mut args);
        // `ZEND_VM_FCALL_INTERRUPT_CHECK`, after the call.
        if r.is_ok() {
            if let Err(e) = self.check_interrupt() {
                r = Err(e);
            }
        }
        // Emptied (`Args::release_in_place`), back to the pool.
        args.release_in_place();
        self.call_bufs.put_args(args);
        let v = r?;
        self.put_call_result(dst, v);
        Ok(())
    }

    /// Stores a call's result in its temporary, or releases it when the
    /// result is unused (a scalar without the out-of-line drop call).
    ///
    /// Always inline, and called directly, not from a closure given to
    /// `Result::map` (which came out of line: ~33 instructions a call).
    #[inline(always)]
    #[allow(clippy::inline_always)] // once per frameless call site
    pub(crate) fn put_call_result(&mut self, dst: Option<u32>, v: Value) {
        match dst {
            Some(d) => {
                let Some(slot) = self
                    .frames
                    .last_mut()
                    .and_then(|f| f.tmps.get_mut(d as usize))
                else {
                    drop(v);
                    return;
                };
                let old = std::mem::replace(slot, v);
                if matches!(
                    old,
                    Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
                ) {
                    std::mem::forget(old);
                } else {
                    drop(old);
                }
            }
            None => {
                if matches!(
                    v,
                    Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
                ) {
                    std::mem::forget(v);
                } else {
                    drop(v);
                }
            }
        }
    }
}

impl Vm {
    /// The handler of a two- or three-argument frameless call evaluated
    /// without the handler's argument and result vectors: `in_array()`,
    /// `array_key_exists()` and the `str_contains()` family, as their handlers
    /// compute them (`php_stdarr::basic::in_array`, `php_stdstr::search`),
    /// for arguments [`frameless_args`] accepted for `class`. `None` for
    /// every other call (the handler then runs).
    #[inline]
    fn frameless_two(
        &mut self,
        name: &[u8],
        class: u32,
        args: &[Option<&Value>],
    ) -> Option<VmResult<Value>> {
        use php_stdstr::search;
        match (class, args) {
            (fl::STR_TEST, [Some(Value::Str(h)), Some(Value::Str(n))]) => {
                let (h, n) = (h.as_bytes(), n.as_bytes());
                let r = match name {
                    b"str_contains" => search::str_contains(h, n),
                    b"str_starts_with" => search::str_starts_with(h, n),
                    b"str_ends_with" => search::str_ends_with(h, n),
                    _ => return None,
                };
                Some(Ok(Value::Bool(r)))
            }
            (fl::IN_ARRAY, [Some(needle), Some(Value::Array(haystack))]) => Some(
                php_stdarr::basic::in_array(self, needle, haystack, false)
                    .map(Value::Bool)
                    .map_err(|e| self.rt_throw(e)),
            ),
            // `array_key_exists()`'s lookups (a numeric string finds its int
            // key), by the key's cached hash.
            (fl::KEY_EXISTS, [Some(Value::Str(k)), Some(Value::Array(a))]) => {
                Some(Ok(Value::Bool(a.get_sym_zstr(k).is_some())))
            }
            (fl::KEY_EXISTS, [Some(Value::Int(k)), Some(Value::Array(a))]) => {
                Some(Ok(Value::Bool(a.get_int(*k).is_some())))
            }
            (
                fl::IN_ARRAY,
                [Some(needle), Some(Value::Array(haystack)), Some(Value::Bool(strict))],
            ) => Some(
                php_stdarr::basic::in_array(self, needle, haystack, *strict)
                    .map(Value::Bool)
                    .map_err(|e| self.rt_throw(e)),
            ),
            _ => None,
        }
    }

    /// [`Vm::frameless_icall_op`] of a two- or three-argument call of
    /// builtin `b` (frameless class `class`) answered without a handler,
    /// the arguments read in place (literals, variables or temporaries that
    /// are no references, of a shape [`frameless_args_ref`] accepts): the
    /// `str_*` tests and `array_key_exists()` ([`Vm::frameless_two`]'s
    /// lookups), `strpos()`, `substr()`, `max()`, `min()` and strict
    /// `in_array()` (`quick_eval_2`/`_3`), and loose `in_array()` (with
    /// copies of its two arguments: it compares with the VM at hand). The
    /// result is computed before anything changes; then the temporaries
    /// are released, the resume point saved, the interrupt checked and the
    /// result stored. `None`, nothing changed, otherwise.
    #[inline]
    fn frameless_many(
        &mut self,
        b: &Builtin,
        class: u32,
        fc: &FramelessCall,
        ip: usize,
    ) -> Option<VmResult<()>> {
        enum Got {
            Done(Value),
            InArray(Value, Value, bool),
        }
        let n = usize::from(fc.argc).min(3);
        let name = b.name.as_bytes();
        let got = {
            let frame = self.frames.last()?;
            let mut refs: [&Value; 3] = [&Value::Null; 3];
            for (slot, operand) in refs.iter_mut().zip(&fc.vals[..n]) {
                let v = match *operand {
                    Operand::Lit(l) => frame.code.literals.get(l as usize)?,
                    Operand::Cv(cv) => frame.cvs.get(cv as usize)?.get()?,
                    Operand::Tmp(t) => frame.tmps.get(t as usize)?,
                    _ => return None,
                };
                if matches!(v, Value::Reference(_)) {
                    return None;
                }
                *slot = v;
            }
            let args = &refs[..n];
            if !frameless_args_ref(class, args) {
                return None;
            }
            match (class, args) {
                (fl::STR_TEST, [Value::Str(h), Value::Str(nd)]) => {
                    let (h, nd) = (h.as_bytes(), nd.as_bytes());
                    Got::Done(Value::Bool(match name {
                        b"str_contains" => php_stdstr::search::str_contains(h, nd),
                        b"str_starts_with" => php_stdstr::search::str_starts_with(h, nd),
                        b"str_ends_with" => php_stdstr::search::str_ends_with(h, nd),
                        _ => return None,
                    }))
                }
                (fl::KEY_EXISTS, [Value::Str(k), Value::Array(a)]) => {
                    Got::Done(Value::Bool(a.get_sym_zstr(k).is_some()))
                }
                (fl::KEY_EXISTS, [Value::Int(k), Value::Array(a)]) => {
                    Got::Done(Value::Bool(a.get_int(*k).is_some()))
                }
                (fl::IN_ARRAY, [needle, haystack] | [needle, haystack, Value::Bool(false)]) => {
                    Got::InArray((*needle).clone(), (*haystack).clone(), false)
                }
                _ => {
                    let (id, _) = quick_id_n(name, n)?;
                    Got::Done(match args {
                        [a, b2] => quick_eval_2(id, a, b2)?,
                        [a, b2, c] => quick_eval_3(id, a, b2, c)?,
                        _ => return None,
                    })
                }
            }
        };
        let v = match got {
            Got::Done(v) => v,
            Got::InArray(needle, Value::Array(h), strict) => {
                match php_stdarr::basic::in_array(self, &needle, &h, strict) {
                    Ok(found) => Value::Bool(found),
                    Err(e) => return Some(Err(self.rt_throw(e))),
                }
            }
            Got::InArray(..) => return None,
        };
        let frame = self.frames.last_mut()?;
        // Temporary arguments are consumed (their `Send`s would have moved
        // them).
        for operand in &fc.vals[..n] {
            if let Operand::Tmp(t) = *operand {
                if let Some(slot) = frame.tmps.get_mut(t as usize) {
                    drop(std::mem::take(slot));
                }
            }
        }
        frame.ip = ip;
        if let Err(e) = self.check_interrupt() {
            return Some(Err(e));
        }
        if let Some(d) = fc.dst {
            self.set_tmp(d, v);
        }
        Some(Ok(()))
    }

    /// [`Vm::frameless_icall_op`] of a one-argument call of builtin `b`
    /// (frameless class `class`, one [`inline_class`] names) answered
    /// without a handler ([`frameless_inline`]: the type tests, `strlen()`,
    /// `count()`, `abs()`, `ord()`, `chr()`, the math functions, ...), the
    /// argument read in place: a literal, a variable or a temporary that is
    /// no reference, of a shape [`frameless_arg1`] accepts. The result is
    /// computed before anything changes; then the temporary is released,
    /// the resume point saved, the interrupt checked and the result
    /// stored. `None`, nothing changed, otherwise: the general path runs.
    #[inline]
    fn frameless_one(
        &mut self,
        b: &Builtin,
        class: u32,
        fc: &FramelessCall,
        ip: usize,
    ) -> Option<VmResult<()>> {
        let v = {
            let frame = self.frames.last()?;
            let arg = match fc.vals[0] {
                Operand::Lit(l) => frame.code.literals.get(l as usize)?,
                Operand::Cv(cv) => frame.cvs.get(cv as usize)?.get()?,
                Operand::Tmp(t) => frame.tmps.get(t as usize)?,
                _ => return None,
            };
            if matches!(arg, Value::Reference(_)) || !frameless_arg1(class, arg) {
                return None;
            }
            match class {
                fl::TYPE_TEST | fl::OBJECT_ARG | fl::STRLEN | fl::COUNT => {
                    crate::call::fast_eval(b.fast_eval_id(), arg)?
                }
                _ => frameless_inline(b.name.as_bytes(), class, Some(arg))?,
            }
        };
        let frame = self.frames.last_mut()?;
        // A temporary argument is consumed (its `Send` would have moved it).
        if let Operand::Tmp(t) = fc.vals[0] {
            if let Some(slot) = frame.tmps.get_mut(t as usize) {
                drop(std::mem::take(slot));
            }
        }
        frame.ip = ip;
        if let Err(e) = self.check_interrupt() {
            return Some(Err(e));
        }
        if let Some(d) = fc.dst {
            self.set_tmp(d, v);
        }
        Some(Ok(()))
    }

    /// The first run of a namespaced frameless call site
    /// ([`FramelessCall::ns`]): as its `InitFCall` would resolve it
    /// (`Vm::op_init_fcall`). A namespaced function cached for the name or
    /// declared is the call's (`None`: the call's own ops run and resolve
    /// it, as when no function of either name exists); otherwise the global function, cached in the site's slot
    /// `slot` for the rest of the request, as `ZEND_JMP_FRAMELESS` and
    /// `ZEND_INIT_NS_FCALL_BY_NAME` cache their outcome.
    #[cold]
    #[inline(never)]
    fn frameless_ns_first_run(&mut self, ns: u32, global: u32, slot: usize) -> Option<Func> {
        let code = Rc::clone(&self.frame().code);
        let by_literal = code.fcall_cache.borrow().get(ns as usize).is_some_and(|c| {
            c.as_ref()
                .and_then(crate::opcode::CachedFunc::get)
                .is_some()
        });
        if by_literal || self.functions.contains_key(&self.lit_str(ns)) {
            return None;
        }
        let func = self.functions.get(&self.lit_str(global)).cloned()?;
        let size = (code.literals.len() + code.ops.len()).max(slot + 1);
        let mut cache = code.fcall_cache.borrow_mut();
        if cache.len() <= slot {
            crate::rtcache::grow_slots(&mut cache, size, || None);
        }
        cache[slot] = Some(crate::opcode::CachedFunc::new(&func));
        Some(func)
    }

    /// [`Op::FramelessICall`](crate::opcode::Op::FramelessICall) in
    /// place (`ZEND_FRAMELESS_ICALL_n`): when the call site resolved to a
    /// builtin with a frameless class (its run-time cache entry, filled by
    /// the first, generic run of the op) and the arguments (literals,
    /// defined variables of a frame without an attached symbol table, and
    /// the temporaries of arguments computed before the op) have
    /// shapes [`frameless_args`] accepts, the handler runs on the caller's
    /// frame with the arguments read straight from their operands: no
    /// pending call, no `SEND`s, no `DO_ICALL`. Each step is the one
    /// [`Vm::frameless_icall_in_place`] takes for the same call, in the
    /// same order: the inline result or the handler with the declared
    /// defaults of omitted parameters, the interrupt check, the buffers
    /// back to their pools, the result stored in `dst`. `ip` (the op after
    /// this one) is saved first, as `SAVE_OPLINE()`.
    ///
    /// Returns `None`, having changed nothing, for any other call and
    /// while observers are enabled: the op then runs the call's ops
    /// (`INIT_FCALL`, `SEND`s, `DO_ICALL`) one after the other.
    #[inline]
    #[allow(clippy::too_many_lines)] // one call's steps in the handler's order
    pub(crate) fn frameless_icall_op(
        &mut self,
        fc: &FramelessCall,
        ip: usize,
    ) -> Option<VmResult<()>> {
        if self.observer.enabled {
            return None;
        }
        let frame = self.frames.last_mut()?;
        if frame.symtab_attached {
            return None;
        }
        // The builtin's handle straight from its cache entry, tested
        // before it is copied (`CachedFunc::get` built a `Func` first).
        let b = match fc.ns {
            None => {
                let cache = frame.code.fcall_cache.borrow();
                let Some(Some(entry)) = cache.get(fc.name as usize) else {
                    return None;
                };
                let crate::opcode::CachedFuncKind::Builtin(b) = &entry.func else {
                    return None;
                };
                if b.flags & flags::FRAMELESS == 0
                    || b.flags & (flags::DEPRECATED | flags::NODISCARD) != 0
                {
                    return None;
                }
                Rc::clone(b)
            }
            // `ZEND_JMP_FRAMELESS`: the call site's slot, where its
            // `InitFCall` keeps the global fallback it resolved to
            // (`Vm::op_init_fcall`, at the same `ip`), decided on the site's
            // first run as there.
            Some(ns) => {
                let slot = frame.code.literals.len() + ip;
                // The site's builtin handle read from its entry in place
                // (`CachedFunc::get` built a whole `Func` per call).
                let cached = {
                    let cache = frame.code.fcall_cache.borrow();
                    match cache.get(slot) {
                        Some(Some(entry)) => match &entry.func {
                            crate::opcode::CachedFuncKind::Builtin(b) => Some(Some(Rc::clone(b))),
                            _ => Some(None),
                        },
                        _ => None,
                    }
                };
                let b = match cached {
                    Some(Some(b)) => b,
                    // A user function took the name: the generic ops.
                    Some(None) => return None,
                    None => match self.frameless_ns_first_run(ns, fc.name, slot)? {
                        Func::Builtin(b) => b,
                        Func::User(_) => return None,
                    },
                };
                if b.flags & flags::FRAMELESS == 0
                    || b.flags & (flags::DEPRECATED | flags::NODISCARD) != 0
                {
                    return None;
                }
                b
            }
        };
        let class = (b.flags & flags::FRAMELESS) >> flags::FRAMELESS_SHIFT;
        if fc.argc == 1 && inline_class(class) {
            if let Some(r) = self.frameless_one(&b, class, fc, ip) {
                return Some(r);
            }
        }
        if fc.argc >= 2 {
            if let Some(r) = self.frameless_many(&b, class, fc, ip) {
                return Some(r);
            }
        }
        let frame = self.frames.last_mut()?;
        let n = usize::from(fc.argc).min(3);
        // What `SEND_VAL` / `SEND_VAR` would put in the call (`send_in`),
        // written once, where the handler reads it: a literal's copy, a
        // variable's value (a reference's target). The record is a pooled
        // one taken whole (`CallBufs::take_args`) and given back on a
        // decline.
        let mut args = self.call_bufs.take_args();
        let mut read = true;
        for (i, operand) in fc.vals[..n].iter().enumerate() {
            let v = match *operand {
                Operand::Lit(l) => frame
                    .code
                    .literals
                    .get(l as usize)
                    .map(crate::func::copy_arg),
                Operand::Cv(cv) => frame
                    .cvs
                    .get(cv as usize)
                    .and_then(crate::vm::CvSlot::get)
                    .map(|v| match v {
                        Value::Reference(r) => r.get(),
                        v => crate::func::copy_arg(v),
                    }),
                // An argument computed before the op: a copy for now; the
                // temporary is released below, once the call is known to
                // run here (its `Send` would have moved it).
                Operand::Tmp(t) => frame.tmps.get(t as usize).map(|v| match v {
                    Value::Reference(r) => r.get(),
                    v => crate::func::copy_arg(v),
                }),
                // `$this->name` / `$cv->name` (`specialise::prop_arg_place`):
                // the property cache hit's value, as the `Send` reads it on
                // its hit (no side effect: on a decline the `Send` reads it
                // again, a miss's warning or `__get()` included).
                Operand::Unused => match &fc.args[i] {
                    crate::opcode::ArgSrc::Place(pid) => frame.send_place_hit(*pid),
                    _ => None,
                },
                _ => None,
            };
            let Some(v) = v else {
                read = false;
                break;
            };
            args.values.push(v);
        }
        if !read || !frameless_values(class, &args.values) {
            args.values.clear();
            self.call_bufs.put_args(args);
            return None;
        }
        for operand in &fc.vals[..n] {
            if let Operand::Tmp(t) = *operand {
                if let Some(slot) = frame.tmps.get_mut(t as usize) {
                    drop(std::mem::take(slot));
                }
            }
        }
        frame.ip = ip;
        let quick = match n {
            1 => frameless_inline(b.name.as_bytes(), class, args.values.first()).map(Ok),
            // `in_array()`, `array_key_exists()` and the `str_*` tests,
            // then what the tape's quick rows answer for global names
            // (`strpos()`, `substr()`, `max()`, `min()`, strict `in_array()`:
            // `quick_eval_2`/`_3`, `None` for shapes they do not take).
            2 | 3 => {
                let sent = [args.values.first(), args.values.get(1), args.values.get(2)];
                self.frameless_two(b.name.as_bytes(), class, &sent[..n])
                    .or_else(|| {
                        let (id, _) = quick_id_n(b.name.as_bytes(), n)?;
                        let v = match &args.values[..] {
                            [a, b] => quick_eval_2(id, a, b),
                            [a, b, c] => quick_eval_3(id, a, b, c),
                            _ => None,
                        }?;
                        Some(Ok(v))
                    })
            }
            _ => None,
        };
        if let Some(v) = quick {
            drop(b);
            args.values.clear();
            self.call_bufs.put_args(args);
            let v = match v {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if let Err(e) = self.check_interrupt() {
                return Some(Err(e));
            }
            if let Some(d) = fc.dst {
                self.set_tmp(d, v);
            }
            return Some(Ok(()));
        }
        self.pending_call_dst = fc.dst;
        if self.magic_call_name.is_some() {
            self.magic_call_name = None;
        }
        // The declared defaults of omitted trailing parameters, read as
        // in [`Vm::frameless_icall_in_place`].
        let sig = b.signature();
        let (nparams, required) = (sig.params.len(), sig.required as usize);
        push_passed(&mut args.passed, n);
        while args.values.len() < nparams {
            let i = args.values.len();
            if let (true, Some(Some(d))) = (i >= required, sig.defaults.get(i)) {
                args.values.push(crate::call::clone_value(d));
                args.passed.push(false);
                continue;
            }
            match self.builtin_default(&b, i) {
                Some(d) if i >= required => {
                    args.values.push(d);
                    args.passed.push(false);
                }
                _ => break,
            }
        }
        let handler = b.handler;
        args.func = Some(b);
        let mut r = handler(self, &mut args);
        // `ZEND_VM_FCALL_INTERRUPT_CHECK`, after the call.
        if r.is_ok() {
            if let Err(e) = self.check_interrupt() {
                r = Err(e);
            }
        }
        // Emptied (`Args::release_in_place`), back to the pool.
        args.release_in_place();
        self.call_bufs.put_args(args);
        let v = match r {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        self.put_call_result(fc.dst, v);
        Some(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        fl, frameless_arg1, frameless_args, frameless_args_ref, frameless_call, frameless_flags,
        frameless_inline, leaf,
    };

    /// `mb_strlen()` and `mb_substr()` with a null encoding (the default
    /// one, as Laravel's `Str::length($value, $encoding = null)` passes it)
    /// are leaf calls; an encoding name other than UTF-8, a non-string
    /// subject and a non-int offset are not.
    #[test]
    fn mbstring_leaf_shapes_take_a_null_encoding() {
        use php_runtime::ZStr;
        let vm = crate::vm::Vm::new(&crate::VmOptions::default(), Box::new(std::io::sink()));
        let s = Value::Str(ZStr::from("héllo"));
        let utf8 = Value::Str(ZStr::from("UTF-8"));
        let other = Value::Str(ZStr::from("ISO-8859-1"));
        let (one, null) = (Value::Int(1), Value::Null);
        assert!(vm.leaf_args(leaf::MB_STRLEN, &[&s]));
        assert!(vm.leaf_args(leaf::MB_STRLEN, &[&s, &null]));
        assert!(vm.leaf_args(leaf::MB_STRLEN, &[&s, &utf8]));
        assert!(!vm.leaf_args(leaf::MB_STRLEN, &[&s, &other]));
        assert!(!vm.leaf_args(leaf::MB_STRLEN, &[&one, &null]));
        assert!(vm.leaf_args(leaf::MB_SUBSTR, &[&s, &one, &null, &null]));
        assert!(vm.leaf_args(leaf::MB_SUBSTR, &[&s, &one, &one, &null]));
        assert!(vm.leaf_args(leaf::MB_SUBSTR, &[&s, &one, &null, &utf8]));
        assert!(!vm.leaf_args(leaf::MB_SUBSTR, &[&s, &one, &null, &other]));
        assert!(!vm.leaf_args(leaf::MB_SUBSTR, &[&s, &s, &null, &null]));
    }

    /// The two- and three-argument shape test read in place accepts
    /// exactly what `frameless_args` accepts, for every class.
    #[test]
    fn frameless_args_ref_is_frameless_args() {
        use php_runtime::{ZArray, ZStr};
        let values = [
            Value::Null,
            Value::Bool(true),
            Value::Bool(false),
            Value::Int(-1),
            Value::Int(0),
            Value::Int(2),
            Value::Int(i64::MIN),
            Value::Float(1.5),
            Value::Str(ZStr::from("")),
            Value::Str(ZStr::from("ab")),
            Value::Str(ZStr::from("a..z")),
            Value::Array(ZArray::new()),
            Value::Array(ZArray::from_values([Value::Int(1)])),
        ];
        for class in 0..32 {
            for a in &values {
                for b in &values {
                    assert_eq!(
                        frameless_args_ref(class, &[a, b]),
                        frameless_args(class, &[Some(a.clone()), Some(b.clone())]),
                        "class {class}: {a:?}, {b:?}"
                    );
                    for c in &values {
                        assert_eq!(
                            frameless_args_ref(class, &[a, b, c]),
                            frameless_args(
                                class,
                                &[Some(a.clone()), Some(b.clone()), Some(c.clone())]
                            ),
                            "class {class}: {a:?}, {b:?}, {c:?}"
                        );
                    }
                }
            }
        }
    }

    /// The one-argument shape test read in place accepts exactly what
    /// `frameless_args` accepts of one argument, for every class.
    #[test]
    fn frameless_arg1_is_frameless_args_of_one() {
        use php_runtime::{ZArray, ZStr};
        let values = [
            Value::Null,
            Value::Bool(true),
            Value::Int(-1),
            Value::Int(0),
            Value::Int(255),
            Value::Int(256),
            Value::Int(i64::MIN),
            Value::Float(1.5),
            Value::Str(ZStr::from("")),
            Value::Str(ZStr::from("a")),
            Value::Str(ZStr::from("ab")),
            Value::Array(ZArray::new()),
        ];
        for class in 0..32 {
            for v in &values {
                assert_eq!(
                    frameless_arg1(class, v),
                    frameless_args(class, &[Some(v.clone())]),
                    "class {class}, {v:?}"
                );
            }
        }
    }

    /// `trim()`, `ltrim()`, `rtrim()`, `strtolower()` and `strtoupper()` of
    /// one string, answered without the handler, return what the handler
    /// does (`php_trim_int`, `zend_string_tolower`): the string itself when
    /// nothing changes, the shared empty string when everything is trimmed,
    /// else a new string; anything but a string goes to the handler.
    #[test]
    fn quick_trim_and_case_match_the_handlers() {
        use super::{
            quick_eval, quick_id, QUICK_LOWER, QUICK_LTRIM, QUICK_RTRIM, QUICK_TRIM, QUICK_UPPER,
        };
        use php_runtime::{Value, ZStr};
        let eval = |id: u8, s: &ZStr| match quick_eval(id, &Value::Str(s.clone())) {
            Some(Value::Str(r)) => r,
            other => panic!("{id}: {other:?}"),
        };
        assert_eq!(quick_id(b"trim"), Some((QUICK_TRIM, fl::TRIM)));
        assert_eq!(quick_id(b"ltrim"), Some((QUICK_LTRIM, fl::TRIM)));
        assert_eq!(quick_id(b"rtrim"), Some((QUICK_RTRIM, fl::TRIM)));
        assert_eq!(quick_id(b"strtolower"), Some((QUICK_LOWER, fl::CASE)));
        assert_eq!(quick_id(b"strtoupper"), Some((QUICK_UPPER, fl::CASE)));
        let padded = ZStr::from(" \t\n\r\x0b\0 ab c \0\n");
        assert_eq!(eval(QUICK_TRIM, &padded).as_bytes(), b"ab c");
        assert_eq!(eval(QUICK_LTRIM, &padded).as_bytes(), b"ab c \0\n");
        assert_eq!(eval(QUICK_RTRIM, &padded).as_bytes(), b" \t\n\r\x0b\0 ab c");
        // `\x0c` (form feed) is in php-src master's default list
        // (`php_trim_int()`: `' \f\t\n\r\v\0'`); PHP 8.5 leaves it.
        assert_eq!(eval(QUICK_TRIM, &ZStr::from("\x0cx\x0c")).as_bytes(), b"x");
        let plain = ZStr::from("abc");
        assert!(ZStr::ptr_eq(&eval(QUICK_TRIM, &plain), &plain));
        assert!(eval(QUICK_TRIM, &ZStr::from("  \n ")).is_interned());
        assert!(ZStr::ptr_eq(&eval(QUICK_LOWER, &plain), &plain));
        assert_eq!(
            eval(QUICK_LOWER, &ZStr::from("AbC\u{c9}")).as_bytes(),
            "abc\u{c9}".as_bytes()
        );
        assert_eq!(eval(QUICK_UPPER, &plain).as_bytes(), b"ABC");
        for id in [
            QUICK_TRIM,
            QUICK_LTRIM,
            QUICK_RTRIM,
            QUICK_LOWER,
            QUICK_UPPER,
        ] {
            assert!(quick_eval(id, &Value::Int(1)).is_none());
            assert!(quick_eval(id, &Value::Null).is_none());
        }
        assert!(matches!(
            frameless_inline(b"trim", fl::TRIM, Some(&Value::Str(ZStr::from(" x ")))),
            Some(Value::Str(r)) if r.as_bytes() == b"x"
        ));
        assert!(frameless_inline(b"trim", fl::TRIM, Some(&Value::Int(1))).is_none());
    }
    use crate::compiler::{special_builtin_bit, SPECIAL_BUILTINS};
    use crate::func::Func;
    use crate::opcode::type_mask;
    use crate::vm::{PendingCall, Vm};
    use php_runtime::{Value, ZStr};

    #[test]
    fn frameless_calls_need_error_free_arities_and_accepted_scalars() {
        let vm = Vm::new(&crate::VmOptions::default(), Box::new(std::io::sink()));
        let call = |name: &str, args: Vec<Value>| {
            let Some(Func::Builtin(b)) = vm.functions.get(&ZStr::from(name)).cloned() else {
                panic!("{name} is a core builtin");
            };
            let mut call = PendingCall::new(Func::Builtin(b.clone()));
            call.args = args.into_iter().map(Some).collect();
            (b, call)
        };
        let frameless = |name: &str, args: Vec<Value>| {
            let (b, call) = call(name, args);
            frameless_call(&b, &call)
        };
        let s = |v: &str| Value::Str(ZStr::from(v));
        assert!(frameless("abs", vec![Value::Int(-1)]));
        assert!(frameless("abs", vec![Value::Float(-1.5)]));
        // zpp would coerce (or deprecate): the frame is needed.
        assert!(!frameless("abs", vec![s("1")]));
        assert!(!frameless("abs", vec![Value::Bool(true)]));
        assert!(!frameless("abs", vec![Value::Null]));
        assert!(frameless("ord", vec![s("a")]));
        assert!(!frameless("ord", vec![s("")]));
        assert!(!frameless("ord", vec![s("ab")]));
        assert!(frameless("chr", vec![Value::Int(65)]));
        assert!(!frameless("chr", vec![Value::Int(256)]));
        assert!(frameless("strpos", vec![s("abc"), s("b")]));
        // An offset inside the haystack cannot fail; one outside throws
        // the `ValueError` whose trace shows the frame.
        assert!(frameless("strpos", vec![s("abc"), s("b"), Value::Int(1)]));
        assert!(frameless("strpos", vec![s("abc"), s("b"), Value::Int(-3)]));
        assert!(!frameless("strpos", vec![s("abc"), s("b"), Value::Int(4)]));
        assert!(!frameless("strpos", vec![s("abc"), s("b"), Value::Int(-4)]));
        assert!(frameless("substr", vec![s("abc"), Value::Int(1)]));
        assert!(frameless(
            "substr",
            vec![s("abc"), Value::Int(1), Value::Null]
        ));
        assert!(!frameless("substr", vec![s("abc"), s("1")]));
        assert!(frameless("trim", vec![s(" a ")]));
        assert!(!frameless("trim", vec![s(" a "), s("a..b")]));
        assert!(!frameless("str_repeat", vec![s("a"), Value::Int(2)]));
        let arr = Value::Array(php_runtime::ZArray::new());
        assert!(!frameless("substr", vec![arr, Value::Int(1)]));
        let (b, mut dynamic) = call("abs", vec![Value::Int(1)]);
        dynamic.dynamic = true;
        assert!(!frameless_call(&b, &dynamic));
        let (b, mut named) = call("abs", vec![Value::Int(1)]);
        named.has_named = true;
        assert!(!frameless_call(&b, &named));
        let (b, mut ctor) = call("abs", vec![Value::Int(1)]);
        ctor.is_ctor = true;
        assert!(!frameless_call(&b, &ctor));
        let (b, mut magic) = call("abs", vec![Value::Int(1)]);
        magic.magic_name = Some(ZStr::from("abs"));
        assert!(!frameless_call(&b, &magic));
    }

    #[test]
    fn inline_frameless_results_are_the_handlers() {
        let s = |v: &str| Value::Str(ZStr::from(v));
        let int = |v: Option<Value>| match v {
            Some(Value::Int(i)) => Some(i),
            _ => None,
        };
        assert_eq!(
            int(frameless_inline(b"abs", fl::ABS, Some(&Value::Int(-7)))),
            Some(7)
        );
        assert_eq!(
            int(frameless_inline(b"abs", fl::ABS, Some(&Value::Int(7)))),
            Some(7)
        );
        // abs(PHP_INT_MIN) is a float: the handler's.
        assert!(frameless_inline(b"abs", fl::ABS, Some(&Value::Int(i64::MIN))).is_none());
        assert!(matches!(
            frameless_inline(b"abs", fl::ABS, Some(&Value::Float(-2.5))),
            Some(Value::Float(x)) if x.to_bits() == 2.5f64.to_bits()
        ));
        assert_eq!(
            int(frameless_inline(b"ord", fl::ORD, Some(&s("A")))),
            Some(65)
        );
        match frameless_inline(b"chr", fl::CHR, Some(&Value::Int(66))) {
            // `ZSTR_CHAR`: the shared one-character string.
            Some(Value::Str(z)) => assert!(ZStr::ptr_eq(&z, &ZStr::from_byte(b'B'))),
            other => panic!("chr(66) gave {other:?}"),
        }
        assert!(frameless_inline(b"chr", fl::CHR, Some(&Value::Int(256))).is_none());
        assert!(frameless_inline(b"substr", fl::SUBSTR, Some(&s("abc"))).is_none());
        assert!(frameless_inline(b"abs", fl::ABS, None).is_none());
    }

    /// The one-argument math functions run without their handlers, with
    /// the handlers' results (`/usr/bin/php8.5 -n`: `var_dump()` of each).
    #[test]
    #[allow(clippy::float_cmp)]
    fn inline_math_results_are_the_handlers() {
        let float = |name: &str, class: u32, v: Value| match frameless_inline(
            name.as_bytes(),
            class,
            Some(&v),
        ) {
            Some(Value::Float(x)) => x,
            other => panic!("{name}({v:?}) gave {other:?}"),
        };
        assert_eq!(
            float("sqrt", fl::FLOAT, Value::Int(2)),
            1.414_213_562_373_095_1
        );
        assert_eq!(
            float("sin", fl::FLOAT, Value::Int(1)),
            0.841_470_984_807_896_5
        );
        assert_eq!(float("log", fl::LOG, Value::Int(10)), 2.302_585_092_994_046);
        assert_eq!(float("round", fl::ROUND, Value::Float(2.5)), 3.0);
        assert_eq!(float("round", fl::ROUND, Value::Float(-2.5)), -3.0);
        assert_eq!(float("round", fl::ROUND, Value::Float(1.955)), 2.0);
        assert_eq!(float("round", fl::ROUND, Value::Int(7)), 7.0);
        assert_eq!(float("floor", fl::FLOAT, Value::Int(-7)), -7.0);
        assert!(float("log", fl::LOG, Value::Int(-1)).is_nan());
        assert!(matches!(
            frameless_inline(b"is_nan", fl::FLOAT, Some(&Value::Float(f64::NAN))),
            Some(Value::Bool(true))
        ));
        // Not one of the evaluated names: the handler runs.
        assert!(frameless_inline(b"sqrtx", fl::FLOAT, Some(&Value::Int(4))).is_none());
    }

    /// The tape's form of the inline results: `quick_eval` by id gives
    /// what `frameless_inline` gives by name for every argument
    /// `frameless_args` accepts, and nothing for any other.
    #[test]
    fn quick_results_are_the_inline_results() {
        use super::{frameless_class_is, quick_eval, quick_id};
        let names = [
            "abs",
            "ord",
            "chr",
            "dechex",
            "sqrt",
            "sin",
            "cos",
            "floor",
            "ceil",
            "round",
            "exp",
            "tan",
            "atan",
            "log",
            "log10",
            "asin",
            "acos",
            "sinh",
            "cosh",
            "tanh",
            "asinh",
            "acosh",
            "atanh",
            "expm1",
            "log1p",
            "deg2rad",
            "rad2deg",
            "is_nan",
            "is_finite",
            "is_infinite",
        ];
        let args = [
            Value::Null,
            Value::Bool(true),
            Value::Int(0),
            Value::Int(-7),
            Value::Int(65),
            Value::Int(255),
            Value::Int(256),
            Value::Int(i64::MIN),
            Value::Int(i64::MAX),
            Value::Float(0.5),
            Value::Float(-2.5),
            Value::Float(f64::NAN),
            Value::Float(f64::INFINITY),
            Value::Str(ZStr::from("")),
            Value::Str(ZStr::from("a")),
            Value::Str(ZStr::from("ab")),
            Value::Str(ZStr::from("16")),
        ];
        let mut ids = Vec::new();
        for name in names {
            let (id, class) = quick_id(name.as_bytes()).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(
                class << crate::func::flags::FRAMELESS_SHIFT,
                frameless_flags(name.as_bytes()),
                "{name}"
            );
            assert!(!ids.contains(&id), "{name}: id {id} twice");
            ids.push(id);
            for v in &args {
                let sent = [Some(v.clone())];
                let inline = if frameless_args(class, &sent) {
                    frameless_inline(name.as_bytes(), class, Some(v))
                } else {
                    None
                };
                assert_eq!(
                    format!("{:?}", quick_eval(id, v)),
                    format!("{inline:?}"),
                    "{name}({v:?})"
                );
            }
        }
        // Functions whose one-argument call needs its handler have no id.
        for name in ["strpos", "substr", "max", "pi", "intdiv", "strlen"] {
            assert!(quick_id(name.as_bytes()).is_none(), "{name}");
        }
        // `dechex()` is the handler's string.
        for n in [0, 1, 15, 16, 255, 4096, -1, i64::MIN, i64::MAX] {
            let ours = super::dechex_str(n);
            assert_eq!(
                ours.as_bytes(),
                php_stdstr::math::long_to_base(n, 16).as_slice(),
                "{n}"
            );
        }
        assert!(quick_eval(0x7f, &Value::Int(1)).is_none());
        // The class test of the call site's cached function.
        let abs = frameless_flags(b"abs");
        assert!(frameless_class_is(abs, fl::ABS));
        assert!(!frameless_class_is(abs, fl::ORD));
        assert!(!frameless_class_is(
            abs | crate::func::flags::DEPRECATED,
            fl::ABS
        ));
        assert!(!frameless_class_is(0, fl::ABS));
    }

    /// `quick_eval_n` of several arguments gives what the function's
    /// handler gives, for every argument list it answers, and answers none
    /// the handler reports about or converts.
    #[test]
    #[allow(clippy::too_many_lines)] // a table of calls
    fn quick_results_of_several_arguments_are_the_handlers() {
        use super::{defers_like_php_src, quick_eval_n, quick_id_n};
        use crate::func::Args;
        let mut vm = Vm::new(&crate::VmOptions::default(), Box::new(std::io::sink()));
        let s = |v: &str| Value::Str(ZStr::from(v));
        let ints = || {
            Value::Array(php_runtime::ZArray::from_values([
                Value::Int(3),
                Value::Int(1),
                Value::Int(2),
            ]))
        };
        let mixed = || {
            Value::Array(php_runtime::ZArray::from_values([
                Value::Null,
                Value::Bool(false),
                Value::Int(1),
                Value::Float(1.5),
                Value::Str(ZStr::from("2")),
            ]))
        };
        // Strings, one behind a reference, and an int among them.
        let letters = || {
            Value::Array(php_runtime::ZArray::from_values([
                Value::Str(ZStr::from("a")),
                Value::Str(ZStr::from("bc")),
                Value::Int(1),
                Value::Reference(php_runtime::PhpRef::new(Value::Str(ZStr::from("r")))),
                Value::Str(ZStr::from("d")),
                Value::Str(ZStr::from("")),
            ]))
        };
        // Strings only.
        let strs = |vals: &[&str]| {
            Value::Array(php_runtime::ZArray::from_values(
                vals.iter().map(|v| Value::Str(ZStr::from(*v))),
            ))
        };
        let t = Value::Bool(true);
        // A string by its bytes (two handles of one content may differ in
        // what else they record).
        let show = |v: &Value| match v {
            Value::Str(s) => format!("string {:?}", s.as_bytes()),
            v => format!("{v:?}"),
        };
        let cases: Vec<(&str, Vec<Value>)> = vec![
            ("strpos", vec![s("hello world"), s("o")]),
            ("strpos", vec![s("hello world"), s("o"), Value::Int(5)]),
            ("strpos", vec![s("hello world"), s("o"), Value::Int(-3)]),
            ("strpos", vec![s("hello world"), s("wor"), Value::Int(0)]),
            ("strpos", vec![s("hello"), s("")]),
            ("strpos", vec![s("hello"), s(""), Value::Int(5)]),
            ("strpos", vec![s("hello"), s("z")]),
            ("strpos", vec![s(""), s("")]),
            ("substr", vec![s("hello"), Value::Int(1)]),
            ("substr", vec![s("hello"), Value::Int(-2)]),
            ("substr", vec![s("hello"), Value::Int(9)]),
            ("substr", vec![s("hello"), Value::Int(-9)]),
            ("substr", vec![s("hello"), Value::Int(4)]),
            ("substr", vec![s("hello"), Value::Int(5)]),
            ("substr", vec![s("hello"), Value::Int(1), Value::Int(1)]),
            ("substr", vec![s("hello"), Value::Int(0), Value::Int(5)]),
            ("substr", vec![s("hello"), Value::Int(0), Value::Int(50)]),
            ("substr", vec![s("hello"), Value::Int(1), Value::Int(-1)]),
            ("substr", vec![s("hello"), Value::Int(1), Value::Int(-9)]),
            ("substr", vec![s("hello"), Value::Int(-9), Value::Int(-9)]),
            ("substr", vec![s("hello"), Value::Int(2), Value::Int(0)]),
            ("substr", vec![s("hello"), Value::Int(2), Value::Null]),
            ("substr", vec![s(""), Value::Int(0), Value::Int(1)]),
            ("max", vec![Value::Int(1), Value::Int(2)]),
            ("max", vec![Value::Int(2), Value::Int(2)]),
            ("max", vec![Value::Int(-3), Value::Int(-7)]),
            ("min", vec![Value::Int(1), Value::Int(2)]),
            ("min", vec![Value::Int(2), Value::Int(2)]),
            ("min", vec![Value::Int(i64::MAX), Value::Int(i64::MIN)]),
            ("max", vec![Value::Float(1.5), Value::Float(-1.5)]),
            ("max", vec![Value::Float(0.0), Value::Float(-0.0)]),
            ("max", vec![Value::Float(-0.0), Value::Float(0.0)]),
            ("max", vec![Value::Float(f64::NAN), Value::Float(1.0)]),
            ("max", vec![Value::Float(1.0), Value::Float(f64::NAN)]),
            ("min", vec![Value::Float(1.5), Value::Float(-1.5)]),
            ("min", vec![Value::Float(0.0), Value::Float(-0.0)]),
            ("min", vec![Value::Float(-0.0), Value::Float(0.0)]),
            ("min", vec![Value::Float(f64::NAN), Value::Float(1.0)]),
            ("min", vec![Value::Float(1.0), Value::Float(f64::NAN)]),
            ("in_array", vec![Value::Int(2), ints(), t.clone()]),
            ("in_array", vec![Value::Int(7), ints(), t.clone()]),
            ("in_array", vec![s("2"), ints(), t.clone()]),
            ("in_array", vec![s("2"), mixed(), t.clone()]),
            ("in_array", vec![Value::Null, mixed(), t.clone()]),
            ("in_array", vec![Value::Null, ints(), t.clone()]),
            ("in_array", vec![Value::Bool(false), mixed(), t.clone()]),
            ("in_array", vec![Value::Bool(true), mixed(), t.clone()]),
            ("in_array", vec![Value::Float(1.5), mixed(), t.clone()]),
            ("in_array", vec![Value::Float(1.0), mixed(), t.clone()]),
            ("in_array", vec![Value::Int(1), mixed(), t.clone()]),
            ("in_array", vec![s("d"), letters(), t.clone()]),
            ("in_array", vec![s("x"), letters(), t.clone()]),
            ("in_array", vec![s("bb"), letters(), t.clone()]),
            ("in_array", vec![s(""), letters(), t.clone()]),
            ("in_array", vec![s("r"), letters(), t.clone()]),
            ("in_array", vec![s("1"), letters(), t.clone()]),
            ("in_array", vec![s("d"), ints(), t.clone()]),
            // A loose `in_array()` of strings among strings, ints among
            // ints (numeric strings compare as numbers).
            ("in_array", vec![Value::Int(1), ints(), Value::Bool(false)]),
            ("in_array", vec![Value::Int(9), ints()]),
            ("in_array", vec![s("r"), strs(&["a", "bc", "r", ""])]),
            (
                "in_array",
                vec![s("x"), strs(&["a", "bc"]), Value::Bool(false)],
            ),
            ("in_array", vec![s("1e0"), strs(&["a", "1"])]),
            ("in_array", vec![s(" 1"), strs(&["01"])]),
            ("in_array", vec![s("1"), strs(&[])]),
            ("trim", vec![s("xxabxx"), s("x")]),
            ("trim", vec![s("abc"), s("")]),
            ("ltrim", vec![s("  \tab "), s(" \t")]),
            ("rtrim", vec![s("ab\0\0"), s("\0")]),
            ("rtrim", vec![s("aaa"), s("a")]),
            ("str_contains", vec![s("hello"), s("ell")]),
            ("str_contains", vec![s("hello"), s("")]),
            ("str_contains", vec![s("hello"), s("elx")]),
            ("str_starts_with", vec![s("hello"), s("he")]),
            ("str_starts_with", vec![s("hello"), s("lo")]),
            ("str_starts_with", vec![s("hello"), s("")]),
            ("str_ends_with", vec![s("hello"), s("lo")]),
            ("str_ends_with", vec![s("hello"), s("he")]),
            ("str_ends_with", vec![s(""), s("")]),
        ];
        for (name, args) in cases {
            let (id, class) =
                quick_id_n(name.as_bytes(), args.len()).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(
                class,
                frameless_flags(name.as_bytes()) >> crate::func::flags::FRAMELESS_SHIFT,
                "{name}"
            );
            let refs: Vec<&Value> = args.iter().collect();
            let quick = quick_eval_n(id, &refs).unwrap_or_else(|| panic!("{name} {args:?}"));
            // The handler's arguments, as a call passes them: the declared
            // defaults of the omitted ones behind the sent ones.
            let Some(Func::Builtin(b)) = vm.functions.get(&ZStr::from(name)).cloned() else {
                panic!("{name} is a core builtin");
            };
            let mut values = args.clone();
            let mut passed = vec![true; values.len()];
            while let Some(d) = vm.builtin_default(&b, values.len()) {
                values.push(d);
                passed.push(false);
            }
            let mut a = Args {
                values,
                passed,
                named: Vec::new(),
                this: None,
                called: None,
                func: Some(b.clone()),
            };
            let Ok(theirs) = (b.handler)(&mut vm, &mut a) else {
                panic!("{name} {args:?} threw");
            };
            assert_eq!(show(&quick), show(&theirs), "{name} {args:?}");
        }
        // What the handler reports about, converts or compares loosely.
        let declined: Vec<(&str, Vec<Value>)> = vec![
            ("strpos", vec![s("abc"), s("b"), Value::Int(4)]),
            ("strpos", vec![s("abc"), s("b"), Value::Int(-4)]),
            ("strpos", vec![s("abc"), Value::Int(1)]),
            ("substr", vec![s("abc"), s("1")]),
            ("substr", vec![Value::Int(123), Value::Int(1)]),
            ("max", vec![Value::Int(1), Value::Float(2.0)]),
            ("max", vec![s("a"), s("b")]),
            ("min", vec![Value::Float(1.0), Value::Int(2)]),
            ("in_array", vec![s("1"), ints(), Value::Bool(false)]),
            ("in_array", vec![Value::Int(1), letters()]),
            ("in_array", vec![ints(), ints(), t.clone()]),
            ("str_contains", vec![s("abc"), Value::Int(1)]),
            ("trim", vec![s("abc"), s("a..c")]),
            ("trim", vec![s("abc"), Value::Int(1)]),
        ];
        for (name, args) in declined {
            let (id, _) =
                quick_id_n(name.as_bytes(), args.len()).unwrap_or_else(|| panic!("{name}"));
            let refs: Vec<&Value> = args.iter().collect();
            assert!(quick_eval_n(id, &refs).is_none(), "{name} {args:?}");
        }
        // Other arities and functions have no id.
        for (name, argc) in [("max", 3), ("in_array", 4), ("strpos", 4), ("trim", 3)] {
            assert!(quick_id_n(name.as_bytes(), argc).is_none(), "{name}/{argc}");
        }
        assert_eq!(quick_id_n(b"ord", 1), super::quick_id(b"ord"));
        // php-src's own frameless functions, by arity.
        for (name, argc, yes) in [
            ("substr", 2, true),
            ("substr", 3, true),
            ("max", 2, true),
            ("max", 3, false),
            ("in_array", 3, true),
            ("str_replace", 3, true),
            ("trim", 1, true),
            ("abs", 1, false),
            ("ord", 1, false),
            ("sqrt", 1, false),
            ("intdiv", 2, false),
            ("strlen", 1, false),
        ] {
            assert_eq!(
                defers_like_php_src(name.as_bytes(), argc),
                yes,
                "{name}/{argc}"
            );
        }
    }

    #[test]
    fn registration_stores_the_frameless_class_in_the_flags() {
        use crate::func::flags;
        let mut vm = Vm::new(&crate::VmOptions::default(), Box::new(std::io::sink()));
        vm.register_function(
            "in_array(mixed $needle, array $haystack, bool $strict = false): bool",
            |_vm, _a| Ok(Value::Bool(false)),
        );
        vm.register_function("unrelated(int $n): int", |_vm, _a| Ok(Value::Int(0)));
        let class_of = |name: &str| match vm.functions.get(&ZStr::from(name)) {
            Some(Func::Builtin(b)) => (b.flags & flags::FRAMELESS) >> flags::FRAMELESS_SHIFT,
            _ => panic!("{name} is registered"),
        };
        assert_eq!(class_of("in_array"), fl::IN_ARRAY);
        assert_eq!(class_of("abs"), fl::ABS);
        assert_eq!(class_of("strtolower"), fl::CASE);
        assert_eq!(class_of("unrelated"), 0);
        assert_eq!(class_of("strlen"), fl::STRLEN);
        assert_eq!(class_of("is_string"), fl::TYPE_TEST);
        assert_eq!(class_of("count"), fl::COUNT);
        // Distinct, non-zero classes that fit the five flag bits.
        let names: &[&[u8]] = &[
            b"abs",
            b"dechex",
            b"strtolower",
            b"trim",
            b"dirname",
            b"is_numeric",
            b"ord",
            b"chr",
            b"strpos",
            b"str_contains",
            b"max",
            b"substr",
            b"strstr",
            b"in_array",
            b"str_replace",
            b"strtr",
            b"sqrt",
            b"fmod",
            b"log",
            b"round",
            b"intdiv",
            b"pi",
            b"is_string",
            b"strlen",
            b"count",
            b"array_key_exists",
        ];
        let mut seen = Vec::new();
        for name in names {
            let bits = frameless_flags(name);
            assert_ne!(bits, 0, "{}", String::from_utf8_lossy(name));
            assert_eq!(bits & !flags::FRAMELESS, 0);
            assert!(!seen.contains(&bits));
            seen.push(bits);
        }
        assert_eq!(frameless_flags(b"STRPOS"), 0);
        assert_eq!(frameless_flags(b"chop"), 0);
        assert_eq!(frameless_flags(b"implode"), 0);
        assert_eq!(frameless_flags(b"preg_match"), 0);
        assert_eq!(frameless_flags(b"ltrim"), frameless_flags(b"rtrim"));
        assert_eq!(frameless_flags(b"sqrt"), frameless_flags(b"floor"));
        assert_eq!(frameless_flags(b"pow"), 0);
        assert_eq!(frameless_flags(b"mt_rand"), 0);
        assert_eq!(frameless_flags(b"min"), frameless_flags(b"max"));
    }

    #[test]
    fn frameless_argument_shapes_exclude_every_error_path() {
        let s = |v: &str| Some(Value::Str(ZStr::from(v)));
        let i = |n: i64| Some(Value::Int(n));
        let ok = |class: u32, args: &[Option<Value>]| frameless_args(class, args);
        // trim(): character lists with a `..` range can warn.
        assert!(ok(fl::TRIM, &[s(" a ")]));
        assert!(ok(fl::TRIM, &[s("xay"), s("xy")]));
        assert!(ok(fl::TRIM, &[s("xay"), s("")]));
        assert!(ok(fl::TRIM, &[s("xay"), s("a.b")]));
        assert!(!ok(fl::TRIM, &[s("xay"), s("a..z")]));
        assert!(!ok(fl::TRIM, &[s("xay"), s("..")]));
        assert!(!ok(fl::TRIM, &[s("xay"), i(1)]));
        // dirname(): levels below 1 throw.
        assert!(ok(fl::DIRNAME, &[s("/a/b")]));
        assert!(ok(fl::DIRNAME, &[s("/a/b"), i(2)]));
        assert!(!ok(fl::DIRNAME, &[s("/a/b"), i(0)]));
        assert!(!ok(fl::DIRNAME, &[s("/a/b"), s("2")]));
        // strpos(): the offset must be inside the haystack.
        assert!(ok(fl::STRPOS, &[s(""), s(""), i(0)]));
        assert!(!ok(fl::STRPOS, &[s(""), s(""), i(1)]));
        assert!(!ok(fl::STRPOS, &[s("ab"), s("b"), i(i64::MIN)]));
        assert!(!ok(fl::STRPOS, &[s("ab"), s("b"), Some(Value::Null)]));
        // in_array(): scalar needles; loose comparisons only with
        // silent elements of a short haystack, strict ones always.
        let mut vm = Vm::new(&crate::VmOptions::default(), Box::new(std::io::sink()));
        let std_class = vm.find_class(b"stdClass").expect("stdClass");
        let instance = vm.new_object(&std_class).expect("an instance");
        let obj = || Value::Object(instance.clone());
        let arr = |vals: Vec<Value>| Some(Value::Array(php_runtime::ZArray::from_values(vals)));
        assert!(ok(
            fl::IN_ARRAY,
            &[i(2), arr(vec![Value::Int(1), Value::Int(2)])]
        ));
        assert!(ok(
            fl::IN_ARRAY,
            &[s("a"), arr(vec![Value::Array(php_runtime::ZArray::new())])]
        ));
        assert!(!ok(fl::IN_ARRAY, &[i(2), arr(vec![obj()])]));
        assert!(ok(
            fl::IN_ARRAY,
            &[i(2), arr(vec![obj()]), Some(Value::Bool(true))]
        ));
        assert!(!ok(
            fl::IN_ARRAY,
            &[i(2), arr(vec![obj()]), Some(Value::Bool(false))]
        ));
        let by_ref = Value::Reference(php_runtime::PhpRef::new(obj()));
        assert!(!ok(fl::IN_ARRAY, &[i(2), arr(vec![by_ref])]));
        let scalar_ref = Value::Reference(php_runtime::PhpRef::new(Value::Int(2)));
        assert!(ok(fl::IN_ARRAY, &[i(2), arr(vec![scalar_ref])]));
        let long = arr((0..=32).map(Value::Int).collect());
        assert!(!ok(fl::IN_ARRAY, &[i(2), long.clone()]));
        assert!(ok(fl::IN_ARRAY, &[i(2), long, Some(Value::Bool(true))]));
        assert!(!ok(fl::IN_ARRAY, &[arr(vec![]), arr(vec![])]));
        assert!(!ok(fl::IN_ARRAY, &[i(2), arr(vec![]), i(1)]));
        // str_replace(): a growing result reserves memory first.
        assert!(ok(fl::STR_REPLACE, &[s("ab"), s("c"), s("xabx")]));
        assert!(ok(fl::STR_REPLACE, &[s("ab"), s("cd"), s("xabx")]));
        assert!(!ok(fl::STR_REPLACE, &[s("a"), s("cd"), s("xax")]));
        assert!(!ok(fl::STR_REPLACE, &[s("a"), s("b"), arr(vec![])]));
        // max()/min() of two scalars; strtr() of three strings.
        assert!(ok(fl::MIN_MAX, &[i(1), s("x")]));
        assert!(!ok(fl::MIN_MAX, &[i(1), arr(vec![])]));
        assert!(!ok(fl::MIN_MAX, &[i(1), i(2), i(3)]));
        assert!(ok(fl::STRTR, &[s("abc"), s("a"), s("b")]));
        assert!(!ok(fl::STRTR, &[s("abc"), arr(vec![])]));
        // Math: ints are widened silently; log() bases, round() modes and
        // intdiv() by zero or overflowing can fail.
        let f = |x: f64| Some(Value::Float(x));
        assert!(ok(fl::FLOAT, &[i(4)]));
        assert!(ok(fl::FLOAT, &[f(-1.0)]));
        assert!(!ok(fl::FLOAT, &[s("4")]));
        assert!(!ok(fl::FLOAT, &[Some(Value::Null)]));
        assert!(!ok(fl::FLOAT, &[i(4), i(4)]));
        assert!(ok(fl::FLOAT2, &[i(4), f(2.0)]));
        assert!(!ok(fl::FLOAT2, &[i(4)]));
        assert!(ok(fl::LOG, &[i(8)]));
        assert!(!ok(fl::LOG, &[i(8), i(2)]));
        assert!(ok(fl::ROUND, &[f(1.5)]));
        assert!(ok(fl::ROUND, &[f(1.55), i(1)]));
        assert!(!ok(fl::ROUND, &[f(1.55), f(1.0)]));
        assert!(!ok(fl::ROUND, &[f(1.55), i(1), i(2)]));
        assert!(ok(fl::INTDIV, &[i(7), i(2)]));
        assert!(ok(fl::INTDIV, &[i(i64::MIN), i(1)]));
        assert!(!ok(fl::INTDIV, &[i(7), i(0)]));
        assert!(!ok(fl::INTDIV, &[i(i64::MIN), i(-1)]));
        assert!(!ok(fl::INTDIV, &[f(7.0), i(2)]));
        assert!(ok(fl::PI, &[]));
        assert!(!ok(fl::PI, &[i(1)]));
        // Unknown classes and gaps never run frameless.
        assert!(!ok(0, &[i(1)]));
        assert!(!ok(fl::ABS, &[None]));
        assert!(!ok(fl::ABS, &[]));
    }

    #[test]
    fn frameless_calls_return_their_buffers_to_the_pools() {
        let mut vm = Vm::new(&crate::VmOptions::default(), Box::new(std::io::sink()));
        let pools = |vm: &Vm| {
            let mut sizes: Vec<_> = vm.call_bufs.args.iter().map(Vec::capacity).collect();
            sizes.sort_unstable();
            (
                sizes,
                vm.call_bufs.values.len(),
                vm.call_bufs.passed.len(),
                vm.call_bufs.args_pool.len(),
            )
        };
        let before = pools(&vm);
        let status = vm.run_script(
            br#"<?php
$s = 'abc';
for ($i = 0; $i < 3; $i++) {
    strpos($s, 'b'); strpos($s, 'b', 1); trim($s); trim($s, 'ac'); substr($s, 1);
    abs(-$i); str_replace('a', 'b', $s); strtr($s, 'a', 'b'); dirname('/a/b', 2);
}
"#,
            "pools.php",
        );
        assert_eq!(status, 0);
        assert_eq!(pools(&vm), before);
        assert!(vm.call_bufs.args.iter().all(Vec::is_empty));
        assert!(vm.call_bufs.values.iter().all(Vec::is_empty));
        // The pooled argument records came back empty.
        assert!(vm
            .call_bufs
            .args_pool
            .iter()
            .all(|a| a.values.is_empty() && a.passed.is_empty() && a.func.is_none()));
    }

    /// Frameless calls made in place (`Vm::frameless_icall_in_place`):
    /// handler calls with and without literal defaults, the inline
    /// results of `abs()`, `ord()` and `chr()`, and arguments taken from
    /// variables, literals and temporaries, which the calls release (the
    /// variables are reused afterwards). Expected output from
    /// `/usr/bin/php8.5 -n`.
    #[test]
    fn frameless_calls_in_place_match_the_handlers() {
        use std::cell::RefCell;
        use std::io::Write;
        use std::rc::Rc;
        #[derive(Clone, Default)]
        struct Sink(Rc<RefCell<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.borrow_mut().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sink = Sink::default();
        let mut vm = Vm::new(&crate::VmOptions::default(), Box::new(sink.clone()));
        let status = vm.run_script(
            br#"<?php
$s = "hello world";
for ($i = 0; $i < 2; $i++) {
    echo strpos($s, "o"), " ", strpos($s, "o", 5), " ", ord("A"), " ", abs(-3), " ",
        chr(66), " ", substr($s, 6), " ", trim("  x  "), " ", round(2.5), " ",
        in_array(2, [1, 2]) ? "y" : "n", " ", str_replace("o", "0", $s), "\n";
}
function f($t) { return ord($t) + strpos($t . "z", "z"); }
echo f("a"), " ", $s, "\n";
"#,
            "t.php",
        );
        assert_eq!(status, 0);
        drop(vm);
        let out = String::from_utf8_lossy(&sink.0.borrow()).into_owned();
        assert_eq!(
            out,
            "4 7 65 3 B world x 3 y hell0 w0rld\n4 7 65 3 B world x 3 y hell0 w0rld\n98 hello world\n"
        );
    }

    /// `Op::FramelessICall` (`Vm::frameless_icall_op`): calls run in
    /// place, and declined ones (a reference, a coerced or invalid
    /// argument, an undefined variable, a `ValueError`) run as the call
    /// ops they replaced, with the builtin's frame in the trace. Expected
    /// output from `/usr/bin/php8.5 -n`.
    #[test]
    fn fused_frameless_calls_match_the_call_ops() {
        use std::cell::RefCell;
        use std::io::Write;
        use std::rc::Rc;
        #[derive(Clone, Default)]
        struct Sink(Rc<RefCell<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.borrow_mut().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let run = |src: &str| {
            let sink = Sink::default();
            let mut vm = Vm::new(&crate::VmOptions::default(), Box::new(sink.clone()));
            let status = vm.run_script(src.as_bytes(), "t.php");
            assert_eq!(status, 0);
            drop(vm);
            let bytes = sink.0.borrow().clone();
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let out = run(r#"<?php
function t($s, $n) {
    $r = [];
    $r[] = strpos($s, "o");
    $r[] = strpos($s, "o", 5);
    $r[] = substr($s, 6);
    $r[] = substr($s, 1, 3);
    $r[] = str_contains($s, "wor") ? "y" : "n";
    $r[] = trim("  x  ");
    $r[] = strtoupper($s);
    $r[] = in_array($n, [1, 2, 3]) ? "y" : "n";
    $r[] = max($n, 2);
    $r[] = str_replace("o", "0", $s);
    $ref = &$s;
    $r[] = strpos($s, "w");
    $r[] = strpos($n, "2");
    $r[] = var_export(@strpos($undef, "x"), true);
    echo implode(",", $r), "\n";
}
t("hello world", 2);
t("hello world", 2);
try { strpos([], "x"); } catch (TypeError $e) { echo $e->getMessage(), " ", $e->getLine(), "\n", $e->getTraceAsString(), "\n"; }
try { substr("abc", "x"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { strpos("abc", "c", 9); } catch (ValueError $e) { echo $e->getMessage(), " ", $e->getLine(), "\n"; }
"#);
        assert_eq!(
            out,
            "4,7,world,ell,y,x,HELLO WORLD,y,2,hell0 w0rld,6,0,false\n\
             4,7,world,ell,y,x,HELLO WORLD,y,2,hell0 w0rld,6,0,false\n\
             strpos(): Argument #1 ($haystack) must be of type string, array given 22\n\
             #0 t.php(22): strpos(Array, 'x')\n\
             #1 {main}\n\
             substr(): Argument #2 ($offset) must be of type int, string given\n\
             strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack) 24\n"
        );
        let out = run(r#"<?php declare(strict_types=1);
$s = "abc"; $n = 5;
echo strpos($s, "b"), "\n";
try { strpos($n, "5"); } catch (TypeError $e) { echo $e->getMessage(), " ", $e->getLine(), "\n"; }
"#);
        assert_eq!(
            out,
            "1\nstrpos(): Argument #1 ($haystack) must be of type string, int given 4\n"
        );
    }

    #[test]
    fn special_builtin_bits_are_distinct_and_lowercase() {
        let mut seen = 0u32;
        for name in SPECIAL_BUILTINS {
            assert_eq!(name.to_ascii_lowercase(), *name);
            let bit = special_builtin_bit(name.as_bytes()).expect("listed");
            assert_eq!(seen & bit, 0, "{name}");
            seen |= bit;
        }
        assert_eq!(special_builtin_bit(b"is_integer"), None);
        assert_eq!(special_builtin_bit(b"doubleval"), None);
        assert_eq!(special_builtin_bit(b"STRLEN"), None);
    }

    #[test]
    fn type_mask_matches_like_zend_type_check() {
        let s = Value::Str(ZStr::from("x"));
        assert!(type_mask::matches(type_mask::STRING, &s));
        assert!(type_mask::matches(type_mask::SCALAR, &s));
        assert!(!type_mask::matches(type_mask::ARRAY, &s));
        assert!(type_mask::matches(type_mask::NULL, &Value::Null));
        assert!(!type_mask::matches(type_mask::SCALAR, &Value::Null));
        assert!(type_mask::matches(type_mask::BOOL, &Value::Bool(false)));
        assert!(type_mask::matches(type_mask::INT, &Value::Int(0)));
        assert!(!type_mask::matches(type_mask::FLOAT, &Value::Int(0)));
        assert!(type_mask::matches(type_mask::FLOAT, &Value::Float(0.5)));
        let r = Value::Reference(php_runtime::PhpRef::new(Value::Int(3)));
        assert!(type_mask::matches(type_mask::INT, &r));
        assert!(!type_mask::matches(type_mask::STRING, &r));
    }
}
