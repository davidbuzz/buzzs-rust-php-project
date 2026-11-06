//! Handlers of the opcodes that `zend_try_compile_special_func()` compiles
//! builtin calls to (ref: Zend/zend_compile.c `zend_compile_func_strlen`,
//! `_count`, `_typecheck`, `_gettype`, `_defined`, `_in_array`,
//! `_array_key_exists`; Zend/zend_vm_def.h `ZEND_STRLEN`, `ZEND_COUNT`,
//! `ZEND_TYPE_CHECK`, `ZEND_GET_TYPE`, `ZEND_DEFINED`, `ZEND_IN_ARRAY`,
//! `ZEND_ARRAY_KEY_EXISTS`; Zend/zend_execute.c `zend_quick_check_constant`,
//! `zend_array_key_exists_fast`, `zend_array_key_exists_error`).
//!
//! The opcodes run in the caller's frame: no builtin frame is pushed, so
//! a `TypeError` they raise has no `strlen()` line in its trace, exactly as
//! in php-src. The common operand types are handled inline by the run
//! loop; the rest comes here.

use crate::compiler::SPECIAL_BUILTINS;
use crate::func::{flags, Func};
use crate::object;
use crate::opcode::Operand;
use crate::typecheck::given_type;
use crate::vm::{PendingCall, Vm, VmResult};
use php_runtime::{Value, ZArray, ZStr};

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
                self.functions.get(&ZStr::from(*name)),
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
        let mut v = v.deref();
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
        match v.deref() {
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
    #[inline]
    pub(crate) fn op_defined(&mut self, name: u32) -> VmResult<bool> {
        if let Value::Str(n) = &self.frame().code.literals[name as usize] {
            return Ok(self.constants.contains_key(n));
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
        let Some(Func::Builtin(b)) = self.functions.get(&ZStr::from(name)).cloned() else {
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

#[cfg(test)]
mod tests {
    use crate::compiler::{special_builtin_bit, SPECIAL_BUILTINS};
    use crate::opcode::type_mask;
    use php_runtime::{Value, ZStr};

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
