//! The service description model (`php_sdl.h`): WSDL functions, bindings,
//! messages and the XML Schema type registry, plus the encoder lookups of
//! `php_sdl.c` (`get_encoder`, `get_encoder_ex`, `get_element`, …).
//!
//! The C structures form a pointer graph; here types and encoders live in
//! arenas owned by [`Sdl`] and refer to each other by index.

use crate::consts::*;
use crate::encoders::{default_by_nscat, EncRef, Encoder, DEFAULT_ENCODERS};
use crate::xml::{parse_qname, Doc, NodeId};
use crate::zhash::ZHash;

/// Index of a type in [`Sdl::type_arena`].
pub type TypeId = u32;

/// `sdlTypeKind`.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    Simple,
    List,
    Union,
    Complex,
    Restriction,
    Extension,
}

/// `sdlForm`.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    Default,
    Qualified,
    Unqualified,
}

/// `sdlUse` (attribute use).
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrUse {
    Default,
    Optional,
    Prohibited,
    Required,
}

/// `sdlRestrictionInt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestrictionInt {
    /// Value.
    pub value: i32,
    /// `fixed="true"`.
    pub fixed: bool,
}

/// `sdlRestrictionChar`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestrictionChar {
    /// Value.
    pub value: String,
    /// `fixed="true"`.
    pub fixed: bool,
}

/// `sdlRestrictions`.
#[allow(missing_docs)]
#[derive(Debug, Clone, Default)]
pub struct Restrictions {
    pub enumeration: Option<ZHash<RestrictionChar>>,
    pub min_exclusive: Option<RestrictionInt>,
    pub min_inclusive: Option<RestrictionInt>,
    pub max_exclusive: Option<RestrictionInt>,
    pub max_inclusive: Option<RestrictionInt>,
    pub total_digits: Option<RestrictionInt>,
    pub fraction_digits: Option<RestrictionInt>,
    pub length: Option<RestrictionInt>,
    pub min_length: Option<RestrictionInt>,
    pub max_length: Option<RestrictionInt>,
    pub white_space: Option<RestrictionChar>,
    pub pattern: Option<RestrictionChar>,
}

/// `sdlContentModel` kinds with their payloads.
#[derive(Debug, Clone)]
pub enum ModelKind {
    /// `XSD_CONTENT_ELEMENT`.
    Element(TypeId),
    /// `XSD_CONTENT_SEQUENCE`.
    Sequence(Vec<Model>),
    /// `XSD_CONTENT_ALL`.
    All(Vec<Model>),
    /// `XSD_CONTENT_CHOICE`.
    Choice(Vec<Model>),
    /// `XSD_CONTENT_GROUP_REF` (unresolved `ns:name`).
    GroupRef(String),
    /// `XSD_CONTENT_GROUP`.
    Group(TypeId),
    /// `XSD_CONTENT_ANY`.
    Any,
}

/// `sdlContentModel`.
#[derive(Debug, Clone)]
pub struct Model {
    /// Kind and content.
    pub kind: ModelKind,
    /// `minOccurs`.
    pub min_occurs: i32,
    /// `maxOccurs` (`-1` = unbounded).
    pub max_occurs: i32,
}

impl Model {
    /// The content list of a sequence/all/choice.
    #[must_use]
    pub fn content(&self) -> Option<&Vec<Model>> {
        match &self.kind {
            ModelKind::Sequence(c) | ModelKind::All(c) | ModelKind::Choice(c) => Some(c),
            _ => None,
        }
    }

    /// Mutable content list.
    pub fn content_mut(&mut self) -> Option<&mut Vec<Model>> {
        match &mut self.kind {
            ModelKind::Sequence(c) | ModelKind::All(c) | ModelKind::Choice(c) => Some(c),
            _ => None,
        }
    }
}

/// `sdlExtraAttribute`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraAttribute {
    /// Namespace of the `QName` value.
    pub ns: Option<String>,
    /// Local part (or the whole value when unprefixed/unresolved).
    pub val: String,
}

/// `sdlAttribute`.
#[allow(missing_docs)]
#[derive(Debug, Clone)]
pub struct SdlAttribute {
    pub name: Option<String>,
    pub namens: Option<String>,
    pub ref_: Option<String>,
    pub def: Option<String>,
    pub fixed: Option<String>,
    pub form: Form,
    pub use_: AttrUse,
    pub extra: Option<ZHash<ExtraAttribute>>,
    pub encode: Option<EncRef>,
}

impl Default for SdlAttribute {
    fn default() -> Self {
        Self {
            name: None,
            namens: None,
            ref_: None,
            def: None,
            fixed: None,
            form: Form::Default,
            use_: AttrUse::Default,
            extra: None,
            encode: None,
        }
    }
}

/// `sdlType`.
#[allow(missing_docs)]
#[derive(Debug, Clone)]
pub struct SdlType {
    pub kind: TypeKind,
    pub nillable: bool,
    pub name: Option<String>,
    pub namens: Option<String>,
    pub elements: Option<ZHash<TypeId>>,
    pub attributes: Option<ZHash<SdlAttribute>>,
    pub restrictions: Option<Restrictions>,
    pub encode: Option<EncRef>,
    pub model: Option<Model>,
    pub def: Option<String>,
    pub fixed: Option<String>,
    pub ref_: Option<String>,
    pub form: Form,
}

impl Default for SdlType {
    fn default() -> Self {
        Self {
            kind: TypeKind::Simple,
            nillable: false,
            name: None,
            namens: None,
            elements: None,
            attributes: None,
            restrictions: None,
            encode: None,
            model: None,
            def: None,
            fixed: None,
            ref_: None,
            form: Form::Default,
        }
    }
}

impl SdlType {
    /// The name, or `""`.
    #[must_use]
    pub fn name_str(&self) -> &str {
        self.name.as_deref().unwrap_or("")
    }
}

/// `sdlParam`.
#[derive(Debug, Clone)]
pub struct Param {
    /// `order`.
    pub order: i32,
    /// `element=` part.
    pub element: Option<TypeId>,
    /// Encoder.
    pub encode: Option<EncRef>,
    /// `paramName`.
    pub name: Option<String>,
}

/// `sdlBindingType`.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingType {
    Soap,
    Http,
}

/// `sdlSoapBinding`.
#[derive(Debug, Clone, Copy)]
pub struct SoapBinding {
    /// `SOAP_RPC` / `SOAP_DOCUMENT`.
    pub style: i64,
    /// Whether the transport is HTTP.
    pub transport_http: bool,
}

/// `sdlBinding`.
#[derive(Debug, Clone)]
pub struct Binding {
    /// Name.
    pub name: String,
    /// Endpoint location.
    pub location: String,
    /// Type.
    pub binding_type: BindingType,
    /// SOAP-specific attributes.
    pub soap: Option<SoapBinding>,
}

/// `sdlRpcEncodingStyle`.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodingStyle {
    Default,
    Soap11,
    Soap12,
}

/// `sdlSoapBindingFunctionHeader`.
#[derive(Debug, Clone)]
pub struct HeaderBinding {
    /// Name.
    pub name: String,
    /// Namespace.
    pub ns: Option<String>,
    /// `SOAP_ENCODED` / `SOAP_LITERAL`.
    pub use_: i64,
    /// Encoding style.
    pub encoding_style: EncodingStyle,
    /// Element of the part.
    pub element: Option<TypeId>,
    /// Encoder.
    pub encode: Option<EncRef>,
    /// `headerfault`s keyed by `ns:name`.
    pub headerfaults: Option<ZHash<HeaderBinding>>,
}

/// `sdlSoapBindingFunctionBody`.
#[derive(Debug, Clone)]
pub struct BodyBinding {
    /// `namespace=`.
    pub ns: Option<String>,
    /// `SOAP_ENCODED` / `SOAP_LITERAL` (0 when there was no `soap:body`).
    pub use_: i64,
    /// Encoding style.
    pub encoding_style: EncodingStyle,
    /// Headers keyed by `ns:name`.
    pub headers: Option<ZHash<HeaderBinding>>,
}

impl Default for BodyBinding {
    fn default() -> Self {
        Self {
            ns: None,
            use_: 0,
            encoding_style: EncodingStyle::Default,
            headers: None,
        }
    }
}

/// `sdlSoapBindingFunction`.
#[derive(Debug, Clone)]
pub struct FunctionBinding {
    /// `soapAction`.
    pub soap_action: Option<String>,
    /// `SOAP_RPC` / `SOAP_DOCUMENT`.
    pub style: i64,
    /// Input body.
    pub input: BodyBinding,
    /// Output body.
    pub output: BodyBinding,
}

/// `sdlSoapBindingFunctionFault`.
#[derive(Debug, Clone)]
pub struct FaultBinding {
    /// `namespace=`.
    pub ns: Option<String>,
    /// Use.
    pub use_: i64,
    /// Encoding style.
    pub encoding_style: EncodingStyle,
}

/// `sdlFault`.
#[derive(Debug, Clone)]
pub struct Fault {
    /// Name.
    pub name: String,
    /// Detail parts.
    pub details: Vec<Param>,
    /// SOAP binding.
    pub binding: Option<FaultBinding>,
}

/// `sdlFunction`.
#[derive(Debug, Clone)]
pub struct Function {
    /// `functionName`.
    pub name: String,
    /// `requestName`.
    pub request_name: Option<String>,
    /// `responseName`.
    pub response_name: Option<String>,
    /// Request parts.
    pub request_params: Option<Vec<Param>>,
    /// Response parts.
    pub response_params: Option<Vec<Param>>,
    /// Index into [`Sdl::binding_list`].
    pub binding: usize,
    /// SOAP binding attributes.
    pub soap: Option<FunctionBinding>,
    /// Faults keyed by name.
    pub faults: Option<ZHash<Fault>>,
}

/// `sdl`.
#[derive(Debug, Clone, Default)]
pub struct Sdl {
    /// All types (arena).
    pub type_arena: Vec<SdlType>,
    /// `sdl->types` (in definition order); `None` when no schema.
    pub types: Option<Vec<TypeId>>,
    /// `sdl->elements` keyed by `ns:name`.
    pub elements: Option<ZHash<TypeId>>,
    /// Encoders owned by the SDL (arena).
    pub enc_arena: Vec<Encoder>,
    /// `sdl->encoders` keyed by `ns:type` (anonymous types have integer keys).
    pub encoders: ZHash<EncRef>,
    /// `sdl->bindings`.
    pub bindings: ZHash<usize>,
    /// Binding storage.
    pub binding_list: Vec<Binding>,
    /// `sdl->functions` keyed by lower-case name.
    pub functions: ZHash<usize>,
    /// Function storage.
    pub function_list: Vec<Function>,
    /// `sdl->requests`.
    pub requests: Option<ZHash<usize>>,
    /// `sdl->groups` keyed by `ns:name`.
    pub groups: Option<ZHash<TypeId>>,
    /// `targetNamespace` of the definitions.
    pub target_ns: Option<String>,
    /// The URI the WSDL was loaded from.
    pub source: String,
}

impl Sdl {
    /// A type.
    #[must_use]
    pub fn ty(&self, id: TypeId) -> &SdlType {
        &self.type_arena[id as usize]
    }

    /// A mutable type.
    pub fn ty_mut(&mut self, id: TypeId) -> &mut SdlType {
        &mut self.type_arena[id as usize]
    }

    /// Adds a type to the arena.
    pub fn new_type(&mut self, t: SdlType) -> TypeId {
        self.type_arena.push(t);
        u32::try_from(self.type_arena.len() - 1).unwrap_or(u32::MAX)
    }

    /// Adds an encoder to the arena.
    pub fn new_encoder(&mut self, e: Encoder) -> EncRef {
        self.enc_arena.push(e);
        EncRef::Sdl(u32::try_from(self.enc_arena.len() - 1).unwrap_or(u32::MAX))
    }

    /// Resolves an encoder reference that is not a typemap entry.
    #[must_use]
    pub fn encoder(&self, e: EncRef) -> Option<Encoder> {
        resolve_static(Some(self), e)
    }

    /// Functions in definition order.
    pub fn functions(&self) -> impl Iterator<Item = &Function> {
        self.functions.values().map(|&i| &self.function_list[i])
    }

    /// The binding of a function.
    #[must_use]
    pub fn binding_of(&self, f: &Function) -> &Binding {
        &self.binding_list[f.binding]
    }

    /// `get_function`: by name (case-insensitive), then by request name.
    #[must_use]
    pub fn get_function(&self, name: &[u8]) -> Option<&Function> {
        let lc = String::from_utf8_lossy(name).to_ascii_lowercase();
        if let Some(&i) = self.functions.get(&lc) {
            return Some(&self.function_list[i]);
        }
        self.requests
            .as_ref()
            .and_then(|r| r.get(&lc))
            .map(|&i| &self.function_list[i])
    }
}

/// Resolves built-in, alias and SDL encoders.
#[must_use]
pub fn resolve_static(sdl: Option<&Sdl>, e: EncRef) -> Option<Encoder> {
    match e {
        EncRef::Default(i) => DEFAULT_ENCODERS
            .get(i as usize)
            .map(super::encoders::DefaultEncoder::to_encoder),
        EncRef::Alias(i, soap12) => DEFAULT_ENCODERS.get(i as usize).map(|d| {
            let mut enc = d.to_encoder();
            enc.ns = Some(
                if soap12 {
                    SOAP_1_2_ENC_NAMESPACE
                } else {
                    SOAP_1_1_ENC_NAMESPACE
                }
                .into(),
            );
            enc
        }),
        EncRef::Sdl(i) => sdl.and_then(|s| s.enc_arena.get(i as usize).cloned()),
        EncRef::Map(_) => None,
    }
}

/// `get_encoder_ex`: built-in table first, then the SDL encoders.
#[must_use]
pub fn get_encoder_ex(sdl: Option<&Sdl>, nscat: &str) -> Option<EncRef> {
    if let Some(i) = default_by_nscat(nscat) {
        return Some(EncRef::Default(i));
    }
    sdl.and_then(|s| s.encoders.get(nscat).copied())
}

/// `get_encoder(sdl, ns, type)`, including the SOAP-ENC → XSD fallback.
/// The fallback encoder is registered in the SDL (when there is one) as
/// an [`EncRef::Alias`].
#[must_use]
pub fn get_encoder(sdl: Option<&Sdl>, ns: Option<&str>, type_: &str) -> Option<EncRef> {
    let nscat = format!("{}:{type_}", ns.unwrap_or(""));
    if let Some(e) = get_encoder_ex(sdl, &nscat) {
        return Some(e);
    }
    let soap12 = match ns {
        Some(SOAP_1_1_ENC_NAMESPACE) => false,
        Some(SOAP_1_2_ENC_NAMESPACE) => true,
        _ => return None,
    };
    let base = default_by_nscat(&format!("{XSD_NAMESPACE}:{type_}"))?;
    if sdl.is_some() {
        Some(EncRef::Alias(base, soap12))
    } else {
        Some(EncRef::Default(base))
    }
}

/// [`get_encoder`] for the loader, which records aliases in the SDL.
pub fn get_encoder_mut(sdl: &mut Sdl, ns: &str, type_: &str) -> Option<EncRef> {
    let e = get_encoder(Some(sdl), Some(ns), type_)?;
    if let EncRef::Alias(..) = e {
        sdl.encoders.update(&format!("{ns}:{type_}"), e);
    }
    Some(e)
}

/// `get_encoder_from_prefix`: resolves the `QName` `type_` in the scope of
/// `node`.
#[must_use]
pub fn get_encoder_from_prefix(
    sdl: Option<&Sdl>,
    doc: &Doc,
    node: NodeId,
    type_: &[u8],
) -> Option<EncRef> {
    let (prefix, local) = parse_qname(type_);
    let local_s = String::from_utf8_lossy(local);
    if let Some(ns) = doc.search_ns(node, prefix) {
        let href = String::from_utf8_lossy(&ns.href);
        get_encoder(sdl, Some(&href), &local_s).or_else(|| get_encoder_ex(sdl, &local_s))
    } else {
        get_encoder_ex(sdl, &String::from_utf8_lossy(type_))
    }
}

/// `get_element`: looks a global element up by `QName` in the scope of
/// `node`.
#[must_use]
pub fn get_element(sdl: &Sdl, doc: &Doc, node: NodeId, type_: &[u8]) -> Option<TypeId> {
    let elements = sdl.elements.as_ref()?;
    let (prefix, local) = parse_qname(type_);
    if let Some(ns) = doc.search_ns(node, prefix) {
        let nscat = format!(
            "{}:{}",
            String::from_utf8_lossy(&ns.href),
            String::from_utf8_lossy(local)
        );
        elements
            .get(&nscat)
            .or_else(|| {
                // `zend_hash_str_find_ptr(sdl->elements, type, type_len)`
                // with the length of the local part: a prefix of the QName.
                let t = &type_[..local.len().min(type_.len())];
                elements.get(&String::from_utf8_lossy(t))
            })
            .copied()
    } else {
        elements.get(&String::from_utf8_lossy(type_)).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoders::ToXml;

    #[test]
    fn soap_enc_alias() {
        let sdl = Sdl::default();
        let e = get_encoder(Some(&sdl), Some(SOAP_1_1_ENC_NAMESPACE), "string").unwrap();
        assert!(matches!(e, EncRef::Alias(_, false)));
        let enc = sdl.encoder(e).unwrap();
        assert_eq!(enc.ns.as_deref(), Some(SOAP_1_1_ENC_NAMESPACE));
        assert_eq!(enc.to_xml, ToXml::String);
        assert!(matches!(
            get_encoder(None, Some(SOAP_1_1_ENC_NAMESPACE), "string"),
            Some(EncRef::Default(_))
        ));
        // SOAP-ENC:Array is a real entry.
        assert!(matches!(
            get_encoder(Some(&sdl), Some(SOAP_1_1_ENC_NAMESPACE), "Array"),
            Some(EncRef::Default(_))
        ));
        assert!(get_encoder(Some(&sdl), Some("urn:x"), "string").is_none());
    }
}
