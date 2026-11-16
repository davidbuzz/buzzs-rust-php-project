// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The PHP callback registry behind `php:function()`/`php:functionString()`
//! and `registerPHPFunctionNS()` (`ext/dom/xpath_callbacks.c`).
//!
//! The registry has one table for the `http://php.net/xsl` namespace
//! (filled by `registerPHPFunctions()`) and one per user namespace (filled
//! by `registerPHPFunctionNS()`). Each table is in one of three modes:
//! nothing registered, *all* functions allowed (called by name), or a *set*
//! of name → callable entries.
//!
//! Callables are opaque values of type `C` supplied by the embedder, which
//! also validates them (`zend_is_callable`); this module only implements
//! PHP's bookkeeping, name validation and error messages.

use crate::error::{XslError, XslResult};

/// `http://php.net/xsl`, reserved for `php:function`.
pub const PHP_NAMESPACE: &[u8] = b"http://php.net/xsl";

/// `php_dom_xpath_callback_ns::mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// `PHP_DOM_REG_FUNC_MODE_NONE`.
    #[default]
    None,
    /// `PHP_DOM_REG_FUNC_MODE_ALL`: any function, called by name.
    All,
    /// `PHP_DOM_REG_FUNC_MODE_SET`: only registered names.
    Set,
}

/// How callback names are validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameValidation {
    /// Non-empty, no NUL byte.
    Nulls,
    /// Additionally an XML `NCName`.
    NcName,
}

/// One namespace's table.
#[derive(Debug, Clone)]
pub struct NsCallbacks<C> {
    functions: Vec<(Vec<u8>, C)>,
    mode: Mode,
}

impl<C> Default for NsCallbacks<C> {
    fn default() -> Self {
        Self {
            functions: Vec::new(),
            mode: Mode::None,
        }
    }
}

impl<C> NsCallbacks<C> {
    fn update(&mut self, name: Vec<u8>, callable: C) {
        match self.functions.iter_mut().find(|(n, _)| *n == name) {
            Some(entry) => entry.1 = callable,
            None => self.functions.push((name, callable)),
        }
    }

    /// The registration mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Registered names, in registration order.
    pub fn names(&self) -> impl Iterator<Item = &[u8]> {
        self.functions.iter().map(|(n, _)| n.as_slice())
    }
}

/// The resolution of a callback name at call time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup<C> {
    /// Call the PHP function of that name (`MODE_ALL`).
    ByName,
    /// Call this registered callable.
    Callable(C),
}

/// The whole registry (`php_dom_xpath_callbacks`).
#[derive(Debug, Clone)]
pub struct CallbackRegistry<C> {
    php_ns: Option<NsCallbacks<C>>,
    namespaces: Option<Vec<(Vec<u8>, NsCallbacks<C>)>>,
}

impl<C> Default for CallbackRegistry<C> {
    fn default() -> Self {
        Self {
            php_ns: None,
            namespaces: None,
        }
    }
}

fn is_name_valid(name: &[u8], validation: NameValidation) -> bool {
    if name.is_empty() || name.contains(&0) {
        return false;
    }
    match validation {
        NameValidation::Nulls => true,
        NameValidation::NcName => crate::raw::is_ncname(name),
    }
}

/// The error for an array value that is not callable.
pub fn array_callable_error(reason: impl std::fmt::Display) -> XslError {
    XslError::arg_type(
        "registerPHPFunctions",
        1,
        "functions",
        format!("must be an array with valid callbacks as values, {reason}"),
    )
}

/// Validates a `registerPHPFunctions("name")` name (before the callable
/// is resolved).
pub fn check_function_name(name: &[u8]) -> XslResult<()> {
    if is_name_valid(name, NameValidation::Nulls) {
        Ok(())
    } else {
        Err(XslError::arg_value(
            "registerPHPFunctions",
            1,
            "functions",
            "must be a valid callback name",
        ))
    }
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

impl<C: Clone> CallbackRegistry<C> {
    fn php_ns_mut(&mut self) -> &mut NsCallbacks<C> {
        self.php_ns.get_or_insert_with(NsCallbacks::default)
    }

    fn ns_mut(&mut self, ns: &[u8]) -> &mut NsCallbacks<C> {
        let list = self.namespaces.get_or_insert_with(Vec::new);
        let idx = if let Some(i) = list.iter().position(|(n, _)| n == ns) {
            i
        } else {
            list.push((ns.to_vec(), NsCallbacks::default()));
            list.len() - 1
        };
        &mut list[idx].1
    }

    /// The `php:` namespace table, if it was ever touched.
    pub fn php_namespace(&self) -> Option<&NsCallbacks<C>> {
        self.php_ns.as_ref()
    }

    /// `registerPHPFunctions()` / `registerPHPFunctions(null)`: allow every
    /// function.
    pub fn register_all(&mut self) {
        self.php_ns_mut().mode = Mode::All;
    }

    /// `registerPHPFunctions("name")`. `resolve` is `zend_is_callable`; its
    /// error is the text after `"must be a callable, "`.
    pub fn register_name(
        &mut self,
        name: &[u8],
        resolve: impl FnOnce(&[u8]) -> Result<C, String>,
    ) -> XslResult<()> {
        check_function_name(name)?;
        let callable = resolve(name).map_err(|e| {
            XslError::arg_type(
                "registerPHPFunctions",
                1,
                "functions",
                format!("must be a callable, {e}"),
            )
        })?;
        let ns = self.php_ns_mut();
        ns.update(name.to_vec(), callable);
        ns.mode = Mode::Set;
        Ok(())
    }

    /// `registerPHPFunctions(array)`. For each `(key, value)` entry, in
    /// order: `resolve(value)` validates the value as a callable (its error
    /// is the text after `"must be an array with valid callbacks as values, "`);
    /// without a string key, `to_name(value)` converts the value to the
    /// callback name (`zval_try_get_string`, which may throw). Entries
    /// registered before a failure stay registered; the mode only becomes
    /// *set* when every entry succeeded.
    pub fn register_array<V, E: From<XslError>>(
        &mut self,
        entries: impl IntoIterator<Item = (Option<Vec<u8>>, V)>,
        mut resolve: impl FnMut(&V) -> Result<C, String>,
        mut to_name: impl FnMut(&V) -> Result<Vec<u8>, E>,
    ) -> Result<(), E> {
        for (key, value) in entries {
            let callable = resolve(&value).map_err(array_callable_error)?;
            let name = match key {
                Some(k) => k,
                None => to_name(&value)?,
            };
            self.register_array_entry(name, callable)?;
        }
        self.finish_array();
        Ok(())
    }

    /// One validated entry of `registerPHPFunctions(array)` (see
    /// [`Self::register_array`]).
    pub fn register_array_entry(&mut self, name: Vec<u8>, callable: C) -> XslResult<()> {
        if !is_name_valid(&name, NameValidation::Nulls) {
            return Err(XslError::arg_value(
                "registerPHPFunctions",
                1,
                "functions",
                "must be an array containing valid callback names",
            ));
        }
        self.php_ns_mut().update(name, callable);
        Ok(())
    }

    /// Completes a successful `registerPHPFunctions(array)`.
    pub fn finish_array(&mut self) {
        self.php_ns_mut().mode = Mode::Set;
    }

    /// `registerPHPFunctionNS(namespace, name, callable)` after the
    /// argument checks: validates the name as an `NCName`.
    pub fn register_ns(&mut self, namespace: &[u8], name: &[u8], callable: C) -> XslResult<()> {
        if !is_name_valid(name, NameValidation::NcName) {
            return Err(XslError::arg_value(
                "registerPHPFunctionNS",
                2,
                "name",
                "must be a valid callback name",
            ));
        }
        let ns = self.ns_mut(namespace);
        ns.update(name.to_vec(), callable);
        ns.mode = Mode::Set;
        Ok(())
    }

    /// Every `(namespace, name)` registered with `registerPHPFunctionNS()`
    /// (`php_dom_xpath_callbacks_delayed_lib_registration`).
    pub fn namespaced_functions(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut out = Vec::new();
        if let Some(list) = &self.namespaces {
            for (ns, table) in list {
                for name in table.names() {
                    out.push((ns.clone(), name.to_vec()));
                }
            }
        }
        out
    }

    /// Resolves a `php:function` name (`namespace == None`) or a function
    /// of a registered namespace (`php_dom_xpath_callback_dispatch`).
    pub fn lookup(&self, namespace: Option<&[u8]>, name: &[u8]) -> XslResult<Lookup<C>> {
        let table = match namespace {
            None => self.php_ns.as_ref(),
            Some(ns) => self
                .namespaces
                .as_ref()
                .and_then(|l| l.iter().find(|(n, _)| n == ns))
                .map(|(_, t)| t),
        };
        let Some(table) = table.filter(|t| t.mode != Mode::None) else {
            return Err(XslError::Error("No callbacks were registered".into()));
        };
        if table.mode == Mode::All {
            return Ok(Lookup::ByName);
        }
        table
            .functions
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, c)| Lookup::Callable(c.clone()))
            .ok_or_else(|| {
                XslError::Error(format!(
                    "No callback handler \"{}\" registered",
                    lossy(name)
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::unnecessary_wraps)]
    fn ok(name: &[u8]) -> Result<String, String> {
        Ok(String::from_utf8_lossy(name).into_owned())
    }

    #[test]
    fn nothing_registered() {
        let r: CallbackRegistry<String> = CallbackRegistry::default();
        assert_eq!(
            r.lookup(None, b"f").unwrap_err().message(),
            "No callbacks were registered"
        );
        assert_eq!(
            r.lookup(Some(b"urn:x"), b"f").unwrap_err().message(),
            "No callbacks were registered"
        );
    }

    #[test]
    fn all_and_set_modes() {
        let mut r: CallbackRegistry<String> = CallbackRegistry::default();
        r.register_all();
        assert_eq!(r.lookup(None, b"anything").unwrap(), Lookup::ByName);
        r.register_name(b"ucwords", ok).unwrap();
        assert_eq!(
            r.lookup(None, b"ucwords").unwrap(),
            Lookup::Callable("ucwords".into())
        );
        assert_eq!(
            r.lookup(None, b"strpos").unwrap_err().message(),
            "No callback handler \"strpos\" registered"
        );
        // registerPHPFunctions(null) switches back to "all".
        r.register_all();
        assert_eq!(r.lookup(None, b"strpos").unwrap(), Lookup::ByName);
    }

    #[test]
    fn register_name_errors() {
        let mut r: CallbackRegistry<String> = CallbackRegistry::default();
        assert_eq!(
            r.register_name(b"", ok).unwrap_err().message(),
            "XSLTProcessor::registerPHPFunctions(): Argument #1 ($functions) must be a valid callback name"
        );
        let e = r
            .register_name(b"nonexistent", |_| {
                Err("function \"nonexistent\" not found or invalid function name".into())
            })
            .unwrap_err();
        assert_eq!(
            e.message(),
            "XSLTProcessor::registerPHPFunctions(): Argument #1 ($functions) must be a callable, function \"nonexistent\" not found or invalid function name"
        );
        assert_eq!(e.class(), crate::ErrorClass::TypeError);
    }

    #[test]
    fn register_array_semantics() {
        let mut r: CallbackRegistry<String> = CallbackRegistry::default();
        // An empty array switches to "set" mode with no entries.
        r.register_array(
            Vec::<(Option<Vec<u8>>, String)>::new(),
            |v| Ok(v.clone()),
            |v: &String| Ok::<_, XslError>(v.clone().into_bytes()),
        )
        .unwrap();
        assert_eq!(
            r.lookup(None, b"ucwords").unwrap_err().message(),
            "No callback handler \"ucwords\" registered"
        );
        r.register_array(
            vec![
                (None, "strrev".to_string()),
                (Some(b"xyz".to_vec()), "dump".to_string()),
            ],
            |v| Ok(v.clone()),
            |v: &String| Ok::<_, XslError>(v.clone().into_bytes()),
        )
        .unwrap();
        assert_eq!(
            r.lookup(None, b"xyz").unwrap(),
            Lookup::Callable("dump".into())
        );
        assert_eq!(
            r.lookup(None, b"strrev").unwrap(),
            Lookup::Callable("strrev".into())
        );

        let e = r
            .register_array(
                vec![(Some(b"\0".to_vec()), "var_dump".to_string())],
                |v| Ok(v.clone()),
                |v: &String| Ok::<_, XslError>(v.clone().into_bytes()),
            )
            .unwrap_err();
        assert_eq!(
            e.message(),
            "XSLTProcessor::registerPHPFunctions(): Argument #1 ($functions) must be an array containing valid callback names"
        );
        let e = r
            .register_array(
                vec![(None, "nonexistent".to_string())],
                |_| Err("function \"nonexistent\" not found or invalid function name".into()),
                |v: &String| Ok::<_, XslError>(v.clone().into_bytes()),
            )
            .unwrap_err();
        assert_eq!(
            e.message(),
            "XSLTProcessor::registerPHPFunctions(): Argument #1 ($functions) must be an array with valid callbacks as values, function \"nonexistent\" not found or invalid function name"
        );
    }

    #[test]
    fn namespaces() {
        let mut r: CallbackRegistry<String> = CallbackRegistry::default();
        assert_eq!(
            r.register_ns(b"urn:foo", b"$$$", "x".into()).unwrap_err().message(),
            "XSLTProcessor::registerPHPFunctionNS(): Argument #2 ($name) must be a valid callback name"
        );
        assert!(r.register_ns(b"urn:foo", b"x:a", "x".into()).is_err());
        r.register_ns(b"urn:foo", b"test", "strrev".into()).unwrap();
        r.register_ns(b"urn:bar", b"test", "strtoupper".into())
            .unwrap();
        assert_eq!(
            r.lookup(Some(b"urn:bar"), b"test").unwrap(),
            Lookup::Callable("strtoupper".into())
        );
        assert_eq!(
            r.namespaced_functions(),
            vec![
                (b"urn:foo".to_vec(), b"test".to_vec()),
                (b"urn:bar".to_vec(), b"test".to_vec())
            ]
        );
        // The php: table is separate.
        assert!(r.lookup(None, b"test").is_err());
    }
}
