//! Generic entry points for an embedding VM: method calls, property
//! access and iteration by name, driven by [`crate::classes::class_table`].
//!
//! Arguments arrive as [`DomValue`]s in declaration order (the VM resolves
//! named arguments; omitted optional arguments may simply be left out).
//! Scalar coercion follows PHP's coercive typing mode for internal
//! functions.

#![allow(clippy::many_single_char_names, clippy::too_many_lines)]

use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;

use crate::classes::{find_method, ParamInfo};
use crate::document::DocFlag;
use crate::element::AdjacentPosition;
use crate::error::{DomError, DomExceptionCode as C, DomResult, Level};
use crate::host::DomHost;
use crate::implementation::Implementation;
use crate::lists::{CollectionIter, IterKey, NamedNodeMap, NodeList, Offset};
use crate::node::C14nXPath;
use crate::object::Node;
use crate::tokenlist::TokenList;
use crate::tree::NodeOrString;
use crate::value::{ArrayKey, DomValue};
use crate::xpath::{CallbackSpec, Restrict, XPath};

/// A DOM extension object.
#[derive(Debug, Clone)]
pub enum DomObject {
    /// Any node class (including `DOMDocument` and `DOMNameSpaceNode`).
    Node(Node),
    /// `DOMNodeList`, `Dom\NodeList`, `Dom\HTMLCollection`.
    NodeList(NodeList),
    /// `DOMNamedNodeMap`, `Dom\NamedNodeMap`, `Dom\DtdNamedNodeMap`.
    NamedNodeMap(NamedNodeMap),
    /// `DOMImplementation`, `Dom\Implementation`.
    Implementation(Implementation),
    /// `DOMXPath`, `Dom\XPath`.
    XPath(XPath),
    /// `Dom\TokenList`.
    TokenList(TokenList),
}

impl DomObject {
    /// Wraps a value that holds an object.
    pub fn from_value(v: &DomValue) -> Option<DomObject> {
        Some(match v {
            DomValue::Node(n) => DomObject::Node(n.clone()),
            DomValue::NodeList(l) => DomObject::NodeList(l.clone()),
            DomValue::NamedNodeMap(m) => DomObject::NamedNodeMap(m.clone()),
            DomValue::Implementation(i) => DomObject::Implementation(i.clone()),
            DomValue::XPath(x) => DomObject::XPath(x.clone()),
            DomValue::TokenList(t) => DomObject::TokenList(t.clone()),
            _ => return None,
        })
    }

    /// The built-in class whose property handlers apply.
    pub fn builtin_class(&self) -> &'static str {
        match self {
            DomObject::Node(n) => n.builtin_class(),
            DomObject::NodeList(l) => l.class_name(),
            DomObject::NamedNodeMap(m) => m.class_name(),
            DomObject::Implementation(i) => i.class_name(),
            DomObject::XPath(x) => x.class_name(),
            DomObject::TokenList(_) => "Dom\\TokenList",
        }
    }
}

/// An iterator object returned by `getIterator()` / used by `foreach`.
#[derive(Debug, Clone)]
pub struct IterHandle(pub Rc<RefCell<DomIterator>>);

/// The state of a `foreach` over a DOM collection.
#[derive(Debug)]
pub enum DomIterator {
    /// Node collections.
    Collection(CollectionIter),
    /// Token lists (yields strings).
    Tokens(crate::tokenlist::TokenIter),
}

impl DomIterator {
    /// Whether the iterator is positioned on an element.
    pub fn valid(&mut self) -> bool {
        match self {
            DomIterator::Collection(c) => c.valid(),
            DomIterator::Tokens(t) => t.valid(),
        }
    }

    /// The current value.
    pub fn current(&self) -> DomValue {
        match self {
            DomIterator::Collection(c) => DomValue::opt_node(c.current()),
            DomIterator::Tokens(t) => DomValue::opt_str(t.current()),
        }
    }

    /// The current key.
    pub fn key(&mut self) -> DomValue {
        match self {
            DomIterator::Collection(c) => match c.key() {
                IterKey::Int(i) => DomValue::Int(i),
                IterKey::Str(s) => DomValue::Str(s),
                IterKey::Null => DomValue::Null,
            },
            DomIterator::Tokens(t) => DomValue::Int(t.key()),
        }
    }

    /// Restarts: a token list's iterator rewinds (`dom_token_list_it_rewind`);
    /// a node collection's has no rewind handler (`php_dom_iterator_funcs`),
    /// so this leaves it where it is.
    pub fn rewind(&mut self) {
        if let DomIterator::Tokens(t) = self {
            t.rewind();
        }
    }

    /// Whether this iterates a token list (`dom_token_list_it`: values read
    /// from the token set at every access) rather than a node collection
    /// (`php_dom_iterator`, which holds its current node).
    pub fn is_tokens(&self) -> bool {
        matches!(self, DomIterator::Tokens(_))
    }

    /// Advances.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> DomResult<()> {
        match self {
            DomIterator::Collection(c) => c.move_forward(),
            DomIterator::Tokens(t) => {
                t.move_forward();
                Ok(())
            }
        }
    }
}

/// Starts a `foreach` over an iterable DOM object.
pub fn iterate(obj: &DomObject) -> DomResult<IterHandle> {
    let it = match obj {
        DomObject::NodeList(l) => DomIterator::Collection(l.iter()?),
        DomObject::NamedNodeMap(m) => DomIterator::Collection(m.iter()?),
        DomObject::TokenList(t) => DomIterator::Tokens(t.iter()),
        _ => return Err(DomError::Error("Object is not iterable".into())),
    };
    Ok(IterHandle(Rc::new(RefCell::new(it))))
}

/// `count($obj)`.
pub fn count(obj: &DomObject) -> Option<i64> {
    match obj {
        DomObject::NodeList(l) => Some(l.length()),
        DomObject::NamedNodeMap(m) => Some(m.length()),
        DomObject::TokenList(t) => Some(t.length()),
        _ => None,
    }
}

/// `$obj[$offset]`.
pub fn offset_get(obj: &DomObject, offset: &Offset) -> DomResult<DomValue> {
    match obj {
        DomObject::NodeList(l) => Ok(DomValue::opt_node(l.offset_get(offset)?)),
        DomObject::NamedNodeMap(m) => Ok(DomValue::opt_node(m.offset_get(offset)?)),
        DomObject::TokenList(t) => t.offset_get(offset).map(DomValue::opt_str),
        _ => Err(DomError::Error("Cannot use object as array".into())),
    }
}

/// `isset($obj[$offset])`.
pub fn offset_exists(obj: &DomObject, offset: &Offset) -> DomResult<bool> {
    match obj {
        DomObject::NodeList(l) => l.offset_exists(offset),
        DomObject::NamedNodeMap(m) => m.offset_exists(offset),
        DomObject::TokenList(t) => t.offset_exists(offset),
        _ => Err(DomError::Error("Cannot use object as array".into())),
    }
}

// ---------------------------------------------------------------------------
// Scalar coercion
// ---------------------------------------------------------------------------

/// A float in PHP's `serialize_precision = -1` form (`%.*H` with -1):
/// the shortest round-tripping digits.
pub fn float_to_repr(f: f64) -> String {
    if !f.is_finite() || f == 0.0 {
        return float_to_string(f);
    }
    let s = format!("{f:e}");
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(char::is_ascii_digit).collect();
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if !(-4..17).contains(&exp) {
        out.push_str(&digits[..1]);
        out.push('.');
        out.push_str(if digits.len() > 1 { &digits[1..] } else { "0" });
        let _ = write!(out, "E{}{}", if exp < 0 { '-' } else { '+' }, exp.abs());
    } else if exp < 0 {
        out.push_str("0.");
        for _ in 0..(-exp - 1) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        let int_len = usize::try_from(exp).unwrap_or(0) + 1;
        if digits.len() <= int_len {
            out.push_str(&digits);
            for _ in digits.len()..int_len {
                out.push('0');
            }
        } else {
            out.push_str(&digits[..int_len]);
            out.push('.');
            out.push_str(&digits[int_len..]);
        }
    }
    out
}

/// PHP's `(string)` of a float (`precision=14`, `%.14G`).
pub fn float_to_string(f: f64) -> String {
    if f.is_nan() {
        return "NAN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "INF".into() } else { "-INF".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    let s = format!("{f:.13e}");
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if !(-5..15).contains(&exp) || exp >= 14 {
        out.push_str(&digits[..1]);
        out.push('.');
        if digits.len() > 1 {
            out.push_str(&digits[1..]);
        } else {
            out.push('0');
        }
        let _ = write!(out, "E{}{}", if exp < 0 { '-' } else { '+' }, exp.abs());
    } else if exp < 0 {
        out.push_str("0.");
        for _ in 0..(-exp - 1) {
            out.push('0');
        }
        out.push_str(digits);
    } else {
        let e = usize::try_from(exp).unwrap_or(0);
        if digits.len() <= e + 1 {
            out.push_str(digits);
            for _ in digits.len()..=e {
                out.push('0');
            }
        } else {
            out.push_str(&digits[..=e]);
            out.push('.');
            out.push_str(&digits[e + 1..]);
        }
    }
    out
}

struct Args<'a> {
    v: &'a [DomValue],
    params: &'static [ParamInfo],
    class: std::borrow::Cow<'a, str>,
    method: &'a str,
}

impl<'a> Args<'a> {
    fn new(class: &str, method: &'a str, v: &'a [DomValue]) -> Args<'a> {
        match find_method(class, method) {
            Some((c, m)) => Args {
                v,
                params: m.params,
                class: std::borrow::Cow::Borrowed(c.name),
                method: m.name,
            },
            None => Args {
                v,
                params: &[],
                class: std::borrow::Cow::Owned(class.to_string()),
                method,
            },
        }
    }

    /// `Class::method`, for error messages.
    fn func(&self) -> String {
        format!("{}::{}", self.class, self.method)
    }

    fn arg_no(i: usize) -> u32 {
        u32::try_from(i + 1).unwrap_or(u32::MAX)
    }

    fn pname(&self, i: usize) -> &str {
        self.params.get(i).map_or("", |p| p.name)
    }

    fn get(&self, i: usize) -> Option<&'a DomValue> {
        self.v.get(i)
    }

    fn type_err(&self, i: usize, expected: &str) -> DomError {
        let given = self
            .v
            .get(i)
            .map_or_else(|| "null".into(), DomValue::type_name);
        DomError::arg_type(
            Self::arg_no(i),
            format!("must be of type {expected}, {given} given"),
        )
    }

    fn null_deprecation(&self, i: usize, ty: &str) {
        crate::error::emit(
            Level::Deprecated,
            format!(
                "{}(): Passing null to parameter #{} (${}) of type {ty} is deprecated",
                self.func(),
                i + 1,
                self.pname(i)
            ),
        );
    }

    fn coerce_str(&self, i: usize, v: &DomValue) -> DomResult<Vec<u8>> {
        match v {
            DomValue::Str(s) => Ok(s.clone()),
            DomValue::Int(n) => Ok(n.to_string().into_bytes()),
            DomValue::Float(f) => Ok(float_to_string(*f).into_bytes()),
            DomValue::Bool(b) => Ok(if *b { b"1".to_vec() } else { Vec::new() }),
            DomValue::Null => {
                self.null_deprecation(i, "string");
                Ok(Vec::new())
            }
            _ => Err(self.type_err(i, "string")),
        }
    }

    /// Required `string`.
    fn str(&self, i: usize) -> DomResult<Vec<u8>> {
        match self.get(i) {
            Some(v) => self.coerce_str(i, v),
            None => Err(self.too_few()),
        }
    }

    /// Optional `string` with default.
    fn str_or(&self, i: usize, d: &[u8]) -> DomResult<Vec<u8>> {
        match self.get(i) {
            Some(v) => self.coerce_str(i, v),
            None => Ok(d.to_vec()),
        }
    }

    /// `?string` (missing → `None`).
    fn opt_str(&self, i: usize) -> DomResult<Option<Vec<u8>>> {
        match self.get(i) {
            None | Some(DomValue::Null) => Ok(None),
            Some(v) => self.coerce_str(i, v).map(Some),
        }
    }

    /// Path-like string (`p` zpp: no NUL bytes).
    fn path(&self, i: usize) -> DomResult<Vec<u8>> {
        let s = self.str(i)?;
        if s.contains(&0) {
            return Err(DomError::arg_value(
                Self::arg_no(i),
                "must not contain any null bytes",
            ));
        }
        Ok(s)
    }

    fn coerce_int(&self, i: usize, v: &DomValue) -> DomResult<i64> {
        match v {
            DomValue::Int(n) => Ok(*n),
            DomValue::Bool(b) => Ok(i64::from(*b)),
            DomValue::Float(f) => {
                if !f.is_finite()
                    || *f < -9.223_372_036_854_776e18
                    || *f >= 9.223_372_036_854_776e18
                {
                    return Err(self.type_err(i, "int"));
                }
                if f.fract() != 0.0 {
                    crate::error::emit(
                        Level::Deprecated,
                        format!(
                            "Implicit conversion from float {} to int loses precision",
                            float_to_string(*f)
                        ),
                    );
                }
                #[allow(clippy::cast_possible_truncation)]
                Ok(*f as i64)
            }
            DomValue::Str(s) => {
                let t = std::str::from_utf8(s).unwrap_or("").trim();
                if let Ok(n) = t.parse::<i64>() {
                    return Ok(n);
                }
                if let Ok(f) = t.parse::<f64>() {
                    #[allow(clippy::cast_possible_truncation)]
                    return Ok(f as i64);
                }
                Err(self.type_err(i, "int"))
            }
            DomValue::Null => {
                self.null_deprecation(i, "int");
                Ok(0)
            }
            _ => Err(self.type_err(i, "int")),
        }
    }

    fn int(&self, i: usize) -> DomResult<i64> {
        match self.get(i) {
            Some(v) => self.coerce_int(i, v),
            None => Err(self.too_few()),
        }
    }

    fn int_or(&self, i: usize, d: i64) -> DomResult<i64> {
        match self.get(i) {
            Some(v) => self.coerce_int(i, v),
            None => Ok(d),
        }
    }

    fn coerce_bool(&self, i: usize, v: &DomValue) -> DomResult<bool> {
        match v {
            DomValue::Bool(b) => Ok(*b),
            DomValue::Int(n) => Ok(*n != 0),
            DomValue::Float(f) => Ok(*f != 0.0),
            DomValue::Str(s) => Ok(!(s.is_empty() || s == b"0")),
            DomValue::Null => {
                self.null_deprecation(i, "bool");
                Ok(false)
            }
            _ => Err(self.type_err(i, "bool")),
        }
    }

    fn bool_or(&self, i: usize, d: bool) -> DomResult<bool> {
        match self.get(i) {
            Some(v) => self.coerce_bool(i, v),
            None => Ok(d),
        }
    }

    fn opt_bool(&self, i: usize) -> DomResult<Option<bool>> {
        match self.get(i) {
            None | Some(DomValue::Null) => Ok(None),
            Some(v) => self.coerce_bool(i, v).map(Some),
        }
    }

    fn node_type_name(&self, i: usize) -> &str {
        self.params.get(i).and_then(|p| p.ty).unwrap_or("DOMNode")
    }

    /// Required node of the declared class.
    fn node(&self, i: usize) -> DomResult<Node> {
        match self.get(i) {
            Some(DomValue::Node(n)) => {
                let want = self.node_type_name(i).trim_start_matches('?');
                if node_is_instance(n, want) {
                    Ok(n.clone())
                } else {
                    Err(self.type_err(i, want))
                }
            }
            Some(_) => Err(self.type_err(i, self.node_type_name(i).trim_start_matches('?'))),
            None => Err(self.too_few()),
        }
    }

    /// Nullable node.
    fn opt_node(&self, i: usize) -> DomResult<Option<Node>> {
        match self.get(i) {
            None | Some(DomValue::Null) => Ok(None),
            Some(_) => self.node(i).map(Some),
        }
    }

    fn rest_nodes(&self, from: usize) -> Vec<NodeOrString> {
        self.v[from.min(self.v.len())..]
            .iter()
            .map(|v| match v {
                DomValue::Node(n) => NodeOrString::Node(n.clone()),
                DomValue::Str(s) => NodeOrString::Str(s.clone()),
                other => NodeOrString::Invalid(other.type_name()),
            })
            .collect()
    }

    fn too_few(&self) -> DomError {
        DomError::Error(format!("Too few arguments to function {}()", self.func()))
    }
}

fn node_is_instance(n: &Node, class: &str) -> bool {
    let class = class.strip_prefix('\\').unwrap_or(class);
    // User classes derive from a built-in class; check the built-in chain.
    crate::classes::is_subclass(n.builtin_class(), class)
        || (class.eq_ignore_ascii_case("DOMNode") && !n.family_is_modern())
        || (class.eq_ignore_ascii_case("Dom\\Node") && n.family_is_modern())
}

fn opt_node_value(n: Option<Node>) -> DomValue {
    DomValue::opt_node(n)
}

fn bytes_value(b: Vec<u8>) -> DomValue {
    DomValue::Str(b)
}

fn false_on_none<T>(v: Option<T>, f: impl FnOnce(T) -> DomValue) -> DomValue {
    v.map_or(DomValue::Bool(false), f)
}

fn silent_false(r: DomResult<Node>) -> DomResult<DomValue> {
    match r {
        Ok(n) => Ok(DomValue::Node(n)),
        Err(DomError::Silent) => Ok(DomValue::Bool(false)),
        Err(e) => Err(e),
    }
}

fn silent_void(r: DomResult<()>) -> DomResult<DomValue> {
    match r {
        Ok(()) | Err(DomError::Silent) => Ok(DomValue::Null),
        Err(e) => Err(e),
    }
}

fn silent_bool(r: DomResult<()>) -> DomResult<DomValue> {
    match r {
        Ok(()) => Ok(DomValue::Bool(true)),
        Err(DomError::Silent) => Ok(DomValue::Bool(false)),
        Err(e) => Err(e),
    }
}

fn adjacent(v: Option<&DomValue>, legacy_arg: &[u8]) -> Result<AdjacentPosition, ()> {
    match v {
        Some(DomValue::Enum(_, case)) => match *case {
            "BeforeBegin" => Ok(AdjacentPosition::BeforeBegin),
            "AfterBegin" => Ok(AdjacentPosition::AfterBegin),
            "BeforeEnd" => Ok(AdjacentPosition::BeforeEnd),
            "AfterEnd" => Ok(AdjacentPosition::AfterEnd),
            _ => Err(()),
        },
        _ => AdjacentPosition::parse(legacy_arg).ok_or(()),
    }
}

#[allow(clippy::type_complexity)]
fn c14n_args(
    a: &Args<'_>,
    base: usize,
) -> DomResult<(bool, bool, Option<C14nXPath>, Option<Vec<Vec<u8>>>)> {
    let exclusive = a.bool_or(base, false)?;
    let with_comments = a.bool_or(base + 1, false)?;
    let xpath = match a.get(base + 2) {
        None | Some(DomValue::Null) => None,
        Some(DomValue::Array(items)) => {
            let mut x = C14nXPath::default();
            for (k, v) in items {
                match k {
                    ArrayKey::Str(k) if k == b"query" => {
                        x.query = Some(match v {
                            DomValue::Str(s) => Ok(s.clone()),
                            other => Err(other.type_name()),
                        });
                    }
                    ArrayKey::Str(k) if k == b"namespaces" => {
                        if let DomValue::Array(ns) = v {
                            for (pk, pv) in ns {
                                if let (ArrayKey::Str(p), DomValue::Str(u)) = (pk, pv) {
                                    x.namespaces.push((p.clone(), u.clone()));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some(x)
        }
        Some(_) => return Err(a.type_err(base + 2, "?array")),
    };
    let prefixes = match a.get(base + 3) {
        None | Some(DomValue::Null) => None,
        Some(DomValue::Array(items)) => Some(
            items
                .iter()
                .filter_map(|(_, v)| match v {
                    DomValue::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
        ),
        Some(_) => return Err(a.type_err(base + 3, "?array")),
    };
    Ok((exclusive, with_comments, xpath, prefixes))
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

/// Calls an instance method. `class` is the class the method was resolved
/// on (any DOM class or a user subclass's built-in ancestor).
#[allow(clippy::too_many_lines)]
pub fn call(
    obj: &DomObject,
    method: &str,
    args: &[DomValue],
    host: &dyn DomHost,
) -> DomResult<DomValue> {
    let class = obj.builtin_class();
    let a = Args::new(class, method, args);
    let m = method.to_ascii_lowercase();
    match obj {
        DomObject::NodeList(l) => match m.as_str() {
            "count" => Ok(DomValue::Int(l.length())),
            "item" => Ok(opt_node_value(l.item(a.int(0)?)?)),
            "getiterator" => Ok(DomValue::Iterator(iterate(obj)?)),
            "nameditem" => Ok(opt_node_value(l.named_item(&a.str(0)?))),
            _ => Err(undefined(class, method)),
        },
        DomObject::NamedNodeMap(map) => match m.as_str() {
            "count" => Ok(DomValue::Int(map.length())),
            "item" => Ok(opt_node_value(map.item(a.int(0)?)?)),
            "getnameditem" => Ok(opt_node_value(map.get_named_item(&a.str(0)?))),
            "getnameditemns" => {
                let ns = a.opt_str(0)?;
                Ok(opt_node_value(
                    map.get_named_item_ns(ns.as_deref(), &a.str(1)?),
                ))
            }
            "getiterator" => Ok(DomValue::Iterator(iterate(obj)?)),
            _ => Err(undefined(class, method)),
        },
        DomObject::Implementation(i) => match m.as_str() {
            "hasfeature" => Ok(DomValue::Bool(Implementation::has_feature(
                &a.str(0)?,
                &a.str(1)?,
            ))),
            "createdocumenttype" => {
                let name = a.str(0)?;
                let (p, s) = if i.modern {
                    (a.path(1)?, a.path(2)?)
                } else {
                    (a.str_or(1, b"")?, a.str_or(2, b"")?)
                };
                if i.modern && name.contains(&0) {
                    return Err(DomError::arg_value(1, "must not contain any null bytes"));
                }
                silent_false(i.create_document_type(&name, &p, &s))
            }
            "createdocument" => {
                let uri = a.opt_str(0)?;
                let name = if i.modern {
                    a.str(1)?
                } else {
                    a.str_or(1, b"")?
                };
                let dt = a.opt_node(2)?;
                Ok(DomValue::Node(i.create_document(
                    uri.as_deref(),
                    &name,
                    dt.as_ref(),
                )?))
            }
            "createhtmldocument" => {
                let t = a.opt_str(0)?;
                Ok(DomValue::Node(i.create_html_document(t.as_deref())?))
            }
            _ => Err(undefined(class, method)),
        },
        DomObject::XPath(x) => call_xpath(x, &a, &m, host),
        DomObject::TokenList(t) => crate::tokenlist::call(t, &a.func(), &m, args),
        DomObject::Node(n) => call_node(n, class, &a, &m, method, host),
    }
}

fn undefined(class: &str, method: &str) -> DomError {
    DomError::Error(format!("Call to undefined method {class}::{method}()"))
}

fn call_xpath(x: &XPath, a: &Args<'_>, m: &str, host: &dyn DomHost) -> DomResult<DomValue> {
    match m {
        "evaluate" | "query" => {
            let expr = a.str(0)?;
            let ctx = a.opt_node(1)?;
            let reg = match a.get(2) {
                None => None,
                Some(v) => Some(a.coerce_bool(2, v)?),
            };
            Ok(x.eval(&expr, ctx.as_ref(), reg, m == "evaluate", host)?
                .into_value())
        }
        "registernamespace" => Ok(DomValue::Bool(
            x.register_namespace(&a.str(0)?, &a.str(1)?)?,
        )),
        "quote" => Ok(DomValue::Str(XPath::quote(&a.path(0)?))),
        "registerphpfunctions" => {
            let r = match a.get(0) {
                None | Some(DomValue::Null) => Restrict::All,
                Some(DomValue::Str(s)) => Restrict::Name(s.clone(), host_resolve(host, s)),
                Some(DomValue::Array(items)) => Restrict::Array(
                    items
                        .iter()
                        .map(|(k, v)| CallbackSpec {
                            key: match k {
                                ArrayKey::Str(s) => Some(s.clone()),
                                ArrayKey::Int(_) => None,
                            },
                            value_string: match v {
                                DomValue::Str(s) => Some(s.clone()),
                                _ => None,
                            },
                            callable: match v {
                                DomValue::Callable(c) => Ok(c.clone()),
                                DomValue::Str(s) => host_resolve(host, s),
                                DomValue::Other(e) => Err(e.clone()),
                                other => {
                                    Err(format!("no array or string given ({})", other.type_name()))
                                }
                            },
                        })
                        .collect(),
                ),
                Some(_) => return Err(a.type_err(0, "array|string|null")),
            };
            x.register_php_functions(r)?;
            Ok(DomValue::Null)
        }
        "registerphpfunctionns" => {
            let ns = a.path(0)?;
            let name = a.path(1)?;
            let c = match a.get(2) {
                Some(DomValue::Callable(c)) => c.clone(),
                Some(DomValue::Str(s)) => host_resolve(host, s)
                    .map_err(|e| DomError::arg_type(3, format!("must be a valid callback, {e}")))?,
                Some(DomValue::Array(items)) if items.len() == 2 => {
                    let mut key = Vec::new();
                    for (_, v) in items {
                        if !key.is_empty() {
                            key.extend_from_slice(b"::");
                        }
                        match v {
                            DomValue::Str(s) => key.extend_from_slice(s),
                            DomValue::Node(n) => key.extend_from_slice(n.class_name().as_bytes()),
                            other => key.extend_from_slice(other.type_name().as_bytes()),
                        }
                    }
                    host_resolve(host, &key).map_err(|e| {
                        DomError::arg_type(3, format!("must be a valid callback, {e}"))
                    })?
                }
                Some(DomValue::Other(e)) => {
                    return Err(DomError::arg_type(
                        3,
                        format!("must be a valid callback, {e}"),
                    ))
                }
                _ => return Err(a.type_err(2, "callable")),
            };
            x.register_php_function_ns(&ns, &name, c)?;
            Ok(DomValue::Null)
        }
        _ => Err(undefined(x.class_name(), m)),
    }
}

fn host_resolve(host: &dyn DomHost, name: &[u8]) -> Result<crate::host::HostCallable, String> {
    host.resolve_callable(name)
}

/// Static methods (`createFromString`, `quote`, …).
pub fn call_static(
    class: &str,
    method: &str,
    args: &[DomValue],
    host: &dyn DomHost,
) -> DomResult<DomValue> {
    let _ = host;
    let a = Args::new(class, method, args);
    let c = crate::classes::find_class(class).map_or(class, |c| c.name);
    let m = method.to_ascii_lowercase();
    match (c, m.as_str()) {
        ("DOMXPath" | "Dom\\XPath", "quote") => Ok(DomValue::Str(XPath::quote(&a.path(0)?))),
        ("Dom\\XMLDocument", "createempty") => {
            let v = a.str_or(0, b"1.0")?;
            let e = a.str_or(1, b"UTF-8")?;
            if e.contains(&0) {
                return Err(DomError::arg_value(2, "must not contain any null bytes"));
            }
            Ok(DomValue::Node(crate::document::xml_document_create_empty(
                &v, &e,
            )?))
        }
        ("Dom\\XMLDocument", "createfromstring" | "createfromfile") => {
            let src = a.str(0)?;
            let opts = a.int_or(1, 0)?;
            let enc = a.opt_str(2)?;
            Ok(DomValue::Node(crate::document::xml_document_create_from(
                &src,
                opts,
                enc.as_deref(),
                m == "createfromfile",
            )?))
        }
        ("Dom\\HTMLDocument", "createempty") => {
            let e = a.str_or(0, b"UTF-8")?;
            Ok(DomValue::Node(crate::html::create_empty(&e)?))
        }
        ("Dom\\HTMLDocument", "createfromstring" | "createfromfile") => {
            let src = a.str(0)?;
            let opts = a.int_or(1, 0)?;
            let enc = a.opt_str(2)?;
            Ok(DomValue::Node(crate::html::create_from(
                &src,
                opts,
                enc.as_deref(),
                m == "createfromfile",
                host,
            )?))
        }
        _ => Err(DomError::Error(format!(
            "Call to undefined method {class}::{method}()"
        ))),
    }
}

/// Runs a constructor on an object created (unbound) by the VM.
pub fn construct(
    obj: &DomObject,
    args: &[DomValue],
    host: &dyn DomHost,
) -> DomResult<Option<DomObject>> {
    let _ = host;
    let class = obj.builtin_class();
    let a = Args::new(class, "__construct", args);
    match obj {
        DomObject::Node(n) => {
            match class {
                "DOMDocument" => n.construct_document(&a.str_or(0, b"1.0")?, &a.str_or(1, b"")?)?,
                "DOMElement" => {
                    let name = a.str(0)?;
                    let v = a.opt_str(1)?;
                    let ns = a.str_or(2, b"")?;
                    n.construct_element(&name, v.as_deref(), &ns)?;
                }
                "DOMAttr" => n.construct_attr(&a.str(0)?, &a.str_or(1, b"")?)?,
                "DOMText" => n.construct_text(a.opt_str(0)?.as_deref())?,
                "DOMComment" => n.construct_comment(a.opt_str(0)?.as_deref())?,
                "DOMCdataSection" => n.construct_cdata(&a.str(0)?)?,
                "DOMProcessingInstruction" => {
                    let name = a.str(0)?;
                    let v = a.opt_str(1)?;
                    n.construct_pi(&name, v.as_deref())?;
                }
                "DOMEntityReference" => n.construct_entity_reference(&a.str(0)?)?,
                "DOMDocumentFragment" => n.construct_fragment()?,
                c if c.starts_with("Dom\\") => {
                    return Err(DomError::Error(format!(
                        "Cannot directly construct {}, use document methods instead",
                        n.class_name()
                    )))
                }
                _ => {}
            }
            Ok(None)
        }
        DomObject::XPath(x) => {
            x.reconstruct(x.class_name())?;
            let doc = a.node(0)?;
            let reg = a.bool_or(1, true)?;
            Ok(Some(DomObject::XPath(XPath::new(&doc, reg)?)))
        }
        _ => Ok(None),
    }
}

/// Creates a new `DOMXPath`/`Dom\XPath` (the constructor of an object
/// the VM is instantiating).
pub fn new_xpath(document: &Node, register_node_ns: bool) -> DomResult<XPath> {
    XPath::new(document, register_node_ns)
}

#[allow(clippy::too_many_lines)]
fn call_node(
    n: &Node,
    class: &str,
    a: &Args<'_>,
    m: &str,
    method: &str,
    host: &dyn DomHost,
) -> DomResult<DomValue> {
    let modern = n.family_is_modern();
    let is_doc = crate::classes::is_subclass(class, "DOMDocument")
        || crate::classes::is_subclass(class, "Dom\\Document");
    let v = match m {
        // ---- DOMNode / Dom\Node ----
        "appendchild" => {
            let c = a.node(0)?;
            if modern {
                DomValue::Node(n.append_child(&c)?)
            } else {
                silent_false(n.append_child(&c))?
            }
        }
        "insertbefore" => {
            let c = a.node(0)?;
            let r = a.opt_node(1)?;
            if modern {
                DomValue::Node(n.insert_before(&c, r.as_ref())?)
            } else {
                silent_false(n.insert_before(&c, r.as_ref()))?
            }
        }
        "replacechild" => {
            let new = a.node(0)?;
            let old = a.node(1)?;
            if modern {
                DomValue::Node(n.replace_child(&new, &old)?)
            } else {
                silent_false(n.replace_child(&new, &old))?
            }
        }
        "removechild" => {
            let c = a.node(0)?;
            if modern {
                DomValue::Node(n.remove_child(&c)?)
            } else {
                silent_false(n.remove_child(&c))?
            }
        }
        "haschildnodes" => DomValue::Bool(n.has_child_nodes()?),
        "clonenode" => silent_false(n.clone_node(a.bool_or(0, false)?))?,
        "normalize" => {
            n.normalize()?;
            DomValue::Null
        }
        "issupported" => DomValue::Bool(Node::is_supported(&a.str(0)?, &a.str(1)?)),
        "hasattributes" => DomValue::Bool(n.has_attributes()?),
        "issamenode" => {
            let o = if modern {
                a.opt_node(0)?
            } else {
                Some(a.node(0)?)
            };
            DomValue::Bool(n.is_same_node(o.as_ref())?)
        }
        "isequalnode" => DomValue::Bool(n.is_equal_node(a.opt_node(0)?.as_ref())?),
        "lookupprefix" => {
            let u = if modern {
                a.opt_str(0)?
            } else {
                Some(a.str(0)?)
            };
            DomValue::opt_str(n.lookup_prefix(u.as_deref())?)
        }
        "lookupnamespaceuri" => {
            DomValue::opt_str(n.lookup_namespace_uri(a.opt_str(0)?.as_deref())?)
        }
        "isdefaultnamespace" => {
            let u = if modern {
                a.opt_str(0)?
            } else {
                Some(a.str(0)?)
            };
            DomValue::Bool(n.is_default_namespace(u.as_deref())?)
        }
        "getnodepath" => DomValue::opt_str(n.get_node_path()?),
        "getlineno" => DomValue::Int(n.get_line_no()?),
        "contains" => {
            let o = match a.get(0) {
                None | Some(DomValue::Null) => None,
                Some(DomValue::Node(o)) => Some(o.clone()),
                Some(other) => {
                    let expected = if modern {
                        "?Dom\\Node"
                    } else {
                        "DOMNode|DOMNameSpaceNode|null"
                    };
                    return Err(DomError::arg_type(
                        1,
                        format!("must be of type {expected}, {} given", other.type_name()),
                    ));
                }
            };
            DomValue::Bool(n.contains(o.as_ref())?)
        }
        "getrootnode" => DomValue::Node(n.get_root_node()?),
        "comparedocumentposition" => DomValue::Int(n.compare_document_position(&a.node(0)?)?),
        "c14n" => {
            let (e, w, x, p) = c14n_args(a, 0)?;
            false_on_none(n.c14n(e, w, x.as_ref(), p.as_deref())?, bytes_value)
        }
        "c14nfile" => {
            let f = a.path(0)?;
            let (e, w, x, p) = c14n_args(a, 1)?;
            false_on_none(
                n.c14n_file(&f, e, w, x.as_ref(), p.as_deref())?,
                DomValue::Int,
            )
        }
        "__sleep" => return Err(Node::sleep(&n.class_name())),
        "__wakeup" => return Err(Node::wakeup(&n.class_name())),
        // ---- ParentNode / ChildNode ----
        "append" => {
            n.append(&a.rest_nodes(0))?;
            DomValue::Null
        }
        "prepend" => {
            n.prepend(&a.rest_nodes(0))?;
            DomValue::Null
        }
        "replacechildren" => {
            n.replace_children(&a.rest_nodes(0))?;
            DomValue::Null
        }
        "after" => {
            n.after(&a.rest_nodes(0))?;
            DomValue::Null
        }
        "before" => {
            n.before(&a.rest_nodes(0))?;
            DomValue::Null
        }
        "replacewith" => {
            n.replace_with(&a.rest_nodes(0))?;
            DomValue::Null
        }
        "remove" => {
            n.remove()?;
            DomValue::Null
        }
        "queryselector" => DomValue::opt_node(crate::css::query_selector(n, &a.str(0)?)?),
        "queryselectorall" => DomValue::NodeList(crate::css::query_selector_all(n, &a.str(0)?)?),
        "matches" => DomValue::Bool(crate::css::matches(n, &a.str(0)?)?),
        "closest" => DomValue::opt_node(crate::css::closest(n, &a.str(0)?)?),
        // ---- CharacterData ----
        "appenddata" => {
            n.append_data(&a.str(0)?)?;
            if modern {
                DomValue::Null
            } else {
                DomValue::Bool(true)
            }
        }
        "substringdata" => match n.substring_data(a.int(0)?, a.int(1)?) {
            Ok(s) => DomValue::Str(s),
            Err(DomError::Silent) => DomValue::Bool(false),
            Err(e) => return Err(e),
        },
        "insertdata" => {
            let r = n.insert_data(a.int(0)?, &a.str(1)?);
            if modern {
                silent_void(r)?
            } else {
                silent_bool(r)?
            }
        }
        "deletedata" => {
            let r = n.delete_data(a.int(0)?, a.int(1)?);
            if modern {
                silent_void(r)?
            } else {
                silent_bool(r)?
            }
        }
        "replacedata" => {
            let r = n.replace_data(a.int(0)?, a.int(1)?, &a.str(2)?);
            if modern {
                silent_void(r)?
            } else {
                silent_bool(r)?
            }
        }
        // ---- Text ----
        "splittext" => silent_false(n.split_text(a.int(0)?))?,
        "iswhitespaceinelementcontent" | "iselementcontentwhitespace" => {
            DomValue::Bool(n.is_whitespace_in_element_content()?)
        }
        // ---- Attr ----
        "isid" => DomValue::Bool(n.is_id()?),
        "rename" => {
            let ns = a.opt_str(0)?;
            n.rename(ns.as_deref(), &a.str(1)?)?;
            DomValue::Null
        }
        // ---- DocumentFragment ----
        "appendxml" => match n.append_xml(&a.str(0)?) {
            Ok(b) => DomValue::Bool(b),
            Err(DomError::Silent) => DomValue::Bool(false),
            Err(e) => return Err(e),
        },
        _ => return call_node_2(n, class, a, m, method, host, modern, is_doc),
    };
    Ok(v)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::fn_params_excessive_bools
)]
fn call_node_2(
    n: &Node,
    class: &str,
    a: &Args<'_>,
    m: &str,
    method: &str,
    host: &dyn DomHost,
    modern: bool,
    is_doc: bool,
) -> DomResult<DomValue> {
    let v = match m {
        // ---- Element ----
        "getattribute" => {
            let r = n.get_attribute(&a.str(0)?)?;
            DomValue::opt_str(r)
        }
        "getattributenames" => DomValue::list(
            n.get_attribute_names()?
                .into_iter()
                .map(DomValue::Str)
                .collect(),
        ),
        "getattributens" => {
            let ns = a.opt_str(0)?;
            DomValue::opt_str(n.get_attribute_ns(ns.as_deref(), &a.str(1)?)?)
        }
        "getattributenode" => {
            let r = n.get_attribute_node(&a.str(0)?)?;
            if modern {
                DomValue::opt_node(r)
            } else {
                false_on_none(r, DomValue::Node)
            }
        }
        "getattributenodens" => {
            let ns = a.opt_str(0)?;
            DomValue::opt_node(n.get_attribute_node_ns(ns.as_deref(), &a.str(1)?)?)
        }
        "getelementsbytagname" => {
            let name = a.path(0)?;
            DomValue::NodeList(n.get_elements_by_tag_name(&name)?)
        }
        "getelementsbytagnamens" => {
            let ns = a.opt_str(0)?;
            if ns.as_ref().is_some_and(|s| s.contains(&0)) {
                return Err(DomError::arg_value(1, "must not contain any null bytes"));
            }
            let local = a.path(1)?;
            DomValue::NodeList(n.get_elements_by_tag_name_ns(ns.as_deref(), &local)?)
        }
        "getelementsbyclassname" => DomValue::NodeList(n.get_elements_by_class_name(&a.str(0)?)?),
        "hasattribute" => DomValue::Bool(n.has_attribute(&a.str(0)?)?),
        "hasattributens" => {
            let ns = a.opt_str(0)?;
            DomValue::Bool(n.has_attribute_ns(ns.as_deref(), &a.str(1)?)?)
        }
        "removeattribute" => {
            let r = n.remove_attribute(&a.str(0)?)?;
            if modern {
                DomValue::Null
            } else {
                DomValue::Bool(r)
            }
        }
        "removeattributens" => {
            let ns = a.opt_str(0)?;
            n.remove_attribute_ns(ns.as_deref(), &a.str(1)?)?;
            DomValue::Null
        }
        "removeattributenode" => {
            let at = a.node(0)?;
            if modern {
                DomValue::Node(n.remove_attribute_node(&at)?)
            } else {
                silent_false(n.remove_attribute_node(&at))?
            }
        }
        "setattribute" => {
            let r = n.set_attribute(&a.str(0)?, &a.str(1)?);
            match r {
                Err(DomError::Silent) => DomValue::Bool(false),
                other => other?,
            }
        }
        "setattributens" => {
            let ns = a.opt_str(0)?;
            silent_void(n.set_attribute_ns(ns.as_deref(), &a.str(1)?, &a.str(2)?))?
        }
        "setattributenode" | "setattributenodens" => {
            let at = a.node(0)?;
            let r = if m == "setattributenode" {
                n.set_attribute_node(&at)
            } else {
                n.set_attribute_node_ns(&at)
            };
            match r {
                Ok(o) => DomValue::opt_node(o),
                Err(DomError::Silent) => DomValue::Bool(false),
                Err(e) => return Err(e),
            }
        }
        "setidattribute" => silent_void(n.set_id_attribute(&a.str(0)?, a.bool_or(1, false)?))?,
        "setidattributens" => {
            let ns = a.opt_str(0)?;
            silent_void(n.set_id_attribute_ns(ns.as_deref(), &a.str(1)?, a.bool_or(2, false)?))?
        }
        "setidattributenode" => {
            silent_void(n.set_id_attribute_node(&a.node(0)?, a.bool_or(1, false)?))?
        }
        "toggleattribute" => DomValue::Bool(n.toggle_attribute(&a.str(0)?, a.opt_bool(1)?)?),
        "insertadjacentelement" => {
            let legacy = if modern { Vec::new() } else { a.str(0)? };
            let pos = adjacent(a.get(0), &legacy);
            let el = a.node(1)?;
            match n.insert_adjacent_element(pos, &el) {
                Ok(o) => DomValue::opt_node(o),
                Err(DomError::Silent) => DomValue::Null,
                Err(e) => return Err(e),
            }
        }
        "insertadjacenttext" => {
            let legacy = if modern { Vec::new() } else { a.str(0)? };
            let pos = adjacent(a.get(0), &legacy);
            silent_void(n.insert_adjacent_text(pos, &a.str(1)?))?
        }
        "insertadjacenthtml" => {
            let pos =
                adjacent(a.get(0), b"").map_err(|()| a.type_err(0, "Dom\\AdjacentPosition"))?;
            n.insert_adjacent_html(pos, &a.str(1)?)?;
            DomValue::Null
        }
        "getinscopenamespaces" => DomValue::list(
            n.get_in_scope_namespaces()?
                .into_iter()
                .map(DomValue::NamespaceInfo)
                .collect(),
        ),
        "getdescendantnamespaces" => DomValue::list(
            n.get_descendant_namespaces()?
                .into_iter()
                .map(DomValue::NamespaceInfo)
                .collect(),
        ),
        _ if is_doc => return call_document(n, class, a, m, method, host, modern),
        _ => return Err(undefined(class, method)),
    };
    Ok(v)
}

#[allow(clippy::too_many_lines)]
fn call_document(
    n: &Node,
    class: &str,
    a: &Args<'_>,
    m: &str,
    method: &str,
    host: &dyn DomHost,
    modern: bool,
) -> DomResult<DomValue> {
    let v = match m {
        "createelement" => {
            let name = a.str(0)?;
            let value = if modern {
                None
            } else {
                Some(a.str_or(1, b"")?)
            };
            let value = value.filter(|v| !v.is_empty() || a.get(1).is_some());
            silent_false(n.create_element(&name, value.as_deref()))?
        }
        "createelementns" => {
            let ns = a.opt_str(0)?;
            let name = a.str(1)?;
            let value = if modern { None } else { a.opt_str(2)? };
            silent_false(n.create_element_ns(ns.as_deref(), &name, value.as_deref()))?
        }
        "createdocumentfragment" => DomValue::Node(n.create_document_fragment()?),
        "createtextnode" => DomValue::Node(n.create_text_node(&a.str(0)?)?),
        "createcomment" => DomValue::Node(n.create_comment(&a.str(0)?)?),
        "createcdatasection" => silent_false(n.create_cdata_section(&a.str(0)?))?,
        "createprocessinginstruction" => {
            let t = a.str(0)?;
            let d = if modern {
                Some(a.str(1)?)
            } else {
                a.opt_str(1)?
            };
            silent_false(n.create_processing_instruction(&t, d.as_deref()))?
        }
        "createattribute" => {
            let name = a.path(0)?;
            silent_false(n.create_attribute(&name))?
        }
        "createattributens" => {
            let ns = a.opt_str(0)?;
            silent_false(n.create_attribute_ns(ns.as_deref(), &a.str(1)?))?
        }
        "createentityreference" => silent_false(n.create_entity_reference(&a.str(0)?))?,
        "getelementbyid" => DomValue::opt_node(n.get_element_by_id(&a.str(0)?)?),
        "importnode" | "importlegacynode" => {
            let src = match a.get(0) {
                Some(DomValue::Node(s)) => s.clone(),
                _ => a.node(0)?,
            };
            silent_false(n.import_node(&src, a.bool_or(1, false)?))?
        }
        "adoptnode" => silent_false(n.adopt_node(&a.node(0)?))?,
        "normalizedocument" => {
            n.normalize_document()?;
            DomValue::Null
        }
        "load" => DomValue::Bool(n.load(&a.str(0)?, a.int_or(1, 0)?)?),
        "loadxml" => DomValue::Bool(n.load_xml(&a.str(0)?, a.int_or(1, 0)?)?),
        "loadhtml" => DomValue::Bool(n.load_html(&a.str(0)?, a.int_or(1, 0)?, false)?),
        "loadhtmlfile" => DomValue::Bool(n.load_html(&a.str(0)?, a.int_or(1, 0)?, true)?),
        "save" => false_on_none(n.save(&a.str(0)?, a.int_or(1, 0)?)?, DomValue::Int),
        "savexml" => {
            let node = a.opt_node(0)?;
            false_on_none(n.save_xml(node.as_ref(), a.int_or(1, 0)?)?, DomValue::Str)
        }
        "savexmlfile" => false_on_none(
            crate::html::save_xml_file(n, &a.path(0)?, a.int_or(1, 0)?)?,
            DomValue::Int,
        ),
        "savehtml" => {
            let node = a.opt_node(0)?;
            if modern {
                DomValue::Str(crate::html::save_html(n, node.as_ref())?)
            } else {
                false_on_none(n.save_html(node.as_ref())?, DomValue::Str)
            }
        }
        "savehtmlfile" => {
            if modern {
                false_on_none(
                    crate::html::save_html_file(n, &a.path(0)?, host)?,
                    DomValue::Int,
                )
            } else {
                false_on_none(n.save_html_file(&a.path(0)?)?, DomValue::Int)
            }
        }
        "validate" => DomValue::Bool(n.validate()?),
        "schemavalidate" => DomValue::Bool(n.schema_validate(&a.str(0)?, a.int_or(1, 0)?, true)?),
        "schemavalidatesource" => {
            DomValue::Bool(n.schema_validate(&a.str(0)?, a.int_or(1, 0)?, false)?)
        }
        "relaxngvalidate" => DomValue::Bool(n.relaxng_validate(&a.str(0)?, true)?),
        "relaxngvalidatesource" => DomValue::Bool(n.relaxng_validate(&a.str(0)?, false)?),
        "xinclude" => {
            let r = n.xinclude(a.int_or(0, 0)?)?;
            false_on_none(r, DomValue::Int)
        }
        "registernodeclass" => {
            let base = class_arg(a, 0)?;
            let ext = match a.get(1) {
                None | Some(DomValue::Null) => None,
                Some(_) => Some(class_arg(a, 1)?),
            };
            n.register_node_class(&base, ext.as_deref(), host)?;
            if modern {
                DomValue::Null
            } else {
                DomValue::Bool(true)
            }
        }
        "debuggettemplatecount" => DomValue::Int(crate::html::template_count(n)),
        _ => return Err(undefined(class, method)),
    };
    Ok(v)
}

fn class_arg(a: &Args<'_>, i: usize) -> DomResult<String> {
    match a.get(i) {
        Some(DomValue::Str(s)) => Ok(String::from_utf8_lossy(s).into_owned()),
        _ => Err(a.type_err(i, "string")),
    }
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// The result of reading a property through the DOM handlers.
#[derive(Debug, Clone)]
pub enum PropRead {
    /// A value.
    Value(DomValue),
    /// The handler failed without an exception: the property reads as
    /// uninitialized (null, and it is omitted from `var_dump()`).
    Uninit,
    /// No DOM handler for this name: use the standard property table.
    NotHandled,
}

/// Whether `name` is handled by a DOM property handler for this object.
pub fn is_handled_property(obj: &DomObject, name: &str) -> bool {
    crate::classes::property_handlers(obj.builtin_class()).contains(&name)
}

/// Reads a property.
pub fn read_property(obj: &DomObject, name: &str) -> DomResult<PropRead> {
    if !is_handled_property(obj, name) {
        return Ok(PropRead::NotHandled);
    }
    match obj {
        DomObject::NodeList(l) => Ok(PropRead::Value(DomValue::Int(l.length()))),
        DomObject::NamedNodeMap(m) => Ok(PropRead::Value(DomValue::Int(m.length()))),
        DomObject::XPath(x) => Ok(PropRead::Value(match name {
            "document" => DomValue::Node(x.document()?),
            _ => DomValue::Bool(x.register_node_namespaces()),
        })),
        DomObject::TokenList(t) => Ok(PropRead::Value(match name {
            "length" => DomValue::Int(t.length()),
            _ => DomValue::Str(t.value()),
        })),
        DomObject::Implementation(_) => Ok(PropRead::NotHandled),
        DomObject::Node(n) => read_node_property(n, name),
    }
}

#[allow(clippy::too_many_lines)]
fn read_node_property(n: &Node, name: &str) -> DomResult<PropRead> {
    let builtin = n.builtin_class();
    let modern = n.family_is_modern();
    let is_doc =
        builtin == "DOMDocument" || builtin.starts_with("Dom\\") && builtin.ends_with("Document");
    let is_attr = builtin == "DOMAttr" || builtin == "Dom\\Attr";
    let is_entity = builtin == "DOMEntity" || builtin == "Dom\\Entity";
    let is_notation = builtin == "DOMNotation";
    let is_doctype = builtin == "DOMDocumentType" || builtin == "Dom\\DocumentType";
    let is_pi = builtin == "DOMProcessingInstruction" || builtin == "Dom\\ProcessingInstruction";
    let v = match name {
        "nodeName" => DomValue::Str(n.node_name()?),
        "nodeValue" => DomValue::opt_str(n.node_value()?),
        "nodeType" => DomValue::Int(n.node_type()?),
        "parentNode" => DomValue::opt_node(n.parent_node()?),
        "parentElement" => DomValue::opt_node(n.parent_element()?),
        "childNodes" => DomValue::NodeList(n.child_nodes()?),
        "firstChild" => DomValue::opt_node(n.first_child()?),
        "lastChild" => DomValue::opt_node(n.last_child()?),
        "previousSibling" => DomValue::opt_node(n.previous_sibling()?),
        "nextSibling" => DomValue::opt_node(n.next_sibling()?),
        "previousElementSibling" => DomValue::opt_node(n.previous_element_sibling()?),
        "nextElementSibling" => DomValue::opt_node(n.next_element_sibling()?),
        "attributes" => match n.attributes()? {
            Some(m) => DomValue::NamedNodeMap(m),
            None => DomValue::Null,
        },
        "isConnected" => DomValue::Bool(n.is_connected()?),
        "ownerDocument" => match n.owner_document() {
            Ok(d) => DomValue::opt_node(d),
            Err(DomError::Silent) => return Ok(PropRead::Uninit),
            Err(e) => return Err(e),
        },
        "namespaceURI" => DomValue::opt_str(n.namespace_uri()?),
        "prefix" => {
            if modern {
                DomValue::opt_str(n.prefix_modern()?)
            } else {
                DomValue::Str(n.prefix()?)
            }
        }
        "localName" => DomValue::opt_str(n.local_name()?),
        "baseURI" => {
            if modern {
                DomValue::Str(n.base_uri_modern()?)
            } else {
                DomValue::opt_str(n.base_uri()?)
            }
        }
        "textContent" => DomValue::opt_str(n.text_content()?),
        "firstElementChild" => DomValue::opt_node(n.first_element_child()?),
        "lastElementChild" => DomValue::opt_node(n.last_element_child()?),
        "childElementCount" => DomValue::Int(n.child_element_count()?),
        "children" => DomValue::NodeList(n.children_collection()?),
        // Element
        "tagName" => DomValue::Str(n.tag_name()?),
        "className" => DomValue::Str(n.class_name_attr()?),
        "id" => DomValue::Str(n.id()?),
        "schemaTypeInfo" => DomValue::Null,
        "classList" => DomValue::TokenList(n.class_list()?),
        "innerHTML" => DomValue::Str(crate::inner_outer::inner_html(n)?),
        "outerHTML" => DomValue::Str(crate::inner_outer::outer_html(n)?),
        "substitutedNodeValue" => DomValue::Str(n.substituted_node_value()?),
        // CharacterData
        "data" => DomValue::Str(n.data()?),
        "length" => DomValue::Int(n.length()?),
        "wholeText" => DomValue::Str(n.whole_text()?),
        "target" if is_pi => DomValue::Str(n.target()?),
        // Attr
        "name" if is_attr => DomValue::Str(n.attr_name()?),
        "specified" => DomValue::Bool(true),
        "value" if is_attr => DomValue::Str(n.attr_value()?),
        "ownerElement" => DomValue::opt_node(n.owner_element()?),
        // DocumentType
        "name" if is_doctype => DomValue::Str(n.doctype_name()?),
        "entities" => DomValue::NamedNodeMap(n.doctype_entities()?),
        "notations" => DomValue::NamedNodeMap(n.doctype_notations()?),
        "publicId" if is_doctype => DomValue::Str(n.doctype_public_id()?),
        "systemId" if is_doctype => DomValue::Str(n.doctype_system_id()?),
        "internalSubset" => DomValue::opt_str(n.doctype_internal_subset()?),
        // Entity / Notation
        "publicId" if is_entity => DomValue::opt_str(n.entity_public_id()?),
        "systemId" if is_entity => DomValue::opt_str(n.entity_system_id()?),
        "notationName" => DomValue::opt_str(n.entity_notation_name()?),
        "actualEncoding" if is_entity => {
            crate::error::deprecated("Property DOMEntity::$actualEncoding is deprecated");
            DomValue::Null
        }
        "encoding" if is_entity => {
            crate::error::deprecated("Property DOMEntity::$encoding is deprecated");
            DomValue::Null
        }
        "version" if is_entity => {
            crate::error::deprecated("Property DOMEntity::$version is deprecated");
            DomValue::Null
        }
        "publicId" if is_notation => DomValue::Str(n.notation_public_id()?),
        "systemId" if is_notation => DomValue::Str(n.notation_system_id()?),
        // Document
        "doctype" if is_doc => DomValue::opt_node(n.doctype()?),
        "implementation" if is_doc => DomValue::Implementation(n.implementation()),
        "documentElement" => DomValue::opt_node(n.document_element()?),
        "actualEncoding" => DomValue::opt_str(n.actual_encoding()?),
        "encoding" | "xmlEncoding" | "characterSet" | "charset" | "inputEncoding" => {
            let e = n.encoding()?;
            if modern {
                DomValue::Str(e.unwrap_or_default())
            } else {
                DomValue::opt_str(e)
            }
        }
        "standalone" | "xmlStandalone" => DomValue::Bool(n.standalone()?),
        "version" | "xmlVersion" => {
            let v = n.version()?;
            if modern {
                DomValue::Str(v.unwrap_or_default())
            } else {
                DomValue::opt_str(v)
            }
        }
        "strictErrorChecking" => DomValue::Bool(n.doc_flag(DocFlag::StrictErrorChecking)),
        "documentURI" | "URL" => DomValue::opt_str(n.document_uri()?),
        "config" => {
            Node::config();
            DomValue::Null
        }
        "formatOutput" => DomValue::Bool(n.doc_flag(DocFlag::FormatOutput)),
        "validateOnParse" => DomValue::Bool(n.doc_flag(DocFlag::ValidateOnParse)),
        "resolveExternals" => DomValue::Bool(n.doc_flag(DocFlag::ResolveExternals)),
        "preserveWhiteSpace" => DomValue::Bool(n.doc_flag(DocFlag::PreserveWhiteSpace)),
        "recover" => DomValue::Bool(n.doc_flag(DocFlag::Recover)),
        "substituteEntities" => DomValue::Bool(n.doc_flag(DocFlag::SubstituteEntities)),
        "body" => DomValue::opt_node(crate::html::body(n)?),
        "head" => DomValue::opt_node(crate::html::head(n)?),
        "title" => DomValue::Str(crate::html::title(n)?),
        _ => return Ok(PropRead::NotHandled),
    };
    Ok(PropRead::Value(v))
}

fn want_str(name: &str, v: &DomValue) -> DomResult<Vec<u8>> {
    match v {
        DomValue::Str(s) => Ok(s.clone()),
        DomValue::Int(i) => Ok(i.to_string().into_bytes()),
        DomValue::Float(f) => Ok(float_to_string(*f).into_bytes()),
        DomValue::Bool(b) => Ok(if *b { b"1".to_vec() } else { Vec::new() }),
        other => Err(DomError::Type(format!(
            "Cannot assign {} to property {name} of type string",
            other.type_name()
        ))),
    }
}

fn want_opt_str(name: &str, v: &DomValue) -> DomResult<Option<Vec<u8>>> {
    match v {
        DomValue::Null => Ok(None),
        other => want_str(name, other).map(Some),
    }
}

fn truthy(v: &DomValue) -> bool {
    match v {
        DomValue::Null => false,
        DomValue::Bool(b) => *b,
        DomValue::Int(i) => *i != 0,
        DomValue::Float(f) => *f != 0.0,
        DomValue::Str(s) => !(s.is_empty() || s == b"0"),
        DomValue::Array(a) => !a.is_empty(),
        _ => true,
    }
}

/// Writes a property (the VM has already checked visibility and coerced
/// the value to the declared type). Returns `false` when no DOM handler
/// applies (the VM writes the standard property table instead).
pub fn write_property(obj: &DomObject, name: &str, value: &DomValue) -> DomResult<bool> {
    if !is_handled_property(obj, name) {
        return Ok(false);
    }
    match obj {
        DomObject::XPath(x) if name == "registerNodeNamespaces" => {
            x.set_register_node_namespaces(truthy(value));
            Ok(true)
        }
        DomObject::TokenList(t) if name == "value" => {
            t.set_value(&want_str(name, value)?)?;
            Ok(true)
        }
        DomObject::Node(n) => write_node_property(n, name, value),
        _ => Err(DomError::Error(format!(
            "Cannot modify readonly property {}::${name}",
            obj.builtin_class()
        ))),
    }
}

fn write_node_property(n: &Node, name: &str, value: &DomValue) -> DomResult<bool> {
    let builtin = n.builtin_class();
    let modern = n.family_is_modern();
    let is_attr = builtin == "DOMAttr" || builtin == "Dom\\Attr";
    let doc_flag = |f: DocFlag| {
        n.set_doc_flag(f, truthy(value));
        Ok(true)
    };
    if modern
        && name == "textContent"
        && !matches!(
            builtin,
            "Dom\\DocumentFragment"
                | "Dom\\CharacterData"
                | "Dom\\Text"
                | "Dom\\CDATASection"
                | "Dom\\Comment"
                | "Dom\\ProcessingInstruction"
                | "Dom\\Attr"
                | "Dom\\Element"
                | "Dom\\HTMLElement"
        )
    {
        return Err(DomError::Error(format!(
            "Cannot modify readonly property {}::${name}",
            n.class_name()
        )));
    }
    match name {
        "nodeValue" => n.set_node_value(want_opt_str(name, value)?.as_deref())?,
        "textContent" => n.set_text_content(want_opt_str(name, value)?.as_deref())?,
        "prefix" => n.set_prefix(&want_str(name, value)?)?,
        "className" => n.set_class_name_attr(&want_str(name, value)?)?,
        "id" => n.set_id(&want_str(name, value)?)?,
        "data" => n.set_data(&want_str(name, value)?)?,
        "value" if is_attr => n.set_attr_value(&want_str(name, value)?)?,
        "innerHTML" => crate::inner_outer::set_inner_html(n, &want_str(name, value)?)?,
        "outerHTML" => crate::inner_outer::set_outer_html(n, &want_str(name, value)?)?,
        "substitutedNodeValue" => n.set_substituted_node_value(&want_str(name, value)?)?,
        "encoding" => n.set_encoding(want_opt_str(name, value)?.as_deref())?,
        "characterSet" | "charset" | "inputEncoding" => {
            n.set_character_set(&want_str(name, value)?)?;
        }
        "standalone" | "xmlStandalone" => n.set_standalone(truthy(value))?,
        "version" | "xmlVersion" => n.set_version(want_opt_str(name, value)?.as_deref())?,
        "documentURI" | "URL" => n.set_document_uri(want_opt_str(name, value)?.as_deref())?,
        "strictErrorChecking" => return doc_flag(DocFlag::StrictErrorChecking),
        "formatOutput" => return doc_flag(DocFlag::FormatOutput),
        "validateOnParse" => return doc_flag(DocFlag::ValidateOnParse),
        "resolveExternals" => return doc_flag(DocFlag::ResolveExternals),
        "preserveWhiteSpace" => return doc_flag(DocFlag::PreserveWhiteSpace),
        "recover" => return doc_flag(DocFlag::Recover),
        "substituteEntities" => return doc_flag(DocFlag::SubstituteEntities),
        "body" => crate::html::set_body(n, value.as_node())?,
        "title" => crate::html::set_title(n, &want_str(name, value)?)?,
        _ => {
            let _ = modern;
            return Err(DomError::Error(format!(
                "Cannot modify readonly property {builtin}::${name}"
            )));
        }
    }
    Ok(true)
}

/// `var_dump()` / `print_r()` view of the DOM-handled properties, in
/// PHP's order, with objects replaced by `"(object value omitted)"` and
/// failing handlers skipped (`dom_get_debug_info_helper`).
pub fn debug_info(obj: &DomObject) -> Vec<(&'static str, DomValue)> {
    debug_info_checked(obj).0
}

/// `dom_get_debug_info` plus the exception a failing property read left
/// in `EG(exception)` (e.g. `Invalid State Error` for an unconstructed
/// node): php-src skips that property, returns the table and the dump
/// throws afterwards. With several, the last one is the pending one.
/// Only `Invalid State Error`s (unconstructed objects) are reported; other
/// read failures stay silent.
pub fn debug_info_checked(obj: &DomObject) -> (Vec<(&'static str, DomValue)>, Option<DomError>) {
    let names = crate::classes::property_handlers(obj.builtin_class());
    crate::error::suppressed(|| {
        let mut out = Vec::new();
        let mut pending = None;
        for name in names {
            match read_property(obj, name) {
                Ok(PropRead::Value(v)) => {
                    let v = if v.is_object() {
                        DomValue::Str(b"(object value omitted)".to_vec())
                    } else {
                        v
                    };
                    out.push((*name, v));
                }
                // Unconstructed objects (`DOM_PROP_NODE` failing); other
                // failures stay silent like before.
                Err(
                    e @ DomError::Dom {
                        code: C::InvalidState,
                        ..
                    },
                ) => pending = Some(e),
                Ok(_) | Err(_) => {}
            }
        }
        (out, pending)
    })
}

/// Silence an unused-import warning for `C` in some configurations.
#[allow(dead_code)]
fn _codes() -> C {
    C::InvalidState
}

/// `clone $obj`.
pub fn clone_object(obj: &DomObject) -> DomResult<DomObject> {
    match obj {
        DomObject::Node(n) => Ok(DomObject::Node(n.clone_object()?)),
        DomObject::Implementation(i) if !i.modern => Ok(obj.clone()),
        other => Err(DomError::Error(format!(
            "Trying to clone an uncloneable object of class {}",
            other.builtin_class()
        ))),
    }
}

/// Creates the native state for `new Class()` of a non-node DOM class
/// (`new DOMNodeList()` and friends yield empty collections).
pub fn instantiate(builtin: &str) -> Option<DomObject> {
    use crate::lists::{new_list, new_map, MapKind};
    let c = crate::classes::find_class(builtin)?;
    Some(match c.name {
        "DOMNodeList" | "Dom\\NodeList" | "Dom\\HTMLCollection" => {
            DomObject::NodeList(new_list(c.name, None, MapKind::Noop))
        }
        "DOMNamedNodeMap" | "Dom\\NamedNodeMap" | "Dom\\DtdNamedNodeMap" => {
            DomObject::NamedNodeMap(new_map(c.name, None, MapKind::Noop))
        }
        "DOMImplementation" => DomObject::Implementation(Implementation::new(false)),
        "Dom\\Implementation" => DomObject::Implementation(Implementation::new(true)),
        _ => return None,
    })
}
