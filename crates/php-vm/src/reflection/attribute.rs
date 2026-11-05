//! `ReflectionAttribute` and the `getAttributes()` helper shared by the
//! reflectors.

use super::class::{bind_args, mark_ctor_failed};
use super::{rexc, target, this_refl, AttrRef, Refl, PRIV, PUB};
use crate::builtins::classbuilder::ClassBuilder;
use crate::class::{cflags, AttributeInfo, Class, ConstExpr};
use crate::func::{flags, Args};
use crate::vm::{PendingCall, Vm, VmResult};
use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};
use std::rc::Rc;

/// `reflect_attributes`: the `getAttributes(?string $name, int $flags)`
/// implementation.
#[allow(dead_code)]
pub(crate) fn attributes_array(
    vm: &mut Vm,
    attrs: &[AttributeInfo],
    tgt: i64,
    scope: Option<&Rc<Class>>,
    a: &Args,
) -> VmResult<Value> {
    attributes_array_in(vm, attrs, tgt, scope, None, a)
}

/// [`attributes_array`] with the declaring file.
pub(crate) fn attributes_array_in(
    vm: &mut Vm,
    attrs: &[AttributeInfo],
    tgt: i64,
    scope: Option<&Rc<Class>>,
    file: Option<ZStr>,
    a: &Args,
) -> VmResult<Value> {
    let name = match a.get(0) {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    };
    let fl = match a.get(1) {
        Some(Value::Int(i)) => i,
        _ => 0,
    };
    if fl & !2 != 0 {
        return Err(vm.make_throw(
            "ValueError",
            &format!(
                "{}(): Argument #2 ($flags) must be a valid attribute filter flag",
                a.func_name_qualified()
            ),
        ));
    }
    let mut base: Option<Rc<Class>> = None;
    let mut name = name;
    if let (Some(n), true) = (&name, fl & 2 != 0) {
        match vm.lookup_class(n)? {
            Some(c) => base = Some(c),
            None => {
                return Err(vm.make_throw(
                    "Error",
                    &format!("Class \"{}\" not found", n.to_string_lossy()),
                ))
            }
        }
        name = None;
    }
    let mut out = Vec::new();
    for at in attrs {
        if let Some(f) = &name {
            let f = f.as_bytes().strip_prefix(b"\\").unwrap_or(f.as_bytes());
            if !at.name.eq_ignore_ascii_case(f) {
                continue;
            }
        } else if let Some(b) = &base {
            match vm.lookup_class(&at.name)? {
                Some(ac) if ac.is_subclass_of(b) => {}
                _ => continue,
            }
        }
        let repeated = attrs
            .iter()
            .filter(|x| x.name.eq_ignore_ascii_case(at.name.as_bytes()))
            .count()
            > 1;
        let r = AttrRef {
            info: at.clone(),
            target: tgt,
            repeated,
            scope: scope.cloned(),
            file: file.clone(),
        };
        out.push(super::new_refl(
            vm,
            "ReflectionAttribute",
            Refl::Attr(Rc::new(r)),
            Some(&at.name),
            None,
        )?);
    }
    Ok(Value::Array(ZArray::from_values(out)))
}

trait QualifiedName {
    fn func_name_qualified(&self) -> String;
}

impl QualifiedName for Args {
    fn func_name_qualified(&self) -> String {
        let n = self.func_name();
        match &self.this {
            Some(o) => {
                let c = crate::object::class_of(o).clone();
                let lc = ZStr::from(n.to_ascii_lowercase());
                let owner = c
                    .methods
                    .get(&lc)
                    .map_or_else(|| c.name.clone(), |m| m.scope().name.clone());
                format!("{}::{n}", owner.to_string_lossy())
            }
            None => n,
        }
    }
}

fn this_attr(vm: &mut Vm, a: &Args) -> VmResult<Rc<AttrRef>> {
    match this_refl(vm, a)? {
        Refl::Attr(r) => Ok(r),
        _ => Err(vm.make_throw(
            "Error",
            "Internal error: Failed to retrieve the reflection object",
        )),
    }
}

fn ra_get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(this_attr(vm, a)?.info.name.clone()))
}
fn ra_in_ns(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(super::in_ns(&this_attr(vm, a)?.info.name)))
}
fn ra_get_ns(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(super::ns_name(&this_attr(vm, a)?.info.name)))
}
fn ra_get_short_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(super::short_name(&this_attr(vm, a)?.info.name)))
}
fn ra_get_target(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(this_attr(vm, a)?.target))
}
fn ra_is_repeated(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_attr(vm, a)?.repeated))
}

/// Evaluates the arguments of an attribute (`zend_get_attribute_value`).
fn attr_args(
    vm: &mut Vm,
    info: &AttributeInfo,
    scope: Option<&Rc<Class>>,
) -> VmResult<(Vec<Value>, Vec<(ZStr, Value)>)> {
    let mut pos = Vec::new();
    let mut named = Vec::new();
    for (n, e) in &info.args {
        let v = vm.eval_const_expr(e, scope)?;
        match n {
            Some(n) => named.push((n.clone(), v)),
            None => pos.push(v),
        }
    }
    Ok((pos, named))
}

fn ra_get_arguments(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_attr(vm, a)?;
    let mut t = HashTable::new();
    for (n, e) in &r.info.args {
        let v = vm.eval_const_expr(e, r.scope.as_ref())?;
        match n {
            Some(n) => {
                t.insert(ArrayKey::Str(n.clone()), v);
            }
            None => {
                let _ = t.push(v);
            }
        }
    }
    Ok(Value::Array(ZArray::from_table(t)))
}

fn ra_to_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_attr(vm, a)?;
    let mut out = b"Attribute [ ".to_vec();
    out.extend_from_slice(r.info.name.as_bytes());
    out.extend_from_slice(b" ]");
    if r.info.args.is_empty() {
        out.push(b'\n');
    } else {
        out.extend_from_slice(
            format!(" {{\n  - Arguments [{}] {{\n", r.info.args.len()).as_bytes(),
        );
        for (i, (n, e)) in r.info.args.iter().enumerate() {
            out.extend_from_slice(format!("    Argument #{i} [ ").as_bytes());
            if let Some(n) = n {
                out.extend_from_slice(n.as_bytes());
                out.extend_from_slice(b" = ");
            }
            let repr = match e {
                ConstExpr::Value(v) => super::export::format_default_value(vm, v),
                ConstExpr::Code(_) => match source_arg(vm, &r, i) {
                    Some(x) => x,
                    None => {
                        let v = vm.eval_const_expr(e, r.scope.as_ref())?;
                        super::export::format_default_value(vm, &v)
                    }
                },
            };
            out.extend_from_slice(&repr);
            out.extend_from_slice(b" ]\n");
        }
        out.extend_from_slice(b"  }\n}\n");
    }
    Ok(Value::Str(ZStr::from(out)))
}

/// The exported source of argument `i` of an attribute.
fn source_arg(vm: &Vm, r: &AttrRef, i: usize) -> Option<Vec<u8>> {
    let file = r.file.clone().or_else(|| {
        vm.frames
            .iter()
            .rev()
            .find(|f| f.kind != crate::vm::FrameKind::Builtin)
            .map(|f| f.code.filename.clone())
    })?;
    let args =
        super::source::attribute_args(vm, file.as_bytes(), r.info.line, r.info.name.as_bytes())?;
    let a = args.get(i)?;
    let ns = super::export::namespace_of(&r.info.name);
    let e = match &a.kind {
        php_parser::ast::ArgKind::Positional(x)
        | php_parser::ast::ArgKind::Named { value: x, .. } => x.clone(),
        _ => return None,
    };
    Some(super::export::export_expr_ns(e, &ns))
}

/// The error of an internal attribute's validator, kept for
/// `#[\DelayedTargetValidation]` (`zend_attribute.validation_error`).
fn delayed_validation_error(c: &Class, r: &AttrRef) -> Option<String> {
    let scope = r.scope.as_ref()?;
    if r.target != target::CLASS {
        return None;
    }
    let n = super::dn(&scope.name);
    let kind = if scope.is_trait() {
        "trait"
    } else if scope.is_interface() {
        "interface"
    } else if scope.is_enum() {
        "enum"
    } else {
        "class"
    };
    match c.lc_name.as_bytes() {
        b"attribute" => {
            let what = if scope.is_trait() || scope.is_interface() || scope.is_enum() {
                kind
            } else if scope.flags & cflags::EXPLICIT_ABSTRACT != 0 {
                "abstract class"
            } else {
                return None;
            };
            Some(format!("Cannot apply #[\\Attribute] to {what} {n}"))
        }
        b"allowdynamicproperties" => {
            let what = if scope.is_trait() || scope.is_interface() || scope.is_enum() {
                kind
            } else if scope.flags & cflags::READONLY != 0 {
                "readonly class"
            } else {
                return None;
            };
            Some(format!(
                "Cannot apply #[\\AllowDynamicProperties] to {what} {n}"
            ))
        }
        b"deprecated" if !scope.is_trait() => {
            Some(format!("Cannot apply #[\\Deprecated] to {kind} {n}"))
        }
        _ => None,
    }
}

/// `zend_get_attribute_target_names`.
pub(crate) fn target_names(f: i64) -> String {
    let names = [
        "class",
        "function",
        "method",
        "property",
        "class constant",
        "parameter",
        "constant",
    ];
    let mut v = Vec::new();
    for (i, n) in names.iter().enumerate() {
        if f & (1 << i) != 0 {
            v.push(*n);
        }
    }
    v.join(", ")
}

/// The `Attribute` flags of an attribute class (`None`: not an attribute).
pub(crate) fn attribute_flags(vm: &mut Vm, c: &Rc<Class>) -> VmResult<Option<i64>> {
    if c.flags & cflags::INTERNAL != 0 {
        return Ok(match c.lc_name.as_bytes() {
            b"attribute" | b"allowdynamicproperties" => Some(target::CLASS),
            b"returntypewillchange" => Some(target::METHOD),
            b"sensitiveparameter" => Some(target::PARAMETER),
            b"override" => Some(target::METHOD | target::PROPERTY | target::CLASS_CONSTANT),
            b"deprecated" => Some(
                target::METHOD
                    | target::FUNCTION
                    | target::CLASS_CONSTANT
                    | target::CONSTANT
                    | target::CLASS,
            ),
            b"nodiscard" => Some(target::METHOD | target::FUNCTION),
            b"delayedtargetvalidation" => Some(target::ALL),
            _ => None,
        });
    }
    let Some(marker) = c
        .attributes
        .iter()
        .find(|x| x.name.eq_ignore_ascii_case(b"Attribute"))
        .cloned()
    else {
        return Ok(None);
    };
    let Some((_, e)) = marker.args.first() else {
        return Ok(Some(target::ALL));
    };
    let v = vm.eval_const_expr(e, Some(c))?;
    match v {
        Value::Int(i) => {
            if i & !(target::ALL | target::IS_REPEATABLE) != 0 {
                return Err(vm.make_throw("Error", "Invalid attribute flags specified"));
            }
            Ok(Some(i))
        }
        other => Err(vm.make_throw(
            "Error",
            &format!(
                "Attribute::__construct(): Argument #1 ($flags) must be of type int, {} given",
                crate::typecheck::given_type(&other)
            ),
        )),
    }
}

fn ra_new_instance(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_attr(vm, a)?;
    let Some(c) = vm.lookup_class(&r.info.name)? else {
        return Err(vm.make_throw(
            "Error",
            &format!("Attribute class \"{}\" not found", r.info.name),
        ));
    };
    let Some(fl) = attribute_flags(vm, &c)? else {
        return Err(vm.make_throw(
            "Error",
            &format!(
                "Attempting to use non-attribute class \"{}\" as attribute",
                r.info.name
            ),
        ));
    };
    if r.target & fl == 0 {
        return Err(vm.make_throw(
            "Error",
            &format!(
                "Attribute \"{}\" cannot target {} (allowed targets: {})",
                r.info.name,
                target_names(r.target),
                target_names(fl)
            ),
        ));
    }
    if let Some(msg) = delayed_validation_error(&c, &r) {
        return Err(vm.make_throw("Error", &msg));
    }
    if c.flags & cflags::INTERNAL == 0 && fl & target::IS_REPEATABLE == 0 && r.repeated {
        return Err(vm.make_throw(
            "Error",
            &format!("Attribute \"{}\" must not be repeated", r.info.name),
        ));
    }
    let (pos, named) = attr_args(vm, &r.info, r.scope.as_ref())?;
    let o = vm.instantiate(&c)?;
    match c.magic.construct.clone() {
        Some(m) => {
            if m.visibility() != flags::PUBLIC {
                mark_ctor_failed(&o);
                let vis = if m.visibility() == flags::PRIVATE {
                    "private"
                } else {
                    "protected"
                };
                return Err(vm.make_throw(
                    "Error",
                    &format!(
                        "Call to {vis} {}::__construct() from global scope",
                        super::dn(&c.name)
                    ),
                ));
            }
            let mut call = PendingCall {
                this: Some(o.clone()),
                scope: Some(m.scope()),
                called: Some(c.clone()),
                ..PendingCall::new(m.func.clone())
            };
            if let Err(e) = bind_args(vm, &mut call, pos, named) {
                mark_ctor_failed(&o);
                return Err(e);
            }
            if let Err(e) = vm.call_pending(call) {
                mark_ctor_failed(&o);
                return Err(e);
            }
        }
        None => {
            if let Some((n, _)) = named.first() {
                return Err(vm.make_throw(
                    "Error",
                    &format!("Unknown named parameter ${}", n.to_string_lossy()),
                ));
            }
        }
    }
    Ok(Value::Object(o))
}

fn ra_construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(rexc(vm, "Cannot directly instantiate ReflectionAttribute"))
}

fn clone_error(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(rexc(vm, "Cannot clone object using __clone()"))
}

/// Registers `ReflectionAttribute`.
pub(crate) fn register(vm: &mut Vm) {
    ClassBuilder::new("ReflectionAttribute")
        .implements(vm, &["Reflector"])
        .flags(cflags::NOT_SERIALIZABLE | cflags::NO_CLONE)
        .hooks(super::constant::hooks())
        .constant("IS_INSTANCEOF", Value::Int(2))
        .prop("name", super::RO, Some("string"), None)
        .method("getName(): string", PUB, ra_get_name)
        .method("inNamespace(): bool", PUB, ra_in_ns)
        .method("getNamespaceName(): string", PUB, ra_get_ns)
        .method("getShortName(): string", PUB, ra_get_short_name)
        .method("getTarget(): int", PUB, ra_get_target)
        .method("isRepeated(): bool", PUB, ra_is_repeated)
        .method("getArguments(): array", PUB, ra_get_arguments)
        .method("newInstance(): object", PUB, ra_new_instance)
        .method("__toString(): string", PUB, ra_to_string)
        .method("__clone(): void", PRIV, clone_error)
        .method("__construct(mixed ...$args)", PRIV, ra_construct)
        .register(vm);
}

#[cfg(test)]
mod tests {
    use super::target_names;

    #[test]
    fn target_name_lists() {
        assert_eq!(target_names(1), "class");
        assert_eq!(target_names(2 | 4), "function, method");
        assert_eq!(
            target_names(127),
            "class, function, method, property, class constant, parameter, constant"
        );
    }
}
