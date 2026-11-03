//! `ReflectionParameter`.

use super::class::class_arg;
use super::export::{self, parse_const_expr};
use super::{
    class_value, function_value, method_value, rexc, set_name_props, this, this_refl, FnRef, Refl,
    PRIV, PUB, RO,
};
use crate::builtins::classbuilder::ClassBuilder;
use crate::class::{cflags, ConstExpr};
use crate::func::{Args, Func};
use crate::object;
use crate::opcode::ParamInfo;
use crate::types::{mask, TypeDecl};
use crate::vm::{Vm, VmResult};
use php_parser::ast::{ClassRef, ExprKind, MagicConst, MemberName, NameKind};
use php_runtime::{ArrayKey, Value, ZStr};
use std::rc::Rc;

fn this_param(vm: &mut Vm, a: &Args) -> VmResult<(FnRef, usize, ParamInfo)> {
    match this_refl(vm, a)? {
        Refl::Param { f, idx } => {
            let p = f
                .func
                .params()
                .get(idx)
                .cloned()
                .unwrap_or_else(|| ParamInfo {
                    name: ZStr::empty(),
                    by_ref: false,
                    variadic: false,
                    ty: None,
                    has_default: false,
                    default_text: None,
                    promote: 0,
                    sensitive: false,
                    default_expr: None,
                    attributes: Vec::new(),
                });
            Ok((f, idx, p))
        }
        _ => Err(vm.make_throw(
            "Error",
            "Internal error: Failed to retrieve the reflection object",
        )),
    }
}

/// `reflection_parameter_factory`.
pub(crate) fn param_value(vm: &mut Vm, f: &FnRef, idx: usize) -> VmResult<Value> {
    let name = f
        .func
        .params()
        .get(idx)
        .map(|p| p.name.clone())
        .unwrap_or_default();
    super::new_refl(
        vm,
        "ReflectionParameter",
        Refl::Param { f: f.clone(), idx },
        Some(&name),
        None,
    )
}

fn rp_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = this(a);
    let reference = a.at(0);
    let f: FnRef = match &reference {
        Value::Str(s) => {
            let n = s.as_bytes().strip_prefix(b"\\").unwrap_or(s.as_bytes());
            match vm.functions.get(n.to_ascii_lowercase().as_slice()).cloned() {
                Some(func) => FnRef::function(func),
                None => {
                    return Err(rexc(
                        vm,
                        &format!("Function {}() does not exist", s.to_string_lossy()),
                    ))
                }
            }
        }
        Value::Array(arr) => {
            let (Some(cr), Some(m)) = (arr.get(&ArrayKey::Int(0)), arr.get(&ArrayKey::Int(1)))
            else {
                return Err(rexc(
                    vm,
                    "Expected array($object, $method) or array($classname, $method)",
                ));
            };
            let (cr, m) = (cr.deref(), m.deref());
            let c = match &cr {
                Value::Object(ob) => object::class_of(ob).clone(),
                other => {
                    let n = vm.to_zstr(other)?;
                    match vm.lookup_class(&n)? {
                        Some(c) => c,
                        None => {
                            return Err(rexc(
                                vm,
                                &format!("Class \"{}\" does not exist", n.to_string_lossy()),
                            ))
                        }
                    }
                }
            };
            let mn = vm.to_zstr(&m)?;
            match c.find_method(mn.as_bytes()).cloned() {
                Some(mm) => FnRef::method(&mm, &c),
                None => {
                    if let (Value::Object(ob), true) = (
                        &cr,
                        c.lc_name.as_bytes() == b"closure" && mn.eq_ignore_ascii_case(b"__invoke"),
                    ) {
                        match vm.closure_data(ob) {
                            Some(d) => {
                                let m = Rc::new(crate::func::Method {
                                    name: ZStr::from("__invoke"),
                                    func: d.func.clone(),
                                    flags: crate::func::flags::PUBLIC,
                                    scope: Rc::downgrade(&c),
                                    proto_scope: Rc::downgrade(&c),
                                });
                                FnRef {
                                    func: d.func,
                                    method: Some(m),
                                    class: Some(c.clone()),
                                    closure: None,
                                }
                            }
                            None => {
                                return Err(rexc(
                                    vm,
                                    &format!(
                                        "Method {}::{}() does not exist",
                                        c.name,
                                        mn.to_string_lossy()
                                    ),
                                ))
                            }
                        }
                    } else {
                        return Err(rexc(
                            vm,
                            &format!(
                                "Method {}::{}() does not exist",
                                c.name,
                                mn.to_string_lossy()
                            ),
                        ));
                    }
                }
            }
        }
        Value::Object(ob) => {
            let c = object::class_of(ob).clone();
            if let Some(d) = vm.closure_data(ob) {
                FnRef {
                    func: d.func,
                    method: None,
                    class: None,
                    closure: Some(ob.clone()),
                }
            } else {
                match c.find_method(b"__invoke").cloned() {
                    Some(m) => FnRef::method(&m, &c),
                    None => {
                        return Err(rexc(
                            vm,
                            &format!("Method {}::__invoke() does not exist", c.name),
                        ))
                    }
                }
            }
        }
        other => {
            let e = rexc(
                vm,
                &format!(
                    "ReflectionParameter::__construct(): Argument #1 ($function) must be a string, an array(class, method), or a callable object, {} given",
                    crate::typecheck::given_type(other)
                ),
            );
            return Err(e);
        }
    };
    let params = f.func.params();
    let idx = match a.at(1) {
        Value::Str(n) => match params.iter().position(|p| p.name == n) {
            Some(i) => i,
            None => {
                return Err(rexc(
                    vm,
                    "The parameter specified by its name could not be found",
                ))
            }
        },
        v => {
            let i = v.as_int().unwrap_or(0);
            if i < 0 {
                return Err(vm.make_throw(
                    "ValueError",
                    "ReflectionParameter::__construct(): Argument #2 ($param) must be greater than or equal to 0",
                ));
            }
            match usize::try_from(i) {
                Ok(i) if i < params.len() => i,
                _ => {
                    return Err(rexc(
                        vm,
                        "The parameter specified by its offset could not be found",
                    ))
                }
            }
        }
    };
    let name = params[idx].name.clone();
    set_name_props(&o, Some(&name), None);
    object::set_internal(&o, Refl::Param { f, idx });
    Ok(Value::Null)
}

fn rp_to_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, idx, _) = this_param(vm, a)?;
    let req = idx < f.func.required() as usize;
    Ok(Value::Str(ZStr::from(export::parameter_string(
        vm, &f, idx, req,
    ))))
}

fn rp_get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(this_param(vm, a)?.2.name))
}
fn rp_get_doc_comment(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, idx, _) = this_param(vm, a)?;
    Ok(super::function::param_doc_comment(&f, idx).map_or(Value::Bool(false), Value::Str))
}
fn rp_get_position(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(i64::try_from(this_param(vm, a)?.1).unwrap_or(0)))
}
fn rp_is_optional(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, idx, _) = this_param(vm, a)?;
    Ok(Value::Bool(idx >= f.func.required() as usize))
}

/// Whether the parameter has a default value (`RECV_INIT` / arginfo default).
fn has_default(p: &ParamInfo, f: &FnRef, idx: usize) -> bool {
    if p.variadic || f.is_trampoline() || (f.func.is_user() && idx < f.func.required() as usize) {
        return false;
    }
    match &f.func {
        Func::Builtin(_) => p.default_text.as_deref().is_some_and(|t| t != "UNKNOWN"),
        Func::User(_) => p.default_expr.is_some(),
    }
}

fn rp_has_default(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, idx, p) = this_param(vm, a)?;
    Ok(Value::Bool(has_default(&p, &f, idx)))
}

fn no_default(vm: &mut Vm) -> crate::vm::Throw {
    rexc(vm, "Internal error: Failed to retrieve the default value")
}

/// Evaluates an internal function's default value text.
fn builtin_default(vm: &mut Vm, f: &FnRef, idx: usize, text: &str) -> VmResult<Value> {
    if let Func::Builtin(b) = &f.func {
        if let Some(Some(v)) = b.defaults.get(idx) {
            return Ok(v.clone());
        }
    }
    let t = text.trim();
    if let Some(p) = t.find("::") {
        let cn = ZStr::from(&t[..p]);
        if let Some(c) = vm.lookup_class(&cn)? {
            return vm.class_constant(&c, &ZStr::from(&t[p + 2..]));
        }
    }
    if let Some(v) = vm.constants.get(&ZStr::from(t)).cloned() {
        return Ok(v);
    }
    if let Some(e) = parse_const_expr(t) {
        if let ExprKind::Array { items, .. } = &e.kind {
            if items.is_empty() {
                return Ok(Value::Array(php_runtime::ZArray::new()));
            }
        }
    }
    Ok(Value::Null)
}

fn rp_get_default(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, idx, p) = this_param(vm, a)?;
    if !has_default(&p, &f, idx) {
        return Err(no_default(vm));
    }
    match &f.func {
        Func::Builtin(_) => {
            let t = p.default_text.clone().unwrap_or_default();
            builtin_default(vm, &f, idx, &t)
        }
        Func::User(_) => {
            let scope = f.scope(vm);
            match &p.default_expr {
                Some(e) => vm.eval_const_expr(e, scope.as_ref()),
                None => Ok(Value::Null),
            }
        }
    }
}

/// The constant named by a default value, when it is a constant
/// (`ZEND_AST_CONSTANT`, `ZEND_AST_CONSTANT_CLASS`, `ZEND_AST_CLASS_CONST`).
fn default_constant_name(vm: &Vm, f: &FnRef, p: &ParamInfo) -> Option<Option<String>> {
    let text = p.default_text.clone()?;
    if !f.func.is_user() {
        let t = text.trim();
        let e = parse_const_expr(t)?;
        return match &e.kind {
            ExprKind::Const(n)
                if !matches!(
                    n.name.to_ascii_lowercase().as_slice(),
                    b"true" | b"false" | b"null"
                ) =>
            {
                Some(Some(t.to_string()))
            }
            ExprKind::ClassConst { .. } => Some(Some(t.to_string())),
            _ => None,
        };
    }
    if !matches!(p.default_expr, Some(ConstExpr::Code(_))) {
        return None;
    }
    let ns = export::fn_namespace(vm, f);
    let e = parse_const_expr(&text)?;
    let join = |n: &php_parser::ast::Name| -> String {
        let mut v = Vec::new();
        if n.kind != NameKind::FullyQualified && !ns.is_empty() {
            v.extend_from_slice(&ns);
            v.push(b'\\');
        }
        v.extend_from_slice(&n.name);
        String::from_utf8_lossy(&v).into_owned()
    };
    match &e.kind {
        ExprKind::Const(n) => Some(Some(join(n))),
        ExprKind::MagicConst(MagicConst::Class) => Some(Some("__CLASS__".into())),
        ExprKind::ClassConst { class, name } => {
            let cn = match class.as_ref() {
                ClassRef::Name(n) => {
                    let lc = n.name.to_ascii_lowercase();
                    if n.kind == NameKind::Unqualified
                        && matches!(lc.as_slice(), b"self" | b"parent" | b"static")
                    {
                        String::from_utf8_lossy(&n.name).into_owned()
                    } else {
                        join(n)
                    }
                }
                ClassRef::Expr(_) => return Some(None),
            };
            let k = match name {
                MemberName::Ident(i) => String::from_utf8_lossy(&i.name).into_owned(),
                MemberName::Expr(_) => return Some(None),
            };
            Some(Some(format!("{cn}::{k}")))
        }
        _ => None,
    }
}

fn rp_is_default_constant(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, idx, p) = this_param(vm, a)?;
    if !has_default(&p, &f, idx) {
        return Err(no_default(vm));
    }
    Ok(Value::Bool(default_constant_name(vm, &f, &p).is_some()))
}

fn rp_get_default_constant_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, idx, p) = this_param(vm, a)?;
    if !has_default(&p, &f, idx) {
        return Err(no_default(vm));
    }
    Ok(match default_constant_name(vm, &f, &p) {
        Some(Some(n)) => Value::Str(ZStr::from(n)),
        _ => Value::Null,
    })
}

fn rp_has_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_param(vm, a)?.2.ty.is_some()))
}
fn rp_get_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _, p) = this_param(vm, a)?;
    let scope = f.scope(vm);
    match p.ty {
        Some(t) => super::types::type_value(vm, &t, scope.as_ref(), true),
        None => Ok(Value::Null),
    }
}
fn rp_allows_null(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_param(vm, a)?.2;
    Ok(Value::Bool(p.ty.as_ref().is_none_or(TypeDecl::allows_null)))
}
fn rp_by_ref(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_param(vm, a)?.2.by_ref))
}
fn rp_by_val(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _, p) = this_param(vm, a)?;
    // `ZEND_SEND_PREFER_REF` parameters of internal functions accept both.
    let prefer_ref = !f.func.is_user() && p.by_ref && p.default_text.is_some();
    Ok(Value::Bool(!p.by_ref || prefer_ref))
}
fn rp_is_variadic(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_param(vm, a)?.2.variadic))
}
fn rp_is_promoted(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_param(vm, a)?.2.promote != 0))
}
fn rp_is_array(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_param(vm, a)?.2;
    Ok(Value::Bool(p.ty.as_ref().is_some_and(|t| {
        t.mask & !mask::NULL == mask::ARRAY && !t.has_classes()
    })))
}
fn rp_is_callable(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_param(vm, a)?.2;
    Ok(Value::Bool(p.ty.as_ref().is_some_and(|t| {
        t.mask & !mask::NULL == mask::CALLABLE && !t.has_classes()
    })))
}
fn rp_get_attributes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _, p) = this_param(vm, a)?;
    let scope = f.scope(vm);
    super::attribute::attributes_array_in(
        vm,
        &p.attributes,
        super::target::PARAMETER,
        scope.as_ref(),
        super::function::fn_file(&f),
        a,
    )
}
fn rp_get_declaring_function(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _, _) = this_param(vm, a)?;
    if let (Some(m), Some(c)) = (&f.method, &f.class) {
        return method_value(vm, &m.clone(), &c.clone());
    }
    match f.scope(vm) {
        Some(s) if f.closure.is_none() => {
            let n = f.name();
            match s.find_method(n.as_bytes()).cloned() {
                Some(m) => method_value(vm, &m, &s),
                None => function_value(vm, f),
            }
        }
        Some(s) => {
            let name = f.name();
            super::new_refl(
                vm,
                "ReflectionMethod",
                Refl::Function(f.clone()),
                Some(&name),
                Some(&s.name),
            )
        }
        None => function_value(vm, f),
    }
}
fn rp_get_declaring_class(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _, _) = this_param(vm, a)?;
    match f.scope(vm) {
        Some(s) => class_value(vm, &s),
        None => Ok(Value::Null),
    }
}
fn rp_get_class(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    vm.deprecated("Method ReflectionParameter::getClass() is deprecated since 8.0, use ReflectionParameter::getType() instead")?;
    let (f, _, p) = this_param(vm, a)?;
    let Some(t) = &p.ty else {
        return Ok(Value::Null);
    };
    let cn = if t.classes.is_empty()
        && t.intersections.is_empty()
        && t.mask & !mask::NULL == mask::ITERABLE
    {
        // `iterable` is `Traversable|array`: its class is `Traversable`.
        ZStr::from("Traversable")
    } else if t.classes.len() != 1 || !t.intersections.is_empty() || t.mask & !mask::NULL != 0 {
        return Ok(Value::Null);
    } else {
        t.classes[0].clone()
    };
    let scope = f.scope(vm);
    let c =
        if cn.eq_ignore_ascii_case(b"self") {
            match scope {
                Some(s) => s,
                None => {
                    return Err(rexc(
                        vm,
                        "Parameter uses \"self\" as type but function is not a class member",
                    ))
                }
            }
        } else if cn.eq_ignore_ascii_case(b"parent") {
            match scope {
                Some(s) => match &s.parent {
                    Some(p) => p.clone(),
                    None => return Err(rexc(
                        vm,
                        "Parameter uses \"parent\" as type although class does not have a parent",
                    )),
                },
                None => {
                    return Err(rexc(
                        vm,
                        "Parameter uses \"parent\" as type but function is not a class member",
                    ))
                }
            }
        } else {
            class_arg(vm, &Value::Str(cn), "Class")?
        };
    class_value(vm, &c)
}

fn deprecated_wrap(
    vm: &mut Vm,
    a: &mut Args,
    which: &str,
    f: fn(&mut Vm, &mut Args) -> VmResult<Value>,
) -> VmResult<Value> {
    vm.deprecated(&format!("Method ReflectionParameter::{which}() is deprecated since 8.0, use ReflectionParameter::getType() instead"))?;
    f(vm, a)
}

fn rp_is_array_dep(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    deprecated_wrap(vm, a, "isArray", rp_is_array)
}
fn rp_is_callable_dep(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    deprecated_wrap(vm, a, "isCallable", rp_is_callable)
}

fn clone_error(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(rexc(vm, "Cannot clone object using __clone()"))
}

/// Registers `ReflectionParameter`.
pub(crate) fn register(vm: &mut Vm) {
    ClassBuilder::new("ReflectionParameter")
        .implements(vm, &["Reflector"])
        .hooks(super::constant::hooks())
        .flags(cflags::NOT_SERIALIZABLE | cflags::NO_CLONE)
        .prop("name", RO, Some("string"), None)
        .method("__clone(): void", PRIV, clone_error)
        .method(
            "__construct($function, int|string $param)",
            PUB,
            rp_construct,
        )
        .method("__toString(): string", PUB, rp_to_string)
        .method("getDocComment(): string|false", PUB, rp_get_doc_comment)
        .method("getName(): string", PUB, rp_get_name)
        .method("isPassedByReference(): bool", PUB, rp_by_ref)
        .method("canBePassedByValue(): bool", PUB, rp_by_val)
        .method(
            "getDeclaringFunction(): ReflectionFunctionAbstract",
            PUB,
            rp_get_declaring_function,
        )
        .method(
            "getDeclaringClass(): ?ReflectionClass",
            PUB,
            rp_get_declaring_class,
        )
        .method("getClass(): ?ReflectionClass", PUB, rp_get_class)
        .method("hasType(): bool", PUB, rp_has_type)
        .method("getType(): ?ReflectionType", PUB, rp_get_type)
        .method("isArray(): bool", PUB, rp_is_array_dep)
        .method("isCallable(): bool", PUB, rp_is_callable_dep)
        .method("allowsNull(): bool", PUB, rp_allows_null)
        .method("getPosition(): int", PUB, rp_get_position)
        .method("isOptional(): bool", PUB, rp_is_optional)
        .method("isDefaultValueAvailable(): bool", PUB, rp_has_default)
        .method("getDefaultValue(): mixed", PUB, rp_get_default)
        .method(
            "isDefaultValueConstant(): bool",
            PUB,
            rp_is_default_constant,
        )
        .method(
            "getDefaultValueConstantName(): ?string",
            PUB,
            rp_get_default_constant_name,
        )
        .method("isVariadic(): bool", PUB, rp_is_variadic)
        .method("isPromoted(): bool", PUB, rp_is_promoted)
        .method(
            "getAttributes(?string $name = null, int $flags = 0): array",
            PUB,
            rp_get_attributes,
        )
        .register(vm);
}
