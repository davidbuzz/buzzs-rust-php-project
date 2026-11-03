//! Engine-side attribute semantics: `#[\Deprecated]` messages
//! (`get_deprecation_suffix_from_attribute`, `zend_deprecated_*`).

use std::rc::Rc;

use php_runtime::{Obj, Value, ZStr};

use crate::class::{AttributeInfo, Class};
use crate::vm::{PendingCall, Vm, VmResult};

/// Evaluated attribute arguments: positional and named.
pub(crate) type AttrArgs = (Vec<Value>, Vec<(ZStr, Value)>);

impl Vm {
    /// Evaluates an attribute's arguments in `scope`.
    pub(crate) fn attribute_args(
        &mut self,
        info: &AttributeInfo,
        scope: Option<&Rc<Class>>,
    ) -> VmResult<AttrArgs> {
        let mut pos = Vec::new();
        let mut named = Vec::new();
        for (n, e) in &info.args {
            let v = self.eval_const_expr(e, scope)?;
            match n {
                Some(n) => named.push((n.clone(), v)),
                None => pos.push(v),
            }
        }
        Ok((pos, named))
    }

    /// `zend_get_attribute_object`: instantiates `cls` with the attribute's
    /// arguments.
    pub(crate) fn attribute_object(
        &mut self,
        info: &AttributeInfo,
        cls: &Rc<Class>,
        scope: Option<&Rc<Class>>,
    ) -> VmResult<Obj> {
        let (pos, named) = self.attribute_args(info, scope)?;
        let o = self.instantiate(cls)?;
        if let Some(m) = cls.magic.construct.clone() {
            let mut call = PendingCall {
                this: Some(o.clone()),
                scope: Some(m.scope()),
                called: Some(cls.clone()),
                ..PendingCall::new(m.func.clone())
            };
            call.args = pos.into_iter().map(Some).collect();
            for (k, v) in named {
                let Some(p) = m.func.params().iter().position(|p| p.name == k) else {
                    return Err(self.make_throw(
                        "Error",
                        &format!("Unknown named parameter ${}", k.to_string_lossy()),
                    ));
                };
                if call.args.len() <= p {
                    call.args.resize(p + 1, None);
                }
                call.args[p] = Some(v);
                call.has_named = true;
            }
            self.call_pending(call)?;
        }
        Ok(o)
    }

    /// The `" since X, message"` suffix of a `#[\Deprecated]` attribute in
    /// `attrs` (empty without one or without arguments). Errors raised
    /// while constructing the attribute (e.g. a `TypeError`) propagate.
    pub(crate) fn deprecation_suffix(
        &mut self,
        attrs: &[AttributeInfo],
        scope: Option<&Rc<Class>>,
    ) -> VmResult<Vec<u8>> {
        let Some(info) = attrs
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(b"Deprecated"))
        else {
            return Ok(Vec::new());
        };
        if info.args.is_empty() {
            return Ok(Vec::new());
        }
        let Some(cls) = self.find_class(b"Deprecated") else {
            return Ok(Vec::new());
        };
        let info = info.clone();
        let o = self.attribute_object(&info, &cls, scope)?;
        let message = self.read_prop_raw(&o, "message");
        let since = self.read_prop_raw(&o, "since");
        Ok(format_suffix(since.as_ref(), message.as_ref()))
    }

    fn read_prop_raw(&mut self, o: &Obj, name: &str) -> Option<ZStr> {
        match self.get_property(o, name) {
            Ok(Value::Str(s)) => Some(s),
            _ => None,
        }
    }
}

/// `" since %s, %s"` with empty parts left out.
pub(crate) fn format_suffix(since: Option<&ZStr>, message: Option<&ZStr>) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(s) = since.filter(|s| !s.is_empty()) {
        out.extend_from_slice(b" since ");
        out.extend_from_slice(s.as_bytes());
    }
    if let Some(m) = message.filter(|m| !m.is_empty()) {
        out.extend_from_slice(b", ");
        out.extend_from_slice(m.as_bytes());
    }
    out
}

impl Vm {
    /// `A::NAME` from code or `constant()`: the value plus the
    /// `#[\Deprecated]` notice (`zend_deprecated_class_constant`).
    pub(crate) fn fetch_class_constant(&mut self, cls: &Rc<Class>, name: &ZStr) -> VmResult<Value> {
        let v = self.class_constant(cls, name)?;
        let Some(c) = cls.find_const(name.as_bytes()).cloned() else {
            return Ok(v);
        };
        if !c
            .attributes
            .iter()
            .any(|a| a.name.eq_ignore_ascii_case(b"Deprecated"))
        {
            return Ok(v);
        }
        // Recursion protection (a message referring to the constant itself).
        let key = Rc::as_ptr(&c) as usize;
        if !self.const_deprecation_guard.insert(key) {
            return Ok(v);
        }
        let decl = self
            .find_class(c.class.as_bytes())
            .unwrap_or_else(|| cls.clone());
        let r = self.deprecation_suffix(&c.attributes, Some(&decl));
        self.const_deprecation_guard.remove(&key);
        let suffix = r?;
        let kind = if c.is_case { "Enum case" } else { "Constant" };
        let mut msg = format!(
            "{kind} {}::{} is deprecated",
            crate::object::display_name(&decl.name),
            name.to_string_lossy()
        )
        .into_bytes();
        msg.extend_from_slice(&suffix);
        let code = if decl.flags & crate::class::cflags::INTERNAL != 0 {
            crate::errors::level::DEPRECATED
        } else {
            crate::errors::level::USER_DEPRECATED
        };
        self.error_bytes(code, &msg)?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::format_suffix;
    use php_runtime::ZStr;

    #[test]
    fn suffix_forms() {
        let s = ZStr::from("8.4");
        let m = ZStr::from("use x");
        let e = ZStr::from("");
        assert_eq!(format_suffix(None, None), b"");
        assert_eq!(format_suffix(Some(&s), None), b" since 8.4");
        assert_eq!(format_suffix(None, Some(&m)), b", use x");
        assert_eq!(format_suffix(Some(&s), Some(&m)), b" since 8.4, use x");
        assert_eq!(format_suffix(Some(&e), Some(&e)), b"");
    }
}
