//! Compile-time attribute validation (`zend_compile_attributes`): internal
//! attribute targets, repetition and validators.

use super::{CResult, Compiler};
use crate::class::{cflags, AttributeInfo};
use php_runtime::ZStr;
use std::cell::RefCell;
use std::collections::HashMap;

/// The validator of an extension's internal attribute
/// (`zend_internal_attribute.validator`): receives the target
/// (`Attribute::TARGET_*`) and returns the compile error, if any.
pub type ExtAttributeValidator = fn(target: u32) -> Option<String>;

/// An extension attribute: its target flags and optional validator.
type ExtAttribute = (u32, Option<ExtAttributeValidator>);

thread_local! {
    /// Attribute classes registered by extensions
    /// (`zend_mark_internal_attribute`), by lowercase name.
    static EXT_ATTRIBUTES: RefCell<HashMap<Vec<u8>, ExtAttribute>> =
        RefCell::new(HashMap::new());
}

/// Registers an extension's internal attribute class.
pub(crate) fn register_internal_attribute(
    name: &str,
    flags: u32,
    validator: Option<ExtAttributeValidator>,
) {
    let key = name
        .trim_start_matches('\\')
        .to_ascii_lowercase()
        .into_bytes();
    EXT_ATTRIBUTES.with(|m| m.borrow_mut().insert(key, (flags, validator)));
}

/// The flags of an extension's internal attribute class.
pub(crate) fn ext_attribute_flags(lcname: &[u8]) -> Option<u32> {
    EXT_ATTRIBUTES.with(|m| m.borrow().get(lcname).map(|(f, _)| *f))
}

fn ext_attribute_validator(lcname: &[u8]) -> Option<ExtAttributeValidator> {
    EXT_ATTRIBUTES.with(|m| m.borrow().get(lcname).and_then(|(_, v)| *v))
}

/// `Attribute::TARGET_*`.
pub(crate) mod target {
    pub const CLASS: u32 = 1;
    pub const FUNCTION: u32 = 2;
    pub const METHOD: u32 = 4;
    pub const PROPERTY: u32 = 8;
    pub const CLASS_CONST: u32 = 16;
    pub const PARAMETER: u32 = 32;
    pub const CONSTANT: u32 = 64;
    pub const ALL: u32 = 127;
    pub const REPEATABLE: u32 = 128;
}

/// The class an attribute list belongs to (for class-target validators).
#[derive(Clone, Copy)]
pub(crate) struct AttrClass<'a> {
    pub name: &'a ZStr,
    pub flags: u32,
}

/// Validators of internal attributes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Validator {
    None,
    Attribute,
    AllowDynamicProperties,
    Deprecated,
    NoDiscard,
    /// Registered by an extension.
    Ext,
}

/// `zend_internal_attribute_get`: flags and validator of an internal
/// attribute class.
fn internal_attribute(lcname: &[u8]) -> Option<(u32, Validator)> {
    use target as t;
    Some(match lcname {
        b"attribute" => (t::CLASS, Validator::Attribute),
        b"returntypewillchange" => (t::METHOD, Validator::None),
        b"allowdynamicproperties" => (t::CLASS, Validator::AllowDynamicProperties),
        b"sensitiveparameter" => (t::PARAMETER, Validator::None),
        b"override" => (t::METHOD | t::PROPERTY | t::CLASS_CONST, Validator::None),
        b"deprecated" => (
            t::METHOD | t::FUNCTION | t::CLASS_CONST | t::CONSTANT | t::CLASS,
            Validator::Deprecated,
        ),
        b"nodiscard" => (t::METHOD | t::FUNCTION, Validator::NoDiscard),
        b"delayedtargetvalidation" => (t::ALL, Validator::None),
        _ => (ext_attribute_flags(lcname)?, Validator::Ext),
    })
}

/// `zend_compile_attributes`: drops the internal attributes of a promoted
/// constructor parameter that only apply to the other declaration (the
/// parameter or the property, `tgt`) and so are not added to it.
pub(crate) fn retain_promoted(attrs: &mut Vec<AttributeInfo>, tgt: u32, promoted: u32) {
    attrs.retain(|a| {
        internal_attribute(&lc(&a.name))
            .is_none_or(|(flags, _)| tgt & flags != 0 || promoted & flags == 0)
    });
}

/// `zend_get_attribute_target_names`.
pub(crate) fn target_names(flags: u32) -> String {
    const NAMES: [(u32, &str); 7] = [
        (target::CLASS, "class"),
        (target::FUNCTION, "function"),
        (target::METHOD, "method"),
        (target::PROPERTY, "property"),
        (target::CLASS_CONST, "class constant"),
        (target::PARAMETER, "parameter"),
        (target::CONSTANT, "constant"),
    ];
    NAMES
        .iter()
        .filter(|(f, _)| flags & f != 0)
        .map(|(_, n)| *n)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `zend_get_object_type_case(ce, false)`.
fn object_type(flags: u32) -> &'static str {
    if flags & cflags::TRAIT != 0 {
        "trait"
    } else if flags & cflags::INTERFACE != 0 {
        "interface"
    } else if flags & cflags::ENUM != 0 {
        "enum"
    } else {
        "class"
    }
}

fn lc(name: &ZStr) -> Vec<u8> {
    let n = name.as_bytes();
    n.strip_prefix(b"\\").unwrap_or(n).to_ascii_lowercase()
}

impl Compiler {
    /// Validates the internal attributes in `attrs` applied to `tgt`
    /// (`promoted`: the extra targets of a promoted constructor parameter).
    pub(crate) fn validate_attributes(
        &self,
        attrs: &[AttributeInfo],
        tgt: u32,
        promoted: u32,
        class: Option<AttrClass<'_>>,
        in_hook: bool,
    ) -> CResult<()> {
        let delayed = attrs
            .iter()
            .any(|a| lc(&a.name) == b"delayedtargetvalidation");
        for (i, a) in attrs.iter().enumerate() {
            let lcname = lc(&a.name);
            let Some((flags, validator)) = internal_attribute(&lcname) else {
                continue;
            };
            // Internal attributes that only apply to the promoted property.
            if tgt & flags == 0 && promoted & flags != 0 {
                continue;
            }
            let mut run_validator = true;
            if tgt & flags == 0 {
                if !delayed {
                    return self.err(
                        format!(
                            "Attribute \"{}\" cannot target {} (allowed targets: {})",
                            a.name.to_string_lossy(),
                            target_names(tgt),
                            target_names(flags)
                        ),
                        a.line,
                    );
                }
                run_validator = false;
            }
            if flags & target::REPEATABLE == 0 && attrs[..i].iter().any(|b| lc(&b.name) == lcname) {
                return self.err(
                    format!(
                        "Attribute \"{}\" must not be repeated",
                        a.name.to_string_lossy()
                    ),
                    a.line,
                );
            }
            if !run_validator || delayed {
                continue;
            }
            let msg = match (validator, class) {
                (Validator::Attribute, Some(c)) if tgt == target::CLASS => {
                    let what = if c.flags & cflags::TRAIT != 0 {
                        Some("trait")
                    } else if c.flags & cflags::INTERFACE != 0 {
                        Some("interface")
                    } else if c.flags & cflags::ENUM != 0 {
                        Some("enum")
                    } else if c.flags & cflags::EXPLICIT_ABSTRACT != 0 {
                        Some("abstract class")
                    } else {
                        None
                    };
                    what.map(|w| {
                        format!(
                            "Cannot apply #[\\Attribute] to {w} {}",
                            c.name.to_string_lossy()
                        )
                    })
                }
                (Validator::AllowDynamicProperties, Some(c)) if tgt == target::CLASS => {
                    let what = if c.flags & cflags::TRAIT != 0 {
                        Some("trait")
                    } else if c.flags & cflags::INTERFACE != 0 {
                        Some("interface")
                    } else if c.flags & cflags::READONLY != 0 {
                        Some("readonly class")
                    } else if c.flags & cflags::ENUM != 0 {
                        Some("enum")
                    } else {
                        None
                    };
                    what.map(|w| {
                        format!(
                            "Cannot apply #[\\AllowDynamicProperties] to {w} {}",
                            c.name.to_string_lossy()
                        )
                    })
                }
                (Validator::Deprecated, Some(c))
                    if tgt == target::CLASS && c.flags & cflags::TRAIT == 0 =>
                {
                    Some(format!(
                        "Cannot apply #[\\Deprecated] to {} {}",
                        object_type(c.flags),
                        c.name.to_string_lossy()
                    ))
                }
                (Validator::NoDiscard, _) if in_hook => {
                    Some("#[\\NoDiscard] is not supported for property hooks".to_string())
                }
                (Validator::Ext, _) => ext_attribute_validator(&lcname).and_then(|v| v(tgt)),
                _ => None,
            };
            if let Some(m) = msg {
                return self.err(m, a.line);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ext_attribute_flags, register_internal_attribute, retain_promoted, target, target_names,
    };
    use crate::class::AttributeInfo;
    use php_runtime::ZStr;

    fn attr(name: &str) -> AttributeInfo {
        AttributeInfo {
            name: ZStr::from(name),
            args: Vec::new(),
            line: 1,
            strict: false,
        }
    }

    #[test]
    fn promoted_parameters_keep_only_matching_internal_attributes() {
        register_internal_attribute("Test\\PropOnly", target::PROPERTY, None);
        assert_eq!(
            ext_attribute_flags(b"test\\proponly"),
            Some(target::PROPERTY)
        );
        let all = || {
            vec![
                attr("\\Test\\PropOnly"),
                attr("SensitiveParameter"),
                attr("UserAttribute"),
            ]
        };
        let names = |v: &[AttributeInfo]| -> Vec<String> {
            v.iter().map(|a| a.name.to_string_lossy()).collect()
        };
        let mut param = all();
        retain_promoted(&mut param, target::PARAMETER, target::PROPERTY);
        assert_eq!(names(&param), ["SensitiveParameter", "UserAttribute"]);
        let mut prop = all();
        retain_promoted(&mut prop, target::PROPERTY, target::PARAMETER);
        assert_eq!(names(&prop), ["\\Test\\PropOnly", "UserAttribute"]);
    }

    #[test]
    fn names_in_php_order() {
        assert_eq!(
            target_names(
                target::METHOD
                    | target::FUNCTION
                    | target::CLASS_CONST
                    | target::CONSTANT
                    | target::CLASS
            ),
            "class, function, method, class constant, constant"
        );
        assert_eq!(target_names(target::PARAMETER), "parameter");
    }
}
