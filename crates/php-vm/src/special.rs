//! Handlers of the opcodes that `zend_try_compile_special_func()` compiles
//! builtin calls to (ref: Zend/zend_compile.c `zend_compile_func_strlen`,
//! `_count`, `_typecheck`, `_gettype`; Zend/zend_vm_def.h `ZEND_STRLEN`,
//! `ZEND_COUNT`, `ZEND_TYPE_CHECK`, `ZEND_GET_TYPE`).
//!
//! The opcodes run in the caller's frame: no builtin frame is pushed, so
//! a `TypeError` they raise has no `strlen()` line in its trace, exactly as
//! in php-src. The common operand types are handled inline by the run
//! loop; the rest comes here.
//!
//! Also here: frameless internal calls (ref: Zend/zend_frameless_function.h,
//! `ZEND_FRAMELESS_ICALL_n` in Zend/zend_vm_def.h), which call a handler
//! without pushing its call frame.

use crate::call::accepts_without_coercion;
use crate::compiler::SPECIAL_BUILTINS;
use crate::func::{flags, Args, Builtin, Func};
use crate::object;
use crate::opcode::Operand;
use crate::typecheck::given_type;
use crate::vm::{PendingCall, Vm, VmResult};
use php_runtime::{Value, ZStr};
use std::rc::Rc;

impl Vm {
    /// [`crate::compiler::CompileOptions::special_builtins`] for the
    /// current function table: the special builtins registered as
    /// (non-deprecated) internal functions. A disabled function redeclared
    /// in user code keeps its calls.
    pub(crate) fn special_builtins(&self) -> u32 {
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

/// Whether `call` of builtin `b` runs frameless: php-src's frameless
/// internal calls (`ZEND_FRAMELESS_ICALL_n`) skip the call frame and
/// materialise it only when the function reports an error. Here a call is
/// frameless only when nothing can see the frame: a function of the list
/// below whose handler has no diagnostic, exception or callback for
/// arguments that already have an accepted scalar type (so zpp coerces
/// nothing and no object can be released), called by name without named
/// arguments. Everything else pushes the builtin frame as before.
pub(crate) fn frameless_call(b: &Builtin, call: &PendingCall) -> bool {
    if call.magic_name.is_some()
        || call.is_ctor
        || call.has_named
        || !call.named.is_empty()
        || call.dynamic
        || call.this.is_some()
        || call.scope.is_some()
        || call.called.is_some()
        || call.closure.is_some()
        || call.pfa.is_some()
        || b.flags & (flags::DEPRECATED | flags::NODISCARD) != 0
    {
        return false;
    }
    let n = call.args.len();
    let name = b.name.as_bytes();
    // The arities (and values) without an error path: strpos()'s offset
    // can be out of range, trim()'s character list can hold an invalid
    // `..` range, dirname()'s levels can be below 1, ord() and chr()
    // deprecate other lengths and values, max()/min() need two values.
    let error_free = match name {
        b"abs" | b"dechex" | b"strtolower" | b"strtoupper" | b"trim" | b"ltrim" | b"rtrim"
        | b"dirname" | b"is_numeric" => n == 1,
        b"ord" => n == 1 && matches!(&call.args[0], Some(Value::Str(s)) if s.len() == 1),
        b"chr" => n == 1 && matches!(&call.args[0], Some(Value::Int(c)) if (0..=255).contains(c)),
        b"strpos" | b"str_contains" | b"str_starts_with" | b"str_ends_with" | b"max" | b"min" => {
            n == 2
        }
        b"substr" | b"strstr" => n == 2 || n == 3,
        _ => false,
    };
    let variadic = b.flags & flags::VARIADIC != 0;
    if !error_free
        || (variadic && !matches!(name, b"max" | b"min"))
        || n < b.required as usize
        || n > b.params.len()
    {
        return false;
    }
    call.args.iter().zip(&b.params).all(|(arg, p)| {
        let Some(v) = arg else {
            return false;
        };
        let scalar = matches!(
            v,
            Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_)
        );
        let accepted = match &p.ty {
            None => true,
            Some(ty) => ty.is_mixed() || accepts_without_coercion(ty.mask, v),
        };
        scalar && accepted && !p.by_ref && p.promote & crate::opcode::UNCHECKED_TYPE == 0
    })
}

impl Vm {
    /// A [`frameless_call`]: the handler runs on the caller's frame with
    /// the arguments moved out of the pending call (and the declared
    /// defaults of omitted trailing parameters, as for a framed call).
    pub(crate) fn call_builtin_frameless(
        &mut self,
        b: &Rc<Builtin>,
        mut call: PendingCall,
    ) -> VmResult<Value> {
        self.magic_call_name = None;
        let nparams = b.params.len();
        let mut values = self.call_bufs.values.pop().unwrap_or_default();
        values.reserve(nparams);
        let mut passed = self.call_bufs.passed.pop().unwrap_or_default();
        passed.reserve(nparams);
        for arg in call.args.drain(..) {
            values.push(arg.unwrap_or_default());
            passed.push(true);
        }
        self.put_arg_vec(std::mem::take(&mut call.args));
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
            func: Some(b.clone()),
        };
        let mut r = (b.handler)(self, &mut args);
        // `ZEND_VM_FCALL_INTERRUPT_CHECK`, after the call.
        if r.is_ok() {
            if let Err(e) = self.check_interrupt() {
                r = Err(e);
            }
        }
        let Args { values, passed, .. } = args;
        self.put_val_vec(values);
        self.put_bool_vec(passed);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::frameless_call;
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
        assert!(!frameless("strpos", vec![s("abc"), s("b"), Value::Int(1)]));
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
