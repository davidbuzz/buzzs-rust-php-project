//! Registration of the stub tables ([`crate::stubs`]) with the VM: every
//! class with its constants, properties and methods, the enums and the
//! procedural functions, each bound to the handler registered under its
//! key (`"Class::method"` or the function name).

use std::collections::HashMap;

use php_intl::constants::{self, ConstValue};
use php_runtime::{Value, ZStr};
use php_vm::ext::{acc, class_flags, ClassBuilder, ClassHooks, ModuleBuilder, NativeFn};

use crate::core;

/// The kind of a stub declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClassKind {
    /// `class`
    Class,
    /// `interface`
    Interface,
    /// `enum`
    Enum,
}

/// A declared property.
#[derive(Debug)]
pub(crate) struct PropStub {
    pub name: &'static str,
    pub ty: &'static str,
    pub visibility: &'static str,
    pub readonly: bool,
}

/// A declared method.
#[derive(Debug)]
pub(crate) struct MethodStub {
    pub sig: &'static str,
    pub key: &'static str,
    pub visibility: &'static str,
    pub is_static: bool,
    pub is_final: bool,
    pub is_abstract: bool,
    /// `#[\Deprecated]` (not applied: the class builder cannot mark
    /// methods deprecated yet).
    #[allow(dead_code)]
    pub deprecated: Option<(&'static str, &'static str)>,
}

/// A declared class, interface or enum.
#[derive(Debug)]
pub(crate) struct ClassStub {
    pub name: &'static str,
    pub kind: ClassKind,
    pub parent: Option<&'static str>,
    pub interfaces: &'static [&'static str],
    pub is_final: bool,
    pub is_abstract: bool,
    pub not_serializable: bool,
    pub cases: &'static [&'static str],
    pub props: &'static [PropStub],
    pub methods: &'static [MethodStub],
}

/// A procedural function.
#[derive(Debug)]
pub(crate) struct FuncStub {
    pub sig: &'static str,
    pub key: &'static str,
    pub deprecated: Option<(&'static str, &'static str)>,
}

/// Per-class options supplied by the class modules.
#[derive(Default)]
pub(crate) struct ClassOpts {
    /// Object handlers (the `create` hook defaults to [`core::create`]).
    pub hooks: ClassHooks,
    /// Extra class flags (`NO_CLONE`, ...).
    pub flags: u32,
}

/// Converts a generated constant.
pub(crate) fn const_value(v: ConstValue) -> Value {
    match v {
        ConstValue::Int(i) => Value::Int(i),
        ConstValue::Float(f) => Value::Float(f),
        ConstValue::Str(s) => Value::Str(ZStr::from(s)),
        ConstValue::Null => Value::Null,
    }
}

fn visibility(v: &str) -> u32 {
    match v {
        "private" => acc::PRIVATE,
        "protected" => acc::PROTECTED,
        _ => acc::PUBLIC,
    }
}

/// Signature overrides: PHP checks these arities itself (with its own
/// messages), so the VM must accept any number of arguments.
fn sig_override(key: &str, sig: &'static str) -> &'static str {
    match key {
        "IntlGregorianCalendar::__construct" => {
            "__construct($timezoneOrYear = UNKNOWN, $localeOrMonth = UNKNOWN, $day = UNKNOWN, $hour = UNKNOWN, $minute = UNKNOWN, $second = UNKNOWN, ...$__extra)"
        }
        "intlgregcal_create_instance" => {
            "intlgregcal_create_instance($timezoneOrYear = UNKNOWN, $localeOrMonth = UNKNOWN, $day = UNKNOWN, $hour = UNKNOWN, $minute = UNKNOWN, $second = UNKNOWN, ...$__extra): ?IntlGregorianCalendar"
        }
        // The "more than 2 arguments" deprecation precedes the arity check.
        "intlcal_set" if sig.starts_with("set(") => {
            "set(int $year, int $month, int $dayOfMonth = UNKNOWN, int $hour = UNKNOWN, int $minute = UNKNOWN, int $second = UNKNOWN, ...$__extra): true"
        }
        // Reset the intl error before parsing their arguments
        // (`PHP_INTL_FUNCTION_WITH_ERROR_RESET`, `msgfmt_ctor`), so the
        // handlers parse them (`core::zpp_*`) and the VM must not.
        "Locale::acceptFromHttp" => match sig.split('(').next() {
            Some("acceptFromHttp") => {
                "acceptFromHttp(@unchecked string $header, @unchecked ?array $availableLocales = null): string|false"
            }
            Some("locale_accept_from_http") => {
                "locale_accept_from_http(@unchecked string $header, @unchecked ?array $availableLocales = null): string|false"
            }
            _ => sig,
        },
        "MessageFormatter::__construct" => {
            "__construct(@unchecked string $locale, @unchecked string $pattern)"
        }
        "MessageFormatter::create" => match sig.split('(').next() {
            Some("create") => {
                "create(@unchecked string $locale, @unchecked string $pattern): ?MessageFormatter"
            }
            Some("msgfmt_create") => {
                "msgfmt_create(@unchecked string $locale, @unchecked string $pattern): ?MessageFormatter"
            }
            _ => sig,
        },
        _ => sig,
    }
}

/// Registers everything.
pub(crate) fn register(
    m: &mut ModuleBuilder<'_>,
    handlers: &HashMap<&'static str, NativeFn>,
    opts: &mut dyn FnMut(&str) -> ClassOpts,
) {
    let deprecated = |class: Option<&str>, name: &str| {
        crate::stubs::DEPRECATED_CONSTANTS
            .iter()
            .find(|d| d.0 == class && d.1 == name)
            .map(|d| (d.2, d.3))
    };
    for (name, v) in constants::GLOBAL {
        match deprecated(None, name) {
            Some((since, msg)) => m.constant_deprecated(name, const_value(*v), since, msg),
            None => m.constant(name, const_value(*v)),
        };
    }
    for c in crate::stubs::CLASSES {
        if c.kind == ClassKind::Enum {
            let mut b = ClassBuilder::new(c.name).unit_enum();
            for case in c.cases {
                b = b.case(case, None);
            }
            b.register(m.vm());
            continue;
        }
        let o = opts(c.name);
        let mut b = ClassBuilder::new(c.name);
        if let Some(p) = c.parent {
            b = b.extends(m.vm(), p);
        }
        if !c.interfaces.is_empty() {
            b = b.implements(m.vm(), c.interfaces);
        }
        let mut fl = o.flags;
        if c.is_final {
            fl |= class_flags::FINAL;
        }
        if c.is_abstract {
            fl |= class_flags::ABSTRACT;
        }
        if c.kind == ClassKind::Interface {
            fl |= class_flags::INTERFACE;
        }
        if c.not_serializable {
            fl |= class_flags::NOT_SERIALIZABLE;
        }
        b = b.flags(fl);
        if let Some(cs) = constants::class_constants(c.name) {
            for (n, v) in cs {
                b = match deprecated(Some(c.name), n) {
                    Some((since, msg)) => b.deprecated_constant(n, const_value(*v), since, msg),
                    None => b.constant(n, const_value(*v)),
                };
            }
        }
        for p in c.props {
            let mut f = visibility(p.visibility);
            if p.readonly {
                f |= acc::READONLY;
            }
            b = b.prop(p.name, f, Some(p.ty), None);
        }
        for mt in c.methods {
            let mut f = visibility(mt.visibility);
            if mt.is_static {
                f |= acc::STATIC;
            }
            if mt.is_final {
                f |= acc::FINAL;
            }
            if mt.is_abstract || c.kind == ClassKind::Interface {
                b = b.abstract_method(mt.sig, f);
                continue;
            }
            let h = handlers
                .get(mt.key)
                .copied()
                .unwrap_or(core::not_implemented as NativeFn);
            b = b.method(sig_override(mt.key, mt.sig), f, h);
        }
        if c.kind == ClassKind::Class && c.name != "IntlException" {
            let mut hooks = o.hooks;
            if hooks.create.is_none() {
                hooks.create = Some(core::create);
            }
            b = b.hooks(hooks);
        }
        b.register(m.vm());
    }
    for f in crate::stubs::FUNCTIONS {
        let h = handlers
            .get(f.key)
            .copied()
            .unwrap_or(core::not_implemented as NativeFn);
        m.function_static(sig_override(f.key, f.sig), h);
        if let Some((since, msg)) = f.deprecated {
            m.deprecated(since, msg);
        }
    }
}
