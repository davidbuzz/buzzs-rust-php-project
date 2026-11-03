//! `XSLTProcessor` (`ext/xsl/xsltprocessor.c`).
//!
//! Every method takes `&self` (state lives in `Cell`/`RefCell`s and no
//! borrow is held while the host runs PHP code), so that PHP code running
//! inside a callback may call back into the same processor — e.g.
//! `setParameter()`, or `importStylesheet()`, which then fails with "while
//! a transformation is in progress" like in PHP.

use std::cell::{Cell, RefCell};
use std::ffi::c_int;
use std::ptr::NonNull;
use std::rc::Rc;

use crate::callbacks::{self, CallbackRegistry, Lookup, PHP_NAMESPACE};
use crate::document::{ResultDocument, XmlDocument};
use crate::error::{XslError, XslResult};
use crate::host::{CallbackArg, CallbackContext, CallbackReturn, ReturnedNode, XslHost};
use crate::params::{parameter_key, ParamKey, Parameters};
use crate::raw::{
    self, CallKind, DispatchRef, Dispatcher, ProfileFile, Stylesheet, Transform, ValueKind,
    XPathCall,
};

/// `XSL_SECPREF_NONE`.
pub const SECPREF_NONE: i64 = 0;
/// `XSL_SECPREF_READ_FILE`.
pub const SECPREF_READ_FILE: i64 = 2;
/// `XSL_SECPREF_WRITE_FILE`.
pub const SECPREF_WRITE_FILE: i64 = 4;
/// `XSL_SECPREF_CREATE_DIRECTORY`.
pub const SECPREF_CREATE_DIRECTORY: i64 = 8;
/// `XSL_SECPREF_READ_NETWORK`.
pub const SECPREF_READ_NETWORK: i64 = 16;
/// `XSL_SECPREF_WRITE_NETWORK`.
pub const SECPREF_WRITE_NETWORK: i64 = 32;
/// `XSL_SECPREF_DEFAULT`: all write access disabled.
pub const SECPREF_DEFAULT: i64 =
    SECPREF_WRITE_NETWORK | SECPREF_CREATE_DIRECTORY | SECPREF_WRITE_FILE;

/// The declared properties of `XSLTProcessor`, in declaration order.
///
/// `doXInclude`/`cloneDocument` are plain `bool` properties that may be
/// unset (`None`, read as false); `maxTemplateDepth`/`maxTemplateVars`
/// are validated `int`s that cannot be unset or referenced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Properties {
    /// `$doXInclude` (default false).
    pub do_xinclude: Option<bool>,
    /// `$cloneDocument` (default false).
    pub clone_document: Option<bool>,
    /// `$maxTemplateDepth` (default `xsltMaxDepth`).
    pub max_template_depth: i64,
    /// `$maxTemplateVars` (default `xsltMaxVars`).
    pub max_template_vars: i64,
}

impl Default for Properties {
    fn default() -> Self {
        raw::ensure_init();
        Self {
            do_xinclude: Some(false),
            clone_document: Some(false),
            max_template_depth: raw::version::max_depth(),
            max_template_vars: raw::version::max_vars(),
        }
    }
}

/// The property names, in declaration order.
pub const PROPERTY_NAMES: [&str; 4] = [
    "doXInclude",
    "cloneDocument",
    "maxTemplateDepth",
    "maxTemplateVars",
];

/// The result of `transformToXml()`: `string|null|false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlOutput {
    /// The serialized result.
    Str(Vec<u8>),
    /// The serialization succeeded but produced nothing.
    Null,
    /// The transformation failed.
    False,
}

/// An `XSLTProcessor` object's state. `C` is the embedder's callable type.
#[derive(Debug)]
pub struct XsltProcessor<C> {
    stylesheet: RefCell<Option<Rc<Stylesheet>>>,
    has_keys: Cell<bool>,
    params: RefCell<Parameters>,
    callbacks: RefCell<CallbackRegistry<C>>,
    security_prefs: Cell<i64>,
    profiling: RefCell<Option<Vec<u8>>>,
    transform_depth: Cell<u32>,
    props: Cell<Properties>,
}

impl<C: Clone> Default for XsltProcessor<C> {
    fn default() -> Self {
        Self::new()
    }
}

/// Decrements the transformation depth when the transformation ends.
struct DepthGuard<'a>(&'a Cell<u32>);

impl<'a> DepthGuard<'a> {
    fn enter(depth: &'a Cell<u32>) -> Self {
        depth.set(depth.get() + 1);
        Self(depth)
    }
}

impl Drop for DepthGuard<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `zend_long` → C `int` as an implicit C conversion does.
#[allow(clippy::cast_possible_truncation)]
fn to_c_int(v: i64) -> c_int {
    v as c_int
}

impl<C: Clone> XsltProcessor<C> {
    /// `new XSLTProcessor()` (`xsl_objects_new`).
    pub fn new() -> Self {
        raw::ensure_init();
        Self {
            stylesheet: RefCell::new(None),
            has_keys: Cell::new(false),
            params: RefCell::new(Parameters::default()),
            callbacks: RefCell::new(CallbackRegistry::default()),
            security_prefs: Cell::new(SECPREF_DEFAULT),
            profiling: RefCell::new(None),
            transform_depth: Cell::new(0),
            props: Cell::new(Properties::default()),
        }
    }

    // ----------------------------------------------------------------- props

    /// The current property values.
    pub fn properties(&self) -> Properties {
        self.props.get()
    }

    /// Writes `$doXInclude` (`None` = unset).
    pub fn set_do_xinclude(&self, value: Option<bool>) {
        let mut p = self.props.get();
        p.do_xinclude = value;
        self.props.set(p);
    }

    /// Writes `$cloneDocument` (`None` = unset).
    pub fn set_clone_document(&self, value: Option<bool>) {
        let mut p = self.props.get();
        p.clone_document = value;
        self.props.set(p);
    }

    /// Writes `$maxTemplateDepth` (after type coercion); negative values
    /// are rejected and the old value kept.
    pub fn set_max_template_depth(&self, value: i64) -> XslResult<()> {
        if value < 0 {
            return Err(XslError::negative_limit("maxTemplateDepth"));
        }
        let mut p = self.props.get();
        p.max_template_depth = value;
        self.props.set(p);
        Ok(())
    }

    /// Writes `$maxTemplateVars`; see [`Self::set_max_template_depth`].
    pub fn set_max_template_vars(&self, value: i64) -> XslResult<()> {
        if value < 0 {
            return Err(XslError::negative_limit("maxTemplateVars"));
        }
        let mut p = self.props.get();
        p.max_template_vars = value;
        self.props.set(p);
        Ok(())
    }

    /// `unset($proc->$name)` for a declared property. Unknown names are
    /// not this crate's business and succeed.
    pub fn unset_property(&self, name: &str) -> XslResult<()> {
        match name {
            "maxTemplateDepth" | "maxTemplateVars" => Err(XslError::cannot_unset(name)),
            "doXInclude" => {
                self.set_do_xinclude(None);
                Ok(())
            }
            "cloneDocument" => {
                self.set_clone_document(None);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// `get_property_ptr_ptr` / write-context reads (`$r =& $p->x`,
    /// `$p->x++`…): forbidden for the validated properties.
    pub fn check_indirect_modification(name: &str) -> XslResult<()> {
        match name {
            "maxTemplateDepth" | "maxTemplateVars" => Err(XslError::indirect_modification(name)),
            _ => Ok(()),
        }
    }

    /// Whether a transformation of this processor is running.
    pub fn is_transforming(&self) -> bool {
        self.transform_depth.get() > 0
    }

    // ------------------------------------------------------------ stylesheet

    /// `importStylesheet($stylesheet)` for the document owning the given
    /// node (the VM resolves `DOMNode`/`SimpleXMLElement` arguments to their
    /// document and performs the clone-handler checks).
    ///
    /// Returns false when libxslt rejects the stylesheet (its errors have
    /// been reported through `php_libxml::error`).
    pub fn import_stylesheet(&self, stylesheet: &dyn XmlDocument) -> XslResult<bool> {
        if self.is_transforming() {
            return Err(XslError::Error(
                "Cannot call XSLTProcessor::importStylesheet() while a transformation is in progress"
                    .into(),
            ));
        }
        // SAFETY: the XmlDocument contract keeps the document alive while
        // borrowed.
        let Some(sheet) =
            (unsafe { Stylesheet::compile(stylesheet.xml_doc(), stylesheet.is_modern()) })
        else {
            return Ok(false);
        };
        let clone_document = self.props.get().clone_document.unwrap_or(false);
        if clone_document || sheet.uses_keys() {
            self.has_keys.set(true);
        }
        *self.stylesheet.borrow_mut() = Some(Rc::new(sheet));
        Ok(true)
    }

    /// Whether a stylesheet has been imported.
    pub fn has_stylesheet(&self) -> bool {
        self.stylesheet.borrow().is_some()
    }

    // ------------------------------------------------------------ transforms

    /// `transformToDoc($document)`: the result tree, or `None` (PHP's
    /// `false`). The VM wraps it in a `DOMDocument` or the `$returnClass`
    /// (see [`XslError::incompatible_return_class`]).
    pub fn transform_to_doc(
        &self,
        document: &dyn XmlDocument,
        host: &mut dyn XslHost<C>,
    ) -> XslResult<Option<ResultDocument>> {
        Ok(self
            .apply_stylesheet("transformToDoc", document, host)?
            .map(|(doc, _)| doc))
    }

    /// `transformToUri($document, $uri)`: bytes written, or -1.
    pub fn transform_to_uri(
        &self,
        document: &dyn XmlDocument,
        uri: &[u8],
        host: &mut dyn XslHost<C>,
    ) -> XslResult<i64> {
        if uri.contains(&0) {
            return Err(XslError::null_bytes("transformToUri", 2, "uri"));
        }
        Ok(
            match self.apply_stylesheet("transformToUri", document, host)? {
                Some((doc, style)) => i64::from(raw::save_to_filename(uri, &doc, &style)),
                None => -1,
            },
        )
    }

    /// `transformToXml($document)`.
    pub fn transform_to_xml(
        &self,
        document: &dyn XmlDocument,
        host: &mut dyn XslHost<C>,
    ) -> XslResult<XmlOutput> {
        let Some((doc, style)) = self.apply_stylesheet("transformToXml", document, host)? else {
            return Ok(XmlOutput::False);
        };
        let (ret, text) = raw::save_to_string(&doc, &style);
        Ok(match (ret, text) {
            (r, _) if r < 0 => XmlOutput::False,
            (_, Some(t)) => XmlOutput::Str(t),
            (_, None) => XmlOutput::Null,
        })
    }

    /// `php_xsl_apply_stylesheet`.
    fn apply_stylesheet(
        &self,
        function: &str,
        document: &dyn XmlDocument,
        host: &mut dyn XslHost<C>,
    ) -> XslResult<Option<(ResultDocument, Rc<Stylesheet>)>> {
        let style = self.stylesheet.borrow().clone().ok_or_else(|| {
            XslError::Error(format!(
                "XSLTProcessor::{function}() can only be called after a stylesheet has been imported"
            ))
        })?;
        let depth = DepthGuard::enter(&self.transform_depth);

        let profile_path = self.profiling.borrow().clone();
        let profile = profile_path.and_then(|path| {
            if host.open_basedir_allows(&path) {
                ProfileFile::open(&path)
            } else {
                None
            }
        });

        let input = document.xml_doc();
        let copy = if self.has_keys.get() {
            // SAFETY: the XmlDocument contract keeps the input alive.
            unsafe { raw::copy_document(input) }
        } else {
            None
        };
        let main = copy
            .as_ref()
            .and_then(|d| NonNull::new(d.as_ptr()))
            .unwrap_or(input);

        let props = self.props.get();
        let params = self.params.borrow().clone();
        let functions = self.callbacks.borrow().namespaced_functions();
        let prefs = self.security_prefs.get();

        let session = Session {
            registry: &self.callbacks,
            host: RefCell::new(host),
            copy: copy.as_ref(),
            modern: document.is_modern(),
            keep: RefCell::new(Vec::new()),
        };
        let result = {
            let dispatch: DispatchRef<'_> = &session;
            // SAFETY: `main` (the input or our copy) outlives the transform.
            match unsafe { Transform::new(&style, main) } {
                None => None,
                Some(mut transform) => {
                    let mut abort = false;
                    for (name, value) in params.iter() {
                        if !transform.quote_param(name, value) {
                            let mut h = session.host.borrow_mut();
                            h.warning(&format!("Could not apply parameter \"{}\"", lossy(name)));
                            abort = h.exception_pending();
                            break;
                        }
                    }
                    if abort {
                        None
                    } else {
                        // SAFETY: `dispatch` outlives the transform (declared
                        // before it, dropped after it).
                        unsafe {
                            transform.configure(
                                &raw const dispatch,
                                props.do_xinclude.unwrap_or(false),
                                to_c_int(props.max_template_depth),
                                to_c_int(props.max_template_vars),
                            );
                        }
                        let secure = prefs == SECPREF_NONE || transform.set_security(prefs);
                        for (ns, name) in &functions {
                            transform.register_function(ns, name);
                        }
                        if secure {
                            transform.apply(profile.as_ref())
                        } else {
                            session.host.borrow_mut().warning(
                                "Can't set libxslt security properties, not doing transformation for security reasons",
                            );
                            None
                        }
                    }
                }
            }
        };
        drop(profile);
        let Session { host, keep, .. } = session;
        drop(keep);
        let host = host.into_inner();
        host.transformation_finished();
        drop(copy);
        drop(depth);
        Ok(result.map(|doc| (doc, style)))
    }

    // ------------------------------------------------------------ parameters

    /// `setParameter($namespace, $name, $value)` with a string name.
    pub fn set_parameter(
        &self,
        namespace: &[u8],
        name: &[u8],
        value: Option<&[u8]>,
    ) -> XslResult<bool> {
        const F: &str = "setParameter";
        if namespace.contains(&0) {
            return Err(XslError::null_bytes(F, 1, "namespace"));
        }
        if value.is_some_and(|v| v.contains(&0)) {
            return Err(XslError::null_bytes(F, 3, "value"));
        }
        let Some(value) = value else {
            return Err(XslError::arg_value(
                F,
                3,
                "value",
                "cannot be null when argument #2 ($name) is a string",
            ));
        };
        if name.contains(&0) {
            return Err(XslError::null_bytes(F, 2, "name"));
        }
        let key = parameter_key(F, 2, namespace, name)?;
        self.params.borrow_mut().insert(key, value.to_vec());
        Ok(true)
    }

    /// `setParameter($namespace, array $name, $value)`. `to_string` is
    /// `zval_try_get_string` on each value (may warn or throw); entries
    /// before a failure stay set.
    pub fn set_parameters<V, E: From<XslError>>(
        &self,
        namespace: &[u8],
        entries: impl IntoIterator<Item = (ParamKey, V)>,
        value: Option<&[u8]>,
        mut to_string: impl FnMut(&V) -> Result<Vec<u8>, E>,
    ) -> Result<bool, E> {
        const F: &str = "setParameter";
        if namespace.contains(&0) {
            return Err(XslError::null_bytes(F, 1, "namespace").into());
        }
        if value.is_some_and(|v| v.contains(&0)) {
            return Err(XslError::null_bytes(F, 3, "value").into());
        }
        if value.is_some() {
            return Err(XslError::arg_value(
                F,
                3,
                "value",
                "must be null when argument #2 ($name) is an array",
            )
            .into());
        }
        for (key, entry) in entries {
            let ParamKey::Str(key) = key else {
                return Err(
                    XslError::arg_type(F, 2, "name", "must contain only string keys").into(),
                );
            };
            if key.contains(&0) {
                return Err(XslError::arg_value(
                    F,
                    3,
                    "value",
                    "must not contain keys with any null bytes",
                )
                .into());
            }
            let table_key = parameter_key(F, 2, namespace, &key)?;
            let s = to_string(&entry)?;
            if s.contains(&0) {
                return Err(XslError::arg_value(
                    F,
                    3,
                    "value",
                    "must not contain values with any null bytes",
                )
                .into());
            }
            self.params.borrow_mut().insert(table_key, s);
        }
        Ok(true)
    }

    fn param_key(function: &'static str, namespace: &[u8], name: &[u8]) -> XslResult<Vec<u8>> {
        if namespace.contains(&0) {
            return Err(XslError::null_bytes(function, 1, "namespace"));
        }
        if name.contains(&0) {
            return Err(XslError::null_bytes(function, 2, "name"));
        }
        parameter_key(function, 2, namespace, name)
    }

    /// `getParameter($namespace, $name)`: `None` is PHP's `false`.
    pub fn get_parameter(&self, namespace: &[u8], name: &[u8]) -> XslResult<Option<Vec<u8>>> {
        let key = Self::param_key("getParameter", namespace, name)?;
        Ok(self.params.borrow().get(&key).map(<[u8]>::to_vec))
    }

    /// `removeParameter($namespace, $name)`.
    pub fn remove_parameter(&self, namespace: &[u8], name: &[u8]) -> XslResult<bool> {
        let key = Self::param_key("removeParameter", namespace, name)?;
        Ok(self.params.borrow_mut().remove(&key))
    }

    /// A snapshot of the parameter table (Clark-notation keys).
    pub fn parameters(&self) -> Parameters {
        self.params.borrow().clone()
    }

    // ------------------------------------------------------------- callbacks

    /// `registerPHPFunctions()` / `registerPHPFunctions(null)`.
    pub fn register_php_functions_all(&self) {
        self.callbacks.borrow_mut().register_all();
    }

    /// `registerPHPFunctions("name")`; `resolve` is `zend_is_callable`
    /// (its error is the text after "must be a callable, ").
    pub fn register_php_functions_name(
        &self,
        name: &[u8],
        resolve: impl FnOnce(&[u8]) -> Result<C, String>,
    ) -> XslResult<()> {
        callbacks::check_function_name(name)?;
        let callable = resolve(name).map_err(|e| {
            XslError::arg_type(
                "registerPHPFunctions",
                1,
                "functions",
                format!("must be a callable, {e}"),
            )
        })?;
        self.callbacks
            .borrow_mut()
            .register_name(name, |_| Ok(callable))
    }

    /// `registerPHPFunctions(array)`; see
    /// [`CallbackRegistry::register_array`] for the callbacks' roles.
    pub fn register_php_functions_array<V, E: From<XslError>>(
        &self,
        entries: impl IntoIterator<Item = (Option<Vec<u8>>, V)>,
        mut resolve: impl FnMut(&V) -> Result<C, String>,
        mut to_name: impl FnMut(&V) -> Result<Vec<u8>, E>,
    ) -> Result<(), E> {
        for (key, value) in entries {
            let callable = resolve(&value).map_err(callbacks::array_callable_error)?;
            let name = match key {
                Some(k) => k,
                None => to_name(&value)?,
            };
            self.callbacks
                .borrow_mut()
                .register_array_entry(name, callable)?;
        }
        self.callbacks.borrow_mut().finish_array();
        Ok(())
    }

    /// `registerPHPFunctionNS($namespaceURI, $name, $callable)`. `resolve`
    /// performs the `callable` parameter parsing (after the path checks of
    /// the first two arguments, as ZPP does).
    pub fn register_php_function_ns<E: From<XslError>>(
        &self,
        namespace: &[u8],
        name: &[u8],
        resolve: impl FnOnce() -> Result<C, E>,
    ) -> Result<(), E> {
        const F: &str = "registerPHPFunctionNS";
        if namespace.contains(&0) {
            return Err(XslError::null_bytes(F, 1, "namespaceURI").into());
        }
        if name.contains(&0) {
            return Err(XslError::null_bytes(F, 2, "name").into());
        }
        let callable = resolve()?;
        if namespace == PHP_NAMESPACE {
            return Err(XslError::arg_value(
                F,
                1,
                "namespaceURI",
                "must not be \"http://php.net/xsl\" because it is reserved by PHP",
            )
            .into());
        }
        self.callbacks
            .borrow_mut()
            .register_ns(namespace, name, callable)
            .map_err(E::from)
    }

    // ------------------------------------------------------- misc. settings

    /// `setProfiling($filename)`.
    pub fn set_profiling(&self, filename: Option<&[u8]>) -> XslResult<bool> {
        if filename.is_some_and(|f| f.contains(&0)) {
            return Err(XslError::null_bytes("setProfiling", 1, "filename"));
        }
        *self.profiling.borrow_mut() = filename.map(<[u8]>::to_vec);
        Ok(true)
    }

    /// The profiling file, if set.
    pub fn profiling(&self) -> Option<Vec<u8>> {
        self.profiling.borrow().clone()
    }

    /// `setSecurityPrefs($preferences)`: returns the old value.
    pub fn set_security_prefs(&self, preferences: i64) -> i64 {
        self.security_prefs.replace(preferences)
    }

    /// `getSecurityPrefs()`.
    pub fn get_security_prefs(&self) -> i64 {
        self.security_prefs.get()
    }

    /// `hasExsltSupport()`: EXSLT is always registered.
    pub fn has_exslt_support() -> bool {
        true
    }
}

/// The per-transformation state the extension functions reach through
/// `ctxt->_private`.
struct Session<'s, C> {
    registry: &'s RefCell<CallbackRegistry<C>>,
    host: RefCell<&'s mut dyn XslHost<C>>,
    copy: Option<&'s php_libxml::tree::Document>,
    modern: bool,
    /// Nodes returned by callbacks, alive until the transformation ends.
    keep: RefCell<Vec<ReturnedNode>>,
}

impl<C: Clone> Session<'_, C> {
    fn throw(&self, error: XslError) {
        self.host.borrow_mut().throw(error);
    }

    /// `php_dom_xpath_callback_fetch_args`.
    fn fetch_args<'c>(
        call: &mut XPathCall<'c>,
        count: usize,
        to_string: bool,
    ) -> Vec<CallbackArg<'c>> {
        let mut args = Vec::with_capacity(count);
        for _ in 0..count {
            let Some(value) = call.pop() else { break };
            let arg = match value.kind() {
                ValueKind::Str(s) => CallbackArg::Str(s),
                ValueKind::Bool(b) => CallbackArg::Bool(b),
                ValueKind::Number(f) => CallbackArg::Float(f),
                ValueKind::NodeSet if !to_string => CallbackArg::Nodes(call.node_args(&value)),
                ValueKind::NodeSet | ValueKind::Other => CallbackArg::Str(value.cast_to_string()),
            };
            args.push(arg);
        }
        args.reverse();
        args
    }

    /// `php_dom_xpath_callback_dispatch`: false on failure (the caller
    /// pushes the sentinel).
    fn dispatch_to(
        &self,
        call: &mut XPathCall<'_>,
        namespace: Option<&[u8]>,
        name: &[u8],
        args: Vec<CallbackArg<'_>>,
    ) -> bool {
        let lookup = self.registry.borrow().lookup(namespace, name);
        let ctx = CallbackContext {
            namespace,
            function: name,
            modern: self.modern,
            document_copy: self.copy,
        };
        let ret = match lookup {
            Err(e) => {
                drop(args);
                self.throw(e);
                return false;
            }
            Ok(Lookup::ByName) => {
                let mut host = self.host.borrow_mut();
                let ret = if host.exception_pending() {
                    CallbackReturn::Undef
                } else {
                    host.call_by_name(&ctx, args)
                };
                if host.exception_pending() {
                    return false;
                }
                ret
            }
            Ok(Lookup::Callable(callable)) => {
                let mut host = self.host.borrow_mut();
                if host.exception_pending() {
                    CallbackReturn::Undef
                } else {
                    host.call(&callable, &ctx, args)
                }
            }
        };
        match ret {
            CallbackReturn::Undef => {}
            CallbackReturn::Node(node) => {
                call.push_node(node.ptr());
                self.keep.borrow_mut().push(node);
            }
            CallbackReturn::Bool(b) => call.push_bool(b),
            CallbackReturn::NonNodeObject => {
                self.throw(XslError::TypeError(
                    "Only objects that are instances of DOM nodes can be converted to an XPath expression"
                        .into(),
                ));
                return false;
            }
            CallbackReturn::Str(s) => call.push_string(&s),
        }
        true
    }

    /// `php_dom_xpath_callbacks_call_php_ns`.
    fn call_php_ns(&self, call: &mut XPathCall<'_>, to_string: bool) {
        let n = call.nargs();
        if n == 0 {
            self.throw(XslError::Error(
                "Function name must be passed as the first argument".into(),
            ));
            call.push_string(b"");
            return;
        }
        let args = Self::fetch_args(call, n - 1, to_string);
        let name = call.pop().and_then(|v| v.stringval());
        let Some(name) = name else {
            drop(args);
            self.throw(XslError::TypeError("Handler name must be a string".into()));
            call.push_string(b"");
            return;
        };
        if !self.dispatch_to(call, None, &name, args) {
            call.push_string(b"");
        }
    }

    /// `php_dom_xpath_callbacks_call_custom_ns`.
    fn call_custom_ns(&self, call: &mut XPathCall<'_>) {
        let n = call.nargs();
        let args = Self::fetch_args(call, n, false);
        let namespace = call.function_uri().unwrap_or_default();
        let name = call.function_name();
        if !self.dispatch_to(call, Some(&namespace), &name, args) {
            call.push_string(b"");
        }
    }
}

impl<C: Clone> Dispatcher for Session<'_, C> {
    fn dispatch(&self, call: &mut XPathCall<'_>, kind: CallKind) {
        match kind {
            CallKind::Namespaced => self.call_custom_ns(call),
            CallKind::PhpString => self.call_php_ns(call, true),
            CallKind::PhpNodeset => self.call_php_ns(call, false),
        }
    }
}
