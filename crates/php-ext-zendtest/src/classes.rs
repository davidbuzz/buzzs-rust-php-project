//! The classes, interfaces, traits, enums and attribute classes of
//! `test.stub.php`, registered in `PHP_MINIT(zend_test)` order.

use crate::patch::{method_in, rebuild_class};
use crate::util::{attribute, s, value_name};
use php_runtime::{Value, ZArray, ZStr};
use php_vm::class::{cflags, ClassConst, ClassDecl, ClassKind, ConstExpr, PropDecl, StaticProp};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ModuleBuilder, VmResult};
use php_vm::types::TypeDecl;
use php_vm::Vm;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

fn none(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

fn ret_true(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(true))
}

fn ret_zero(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(0))
}

/// A type as the compiler resolves it in a declaration (exact union,
/// intersection and DNF shapes, which the stub-signature parser
/// flattens): compiled from `function f(TYPE $x) {}`.
pub(crate) fn type_of(_vm: &mut Vm, ty: &str) -> Option<TypeDecl> {
    let src = format!("<?php function __zend_test_type_probe({ty} $x) {{}}");
    let program = php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).ok()?;
    let unit = php_vm::compiler::compile(
        &program,
        std::rc::Rc::from(src.as_bytes()),
        php_vm::compiler::CompileOptions::default(),
    )
    .ok()?;
    let fns = unit.decls.functions.borrow();
    fns.first()?.params.first()?.ty.clone()
}

/// Sets the declared type of the class constant `name`.
fn set_const_type(c: &mut php_vm::class::Class, name: &str, ty: Option<TypeDecl>) {
    if let Some(&i) = c.const_map.get(&ZStr::from(name)) {
        let mut nc: ClassConst = (*c.consts[i]).clone();
        nc.ty = ty;
        c.consts[i] = Rc::new(nc);
    }
}

/// Marks the native method `lc` of `c` deprecated (`@deprecated`).
fn deprecate_method(c: &mut php_vm::class::Class, lc: &str) {
    let key = ZStr::from(lc);
    let Some(m) = c.methods.get(&key).cloned() else {
        return;
    };
    let php_vm::func::Func::Builtin(b) = &m.func else {
        return;
    };
    let mut nb = (**b).clone();
    nb.flags |= php_vm::func::flags::DEPRECATED;
    c.methods.insert(
        key,
        Rc::new(php_vm::func::Method {
            name: m.name.clone(),
            func: php_vm::func::Func::Builtin(Rc::new(nb)),
            flags: m.flags | php_vm::func::flags::DEPRECATED,
            scope: m.scope.clone(),
            proto_scope: m.proto_scope.clone(),
        }),
    );
}

/// Sets the declared type of the static property `name`.
fn set_static_type(c: &mut php_vm::class::Class, name: &str, ty: Option<TypeDecl>) {
    if let Some(&i) = c.static_map.get(&ZStr::from(name)) {
        c.static_props[i].ty = ty;
    }
}

/// Edits the property info `name` declared by `c`.
fn edit_prop(
    c: &mut php_vm::class::Class,
    name: &str,
    edit: impl FnOnce(&mut php_vm::class::PropInfo),
) {
    let key = ZStr::from(name);
    if let Some(pi) = c.prop_map.get(&key).cloned() {
        let mut np = (*pi).clone();
        edit(&mut np);
        let np = Rc::new(np);
        if let Some(slot) = c.props.get_mut(np.slot as usize) {
            *slot = np.clone();
        }
        c.prop_map.insert(key, np);
    }
}

// ---------------------------------------------------------------------
// _ZendTestClass
// ---------------------------------------------------------------------

/// `_ZendTestClass::is_object()`: an internal method named like a
/// function returning a different type.
fn is_object(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(42))
}

fn to_string(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(s(""))
}

/// `returnsStatic()`: an instance of the called class.
fn returns_static(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cls = match (&a.called, &a.this) {
        (Some(c), _) => c.clone(),
        (None, Some(o)) => php_vm::object::class_of(o).clone(),
        (None, None) => return Ok(Value::Null),
    };
    Ok(Value::Object(vm.new_object(&cls)?))
}

fn throws_dummy(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw("Error", "Dummy"))
}

/// `variadicTest(string|Iterator ...$elements): static`.
fn variadic_test(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    for (i, v) in a.values.iter().enumerate() {
        let ok = match v {
            Value::Str(_) => true,
            Value::Object(o) => php_vm::object::class_of(o).instance_of_name(b"iterator"),
            _ => false,
        };
        if !ok {
            return Err(a.type_error(
                vm,
                i + 1,
                &format!(
                    "must be of class Iterator or a string, {} given",
                    crate::util::type_name(v)
                ),
            ));
        }
    }
    returns_static(vm, a)
}

/// `takesUnionType(stdclass|Iterator $arg): void`.
fn takes_union_type(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

/// The `test` trampoline of `zend_test_class_method_get` /
/// `zend_test_class_static_method_get`: returns the called name.
fn trampoline_test(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    if let Some((n, _)) = a.named.first() {
        return Err(vm.throw(
            "Error",
            &format!("Unknown named parameter ${}", n.to_string_lossy()),
        ));
    }
    Ok(s("test"))
}

/// `ZendTestTmpMethods::testTmpMethodWithArgInfo()`.
fn tmp_method(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

const DOUBLE_QUOTE_ESCAPED: &[u8] = b"BEGIN \n\r\t\x0b\x1b\x0c\\$\"AAA END";
const SINGLE_QUOTE_ESCAPED: &str = r#"BEGIN \n\r\t\v\e\f\\\\$\"\101\x41\u{41} END"#;
const ESCAPE_INTERPOLATED: &str = r"begin $ \$ end";

#[allow(clippy::too_many_lines)]
fn register_test_class(vm: &mut Vm) {
    let pub_ = acc::PUBLIC;
    let cls = ClassBuilder::new("_ZendTestClass")
        .implements(vm, &["_ZendTestInterface", "Stringable"])
        .flags(class_flags::NO_CLONE)
        .constant("TYPED_CLASS_CONST1", Value::Array(ZArray::new()))
        .constant("TYPED_CLASS_CONST2", Value::Int(42))
        .constant("TYPED_CLASS_CONST3", Value::Int(1))
        .deprecated_constant("ZEND_TEST_DEPRECATED", Value::Int(42), "", "")
        .deprecated_constant("ZEND_TEST_DEPRECATED_ATTR", Value::Int(42), "", "custom message")
        .static_prop("_StaticProp", pub_, Value::Null)
        .static_prop("staticIntProp", pub_, Value::Int(123))
        .static_prop(
            "doubleQuoteEscaped",
            pub_,
            Value::Str(ZStr::from(DOUBLE_QUOTE_ESCAPED)),
        )
        .static_prop("singleQuoteEscaped", pub_, s(SINGLE_QUOTE_ESCAPED))
        .static_prop("escapeInterpolated", pub_, s(ESCAPE_INTERPOLATED))
        .prop("intProp", pub_, Some("int"), Some(Value::Int(123)))
        .prop("classProp", pub_, Some("?stdClass"), Some(Value::Null))
        .prop(
            "classUnionProp",
            pub_,
            Some("stdClass|Iterator|null"),
            Some(Value::Null),
        )
        .prop(
            "classIntersectionProp",
            pub_,
            Some("Traversable&Countable"),
            None,
        )
        .prop("readonlyProp", pub_ | acc::READONLY, Some("int"), None)
        .prop("finalProp", pub_ | acc::FINAL, Some("int"), None)
        .prop(
            "dnfProperty",
            pub_,
            Some("Iterator|(Traversable&Countable)"),
            None,
        )
        .method("is_object(): int", pub_ | acc::STATIC, is_object)
        .method("__toString(): string", pub_, to_string)
        .method("returnsStatic(): static", pub_, returns_static)
        .method("returnsThrowable(): Throwable", pub_, throws_dummy)
        .method(
            "variadicTest(string|Iterator ...$elements): static",
            pub_ | acc::STATIC,
            variadic_test,
        )
        .method(
            "takesUnionType(stdClass|Iterator $arg): void",
            pub_,
            takes_union_type,
        )
        .method(
            "test(mixed ...$args): string",
            pub_ | acc::STATIC | acc::STATIC_KEEPS_THIS,
            trampoline_test,
        )
        .method(
            "testTmpMethodWithArgInfo(Foo|Bar|null $tmpMethodParamName = null, string $tmpMethodParamWithStringDefaultValue = \"tmpMethodParamWithStringDefaultValue\")",
            pub_,
            tmp_method,
        )
        .register(vm);
    let tys = [
        ("TYPED_CLASS_CONST1", type_of(vm, "mixed")),
        ("TYPED_CLASS_CONST2", type_of(vm, "int|array")),
        ("TYPED_CLASS_CONST3", type_of(vm, "int|string")),
        ("ZEND_TEST_DEPRECATED", type_of(vm, "int")),
        ("ZEND_TEST_DEPRECATED_ATTR", type_of(vm, "int")),
    ];
    let int_ty = type_of(vm, "int");
    let str_ty = type_of(vm, "string");
    let inter_ty = type_of(vm, "Traversable&Countable");
    let dnf_ty = type_of(vm, "Iterator|(Traversable&Countable)");
    let cls = rebuild_class(vm, &cls, |c, _| {
        edit_prop(c, "classIntersectionProp", |p| p.ty = inter_ty);
        edit_prop(c, "dnfProperty", |p| p.ty = dnf_ty);
        for (n, t) in tys {
            set_const_type(c, n, t);
        }
        set_static_type(c, "staticIntProp", int_ty);
        deprecate_method(c, "__tostring");
        for n in [
            "doubleQuoteEscaped",
            "singleQuoteEscaped",
            "escapeInterpolated",
        ] {
            set_static_type(c, n, str_ty.clone());
        }
    });
    // `@alias _ZendTestClassAlias`.
    vm.classes
        .insert(ZStr::from("_zendtestclassalias"), cls.clone());

    ClassBuilder::new("_ZendTestChildClass")
        .extends(vm, "_ZendTestClass")
        .method("returnsThrowable(): Exception", pub_, throws_dummy)
        .register(vm);
}

// ---------------------------------------------------------------------
// Magic call classes
// ---------------------------------------------------------------------

/// The `(name, arguments)` of a native `__call`: the VM passes the
/// called method's arguments unpacked (named ones in `a.named`) and the
/// name through [`Vm::magic_call_name`]; a direct `->__call($n, $args)`
/// call passes them as written.
fn magic_call_args(vm: &Vm, a: &Args) -> (Value, Value) {
    match vm.magic_call_name() {
        Some(name) => {
            let mut t = php_runtime::HashTable::new();
            for v in &a.values {
                let _ = t.push(v.clone());
            }
            for (k, v) in &a.named {
                t.insert(k.clone(), v.clone());
            }
            (Value::Str(name), Value::Array(ZArray::from_table(t)))
        }
        None => (a.at(0), a.at(1)),
    }
}

/// `_ZendTestMagicCall::__call()`: `[$name, $args]`.
fn magic_call(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (name, args) = magic_call_args(vm, a);
    Ok(Value::Array(ZArray::from_values([name, args])))
}

/// `_ZendTestMagicCallForward::__call()`: calls the function `$name`
/// without arguments.
fn magic_call_forward(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _) = magic_call_args(vm, a);
    let r = vm.call_value(&f, Vec::new())?;
    Ok(match r {
        Value::Reference(r) => r.get(),
        v => v,
    })
}

// ---------------------------------------------------------------------
// Attributes
// ---------------------------------------------------------------------

/// `Attribute::TARGET_*`.
mod target {
    pub const CLASS: i64 = 1;
    pub const PROPERTY: i64 = 8;
    pub const PARAMETER: i64 = 32;
    pub const ALL: i64 = 127;
    pub const IS_REPEATABLE: i64 = 128;
}

/// `__construct(string $parameter)` of the parameter/property attributes.
fn attr_parameter_ctor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    vm.set_property_raw(&o, "parameter", Value::Str(a.str(0)));
    Ok(Value::Null)
}

/// `ZendTestAttributeWithArguments::__construct(mixed $arg)`.
fn attr_with_arguments_ctor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    vm.set_property_raw(&o, "arg", a.at(0));
    Ok(Value::Null)
}

/// Makes a registered class an attribute class (`#[Attribute(flags)]`,
/// `zend_mark_internal_attribute`).
fn mark_attribute(vm: &mut Vm, name: &str, flags: i64, doc: Option<&str>) {
    let Some(cls) = vm.find_class(name.as_bytes()) else {
        return;
    };
    rebuild_class(vm, &cls, |c, _| {
        c.attributes = vec![attribute("Attribute", vec![(None, Value::Int(flags))])];
        c.doc_comment = doc.map(ZStr::from);
    });
}

fn register_attributes(vm: &mut Vm) {
    let fin = class_flags::FINAL;
    ClassBuilder::new("ZendTestAttribute")
        .flags(fin)
        .register(vm);
    mark_attribute(vm, "ZendTestAttribute", target::ALL, None);
    ClassBuilder::new("ZendTestRepeatableAttribute")
        .flags(fin)
        .register(vm);
    mark_attribute(
        vm,
        "ZendTestRepeatableAttribute",
        target::ALL | target::IS_REPEATABLE,
        None,
    );
    ClassBuilder::new("ZendTestParameterAttribute")
        .flags(fin)
        .prop("parameter", acc::PUBLIC, Some("string"), None)
        .method(
            "__construct(string $parameter)",
            acc::PUBLIC,
            attr_parameter_ctor,
        )
        .register(vm);
    mark_attribute(vm, "ZendTestParameterAttribute", target::PARAMETER, None);
    ClassBuilder::new("ZendTestPropertyAttribute")
        .flags(fin)
        .prop("parameter", acc::PUBLIC, Some("string"), None)
        .method(
            "__construct(string $parameter)",
            acc::PUBLIC,
            attr_parameter_ctor,
        )
        .register(vm);
    mark_attribute(
        vm,
        "ZendTestPropertyAttribute",
        target::PROPERTY,
        Some("/**\n     * \"Lorem ipsum\"\n     * @see https://www.php.net\n     * @since 8.1\n     */"),
    );
    if let Some(cls) = vm.find_class(b"ZendTestPropertyAttribute") {
        rebuild_class(vm, &cls, |c, _| {
            edit_prop(c, "parameter", |p| {
                p.doc_comment = Some(ZStr::from(
                    "/**\n         * \"Lorem ipsum\"\n         * @see https://www.php.net\n         * @since 8.4\n         */",
                ));
            });
        });
    }
    ClassBuilder::new("ZendTestAttributeWithArguments")
        .flags(fin)
        .prop("arg", acc::PUBLIC | acc::READONLY, Some("mixed"), None)
        .method(
            "__construct(mixed $arg)",
            acc::PUBLIC,
            attr_with_arguments_ctor,
        )
        .register(vm);
    mark_attribute(vm, "ZendTestAttributeWithArguments", target::ALL, None);
    let _ = target::CLASS;
}

/// `zend_attribute_validate_zendtestattribute`.
fn validate_zend_test_attribute(target: u32) -> Option<String> {
    (i64::from(target) != target::CLASS)
        .then(|| "Only classes can be marked with #[ZendTestAttribute]".to_string())
}

/// `zend_mark_internal_attribute()` of the attribute classes.
fn register_internal_attributes(m: &mut ModuleBuilder<'_>) {
    let fl = |f: i64| u32::try_from(f).unwrap_or(0);
    m.internal_attribute(
        "ZendTestAttribute",
        fl(target::ALL),
        Some(validate_zend_test_attribute),
    );
    m.internal_attribute(
        "ZendTestRepeatableAttribute",
        fl(target::ALL | target::IS_REPEATABLE),
        None,
    );
    m.internal_attribute("ZendTestParameterAttribute", fl(target::PARAMETER), None);
    m.internal_attribute("ZendTestPropertyAttribute", fl(target::PROPERTY), None);
    m.internal_attribute("ZendTestAttributeWithArguments", fl(target::ALL), None);
}

fn param_attr_2(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(2))
}

fn param_attr_3(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(3))
}

fn param_attr_4(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(4))
}

/// Adds `#[ZendTestParameterAttribute(value)]` to the first parameter of
/// method `method` declared by `c`.
fn add_param_attribute(c: &mut php_vm::class::Class, method: &str, value: &str) {
    let lc = ZStr::from(method.to_ascii_lowercase());
    let Some(m) = c.methods.get(&lc).cloned() else {
        return;
    };
    let php_vm::func::Func::Builtin(b) = &m.func else {
        return;
    };
    let mut nb = (**b).clone();
    if let Some(p) = nb.params.first_mut() {
        p.attributes = vec![attribute(
            "ZendTestParameterAttribute",
            vec![(None, s(value))],
        )];
    }
    let nm = php_vm::func::Method {
        name: m.name.clone(),
        func: php_vm::func::Func::Builtin(Rc::new(nb)),
        flags: m.flags,
        scope: m.scope.clone(),
        proto_scope: m.proto_scope.clone(),
    };
    c.methods.insert(lc, Rc::new(nm));
}

fn register_attribute_users(vm: &mut Vm) {
    let cls = ClassBuilder::new("ZendAttributeTest")
        .constant("TEST_CONST", Value::Int(1))
        .prop("testProp", acc::PUBLIC, None, Some(Value::Null))
        .method("testMethod(): bool", acc::PUBLIC, ret_true)
        .register(vm);
    rebuild_class(vm, &cls, |c, _| {
        if let Some(&i) = c.const_map.get(&ZStr::from("TEST_CONST")) {
            let mut nc: ClassConst = (*c.consts[i]).clone();
            nc.attributes = vec![
                attribute("ZendTestRepeatableAttribute", vec![]),
                attribute("ZendTestRepeatableAttribute", vec![]),
            ];
            c.consts[i] = Rc::new(nc);
        }
        edit_prop(c, "testProp", |p| {
            p.attributes = vec![
                attribute("ZendTestRepeatableAttribute", vec![]),
                attribute("ZendTestPropertyAttribute", vec![(None, s("testProp"))]),
            ];
        });
    });
    vm.builtin_attributes.insert(
        ZStr::from("zendattributetest::testmethod"),
        Rc::new(vec![attribute("ZendTestAttribute", vec![])]),
    );

    let cls = ClassBuilder::new("ZendTestClassWithMethodWithParameterAttribute")
        .method(
            "no_override(string $parameter): int",
            acc::PUBLIC | acc::FINAL,
            param_attr_2,
        )
        .method(
            "override(string $parameter): int",
            acc::PUBLIC,
            param_attr_3,
        )
        .register(vm);
    rebuild_class(vm, &cls, |c, _| {
        add_param_attribute(c, "no_override", "value2");
        add_param_attribute(c, "override", "value3");
    });
    let cls = ClassBuilder::new("ZendTestChildClassWithMethodWithParameterAttribute")
        .extends(vm, "ZendTestClassWithMethodWithParameterAttribute")
        .method(
            "override(string $parameter): int",
            acc::PUBLIC,
            param_attr_4,
        )
        .register(vm);
    rebuild_class(vm, &cls, |c, _| {
        add_param_attribute(c, "override", "value4");
    });

    let cls = ClassBuilder::new("ZendTestClassWithPropertyAttribute")
        .prop("attributed", acc::PUBLIC, Some("string"), None)
        .register(vm);
    rebuild_class(vm, &cls, |c, _| {
        edit_prop(c, "attributed", |p| {
            p.attributes = vec![attribute("ZendTestAttribute", vec![])];
        });
    });
}

// ---------------------------------------------------------------------
// Traits
// ---------------------------------------------------------------------

fn trait_method(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(789))
}

fn trait_method2(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(101))
}

/// A `ClassDecl` carrying a trait's properties (what the linker copies
/// into classes using the trait).
fn trait_decl(name: &str, props: Vec<PropDecl>) -> Rc<ClassDecl> {
    Rc::new(ClassDecl {
        name: ZStr::from(name),
        kind: ClassKind::Trait,
        flags: cflags::TRAIT,
        parent: None,
        interfaces: Vec::new(),
        traits: Vec::new(),
        trait_rules: Vec::new(),
        consts: Vec::new(),
        props,
        methods: Vec::new(),
        cases: Vec::new(),
        case_attributes: Vec::new(),
        case_doc_comments: Vec::new(),
        backing: None,
        attributes: Vec::new(),
        doc_comment: None,
        filename: ZStr::from(""),
        line_start: 0,
        line_end: 0,
    })
}

fn prop_decl(name: &str, flags: u32, ty: Option<TypeDecl>, default: Option<Value>) -> PropDecl {
    PropDecl {
        name: ZStr::from(name),
        default: default.map(ConstExpr::Value),
        flags,
        ty,
        doc_comment: None,
        attributes: Vec::new(),
        line: 0,
        hooks: None,
    }
}

/// Registers a trait: the methods through `ClassBuilder`, properties
/// through a synthesized declaration.
fn register_trait(
    vm: &mut Vm,
    builder: ClassBuilder,
    name: &str,
    props: Vec<PropDecl>,
) -> Rc<php_vm::class::Class> {
    let cls = builder.flags(cflags::TRAIT).register(vm);
    let decl = trait_decl(name, props);
    rebuild_class(vm, &cls, |c, _| c.decl = Some(decl))
}

fn register_traits(vm: &mut Vm) {
    let union_ty = type_of(vm, "Traversable|Countable");
    let int_ty = type_of(vm, "int");
    register_trait(
        vm,
        ClassBuilder::new("_ZendTestTrait").method("testMethod(): bool", acc::PUBLIC, ret_true),
        "_ZendTestTrait",
        vec![
            prop_decl("testProp", acc::PUBLIC, None, Some(Value::Null)),
            prop_decl("classUnionProp", acc::PUBLIC, union_ty, None),
        ],
    );
    let t1 = register_trait(
        vm,
        ClassBuilder::new("_ZendTestTraitForInternalClass")
            .constant("ZEND_TRAIT_CONST", Value::Int(123))
            .method("traitMethod(): int", acc::PUBLIC, trait_method),
        "_ZendTestTraitForInternalClass",
        vec![prop_decl(
            "traitProp",
            acc::PUBLIC,
            int_ty.clone(),
            Some(Value::Int(456)),
        )],
    );
    let t2 = register_trait(
        vm,
        ClassBuilder::new("_ZendTestTraitForInternalClass2")
            .constant("ZEND_TRAIT_CONST2", Value::Int(321))
            .method("traitMethod2(): int", acc::PUBLIC, trait_method2),
        "_ZendTestTraitForInternalClass2",
        vec![prop_decl(
            "staticTraitProp",
            acc::PUBLIC | acc::STATIC,
            int_ty.clone(),
            Some(Value::Int(999)),
        )],
    );
    // `class _ZendTestClassWithTraits { use ...; use ...2; }`: the trait
    // members are bound into the internal class at registration.
    let cls = ClassBuilder::new("_ZendTestClassWithTraits")
        .constant("ZEND_TRAIT_CONST", Value::Int(123))
        .constant("ZEND_TRAIT_CONST2", Value::Int(321))
        .prop("traitProp", acc::PUBLIC, Some("int"), Some(Value::Int(456)))
        .register(vm);
    rebuild_class(vm, &cls, |c, weak| {
        c.traits = vec![t1.clone(), t2.clone()];
        for t in [&t1, &t2] {
            for lc in &t.method_order {
                if let Some(m) = t.methods.get(lc) {
                    c.methods.insert(lc.clone(), method_in(m, weak));
                    c.method_order.push(lc.clone());
                }
            }
        }
        c.static_map
            .insert(ZStr::from("staticTraitProp"), c.static_props.len());
        c.static_props.push(StaticProp {
            name: ZStr::from("staticTraitProp"),
            flags: acc::PUBLIC | acc::STATIC,
            ty: int_ty,
            class: c.name.clone(),
            cell: Rc::new(RefCell::new(Value::Int(999))),
            default: None,
            initialized: Rc::new(Cell::new(true)),
            doc_comment: None,
        });
    });
}

// ---------------------------------------------------------------------
// ZendTestForbidDynamicCall
// ---------------------------------------------------------------------

/// `zend_forbid_dynamic_call()` needs the caller's call flags, which the
/// VM does not expose to native methods; the methods are no-ops.
fn forbid_dynamic_call(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

// ---------------------------------------------------------------------
// Namespaced classes
// ---------------------------------------------------------------------

fn register_namespaced(vm: &mut Vm) {
    ClassBuilder::new("ZendTestNS\\Foo")
        .method("method(): int", acc::PUBLIC, ret_zero)
        .register(vm);
    ClassBuilder::new("ZendTestNS\\UnlikelyCompileError")
        .method(
            "method(): ?ZendTestNS\\UnlikelyCompileError",
            acc::PUBLIC,
            none,
        )
        .register(vm);
    ClassBuilder::new("ZendTestNS\\NotUnlikelyCompileError")
        .method(
            "method(): ?ZendTestNS\\NotUnlikelyCompileError",
            acc::PUBLIC,
            none,
        )
        .register(vm);
    ClassBuilder::new("ZendTestNS\\Bar")
        .flags(class_flags::INTERFACE | class_flags::ABSTRACT)
        .register(vm);
    ClassBuilder::new("ZendTestNS2\\Foo")
        .prop(
            "foo",
            acc::PUBLIC,
            Some("ZendTestNS2\\ZendSubNS\\Foo"),
            None,
        )
        .prop(
            "intersectionProp",
            acc::PUBLIC,
            Some("ZendTestNS2\\ZendSubNS\\Foo&ZendTestNS\\Bar"),
            None,
        )
        .prop(
            "unionProp",
            acc::PUBLIC,
            Some("ZendTestNS2\\ZendSubNS\\Foo|ZendTestNS\\Bar"),
            None,
        )
        .prop("fooAlias", acc::PUBLIC, Some("ZendTestNS\\Foo"), None)
        .prop(
            "unlProp",
            acc::PUBLIC,
            Some("ZendTestNS\\UnlikelyCompileError"),
            None,
        )
        .prop(
            "notUnlProp",
            acc::PUBLIC,
            Some("ZendTestNS\\NotUnlikelyCompileError"),
            None,
        )
        .method("method(): void", acc::PUBLIC, none)
        .register(vm);
    ClassBuilder::new("ZendTestNS2\\ZendSubNS\\Foo")
        .method("method(): void", acc::PUBLIC, none)
        .register(vm);
    // Exact (compiler-resolved) property types.
    let tys: Vec<(&str, Option<TypeDecl>)> = [
        ("foo", "ZendTestNS2\\ZendSubNS\\Foo"),
        (
            "intersectionProp",
            "ZendTestNS2\\ZendSubNS\\Foo&ZendTestNS\\Bar",
        ),
        ("unionProp", "ZendTestNS2\\ZendSubNS\\Foo|ZendTestNS\\Bar"),
        ("fooAlias", "ZendTestNS\\Foo"),
        ("unlProp", "ZendTestNS\\UnlikelyCompileError"),
        ("notUnlProp", "ZendTestNS\\NotUnlikelyCompileError"),
    ]
    .into_iter()
    .map(|(n, t)| (n, type_of(vm, t)))
    .collect();
    if let Some(cls) = vm.find_class(b"ZendTestNS2\\Foo") {
        rebuild_class(vm, &cls, |c, _| {
            for (n, t) in tys {
                edit_prop(c, n, |p| p.ty = t);
            }
        });
    }
}

fn register_enums(vm: &mut Vm) {
    ClassBuilder::new("ZendTestUnitEnum")
        .unit_enum()
        .case("Foo", None)
        .case("Bar", None)
        .register(vm);
    ClassBuilder::new("ZendTestStringEnum")
        .backed_enum("string")
        .case("Foo", Some(s("Test1")))
        .case("Bar", Some(s("Test2")))
        .case("Baz", Some(s("Test2\\a")))
        .case("FortyTwo", Some(s("42")))
        .register(vm);
    ClassBuilder::new("ZendTestIntEnum")
        .backed_enum("int")
        .case("Foo", Some(Value::Int(1)))
        .case("Bar", Some(Value::Int(3)))
        .case("Baz", Some(Value::Int(-1)))
        .register(vm);
    ClassBuilder::new("ZendTestEnumWithInterface")
        .unit_enum()
        .implements(vm, &["_ZendTestInterface"])
        .case("Foo", None)
        .case("Bar", None)
        .register(vm);
}

#[allow(clippy::too_many_lines)]
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let vm = m.vm();
    let iface = ClassBuilder::new("_ZendTestInterface")
        .flags(class_flags::INTERFACE | class_flags::ABSTRACT)
        .constant("DUMMY", Value::Int(0))
        .register(vm);
    rebuild_class(vm, &iface, |c, _| {
        if let Some(&i) = c.const_map.get(&ZStr::from("DUMMY")) {
            let mut nc: ClassConst = (*c.consts[i]).clone();
            nc.doc_comment = Some(ZStr::from(
                "/**\n         * \"Lorem ipsum\"\n         * @see https://www.php.net\n         * @since 8.2\n         */",
            ));
            c.consts[i] = Rc::new(nc);
        }
    });
    register_test_class(vm);
    register_internal_attributes(m);
    let vm = m.vm();
    ClassBuilder::new("ZendTestGenStubFlagCompatibilityTest")
        .flags(class_flags::FINAL | class_flags::NOT_SERIALIZABLE)
        .register(vm);
    // Attribute classes first: other declarations reference them.
    register_attributes(vm);
    register_attribute_users(vm);
    register_traits(vm);
    ClassBuilder::new("ZendTestForbidDynamicCall")
        .flags(class_flags::FINAL)
        .method("call(): void", acc::PUBLIC, forbid_dynamic_call)
        .method(
            "callStatic(): void",
            acc::PUBLIC | acc::STATIC,
            forbid_dynamic_call,
        )
        .register(vm);
    register_namespaced(vm);
    register_enums(vm);
    ClassBuilder::new("_ZendTestMagicCall")
        .method("__call(mixed ...$args): mixed", acc::PUBLIC, magic_call)
        .register(vm);
    ClassBuilder::new("_ZendTestMagicCallForward")
        .method(
            "__call(mixed ...$args): mixed",
            acc::PUBLIC,
            magic_call_forward,
        )
        .register(vm);
    let _ = value_name;
}
