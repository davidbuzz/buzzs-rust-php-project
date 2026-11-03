//! `SoapClient::__getFunctions()` / `__getTypes()` renderings
//! (`function_to_string`, `type_to_string`, `model_to_string`).

use crate::consts::*;
use crate::sdl::*;

fn enc_type_str(sdl: &Sdl, e: Option<crate::encoders::EncRef>) -> Option<String> {
    e.and_then(|e| sdl.encoder(e))
        .and_then(|enc| enc.type_str.map(|s| s.to_string()))
}

fn enc_type_id(sdl: &Sdl, e: Option<crate::encoders::EncRef>) -> Option<i64> {
    e.and_then(|e| sdl.encoder(e)).map(|enc| enc.type_id)
}

/// `function_to_string`.
#[must_use]
pub fn function_to_string(sdl: &Sdl, f: &Function) -> String {
    let mut buf = String::new();
    match f.response_params.as_ref().filter(|p| !p.is_empty()) {
        Some(params) if params.len() == 1 => {
            match enc_type_str(sdl, params[0].encode) {
                Some(t) => buf.push_str(&t),
                None => buf.push_str("UNKNOWN"),
            }
            buf.push(' ');
        }
        Some(params) => {
            buf.push_str("list(");
            for (i, p) in params.iter().enumerate() {
                if i > 0 {
                    buf.push_str(", ");
                }
                buf.push_str(&enc_type_str(sdl, p.encode).unwrap_or_else(|| "UNKNOWN".into()));
                buf.push_str(" $");
                buf.push_str(p.name.as_deref().unwrap_or(""));
            }
            buf.push_str(") ");
        }
        None => buf.push_str("void "),
    }
    buf.push_str(&f.name);
    buf.push('(');
    if let Some(params) = &f.request_params {
        for (i, p) in params.iter().enumerate() {
            if i > 0 {
                buf.push_str(", ");
            }
            buf.push_str(&enc_type_str(sdl, p.encode).unwrap_or_else(|| "UNKNOWN".into()));
            buf.push_str(" $");
            buf.push_str(p.name.as_deref().unwrap_or(""));
        }
    }
    buf.push(')');
    buf
}

/// `model_to_string`.
fn model_to_string(sdl: &Sdl, model: &Model, buf: &mut String, level: usize) {
    match &model.kind {
        ModelKind::Element(t) => {
            type_to_string_into(sdl, *t, buf, level);
            buf.push_str(";\n");
        }
        ModelKind::Any => {
            buf.push_str(&" ".repeat(level));
            buf.push_str("<anyXML> any;\n");
        }
        ModelKind::Sequence(c) | ModelKind::All(c) | ModelKind::Choice(c) => {
            for m in c {
                model_to_string(sdl, m, buf, level);
            }
        }
        ModelKind::Group(g) => {
            if let Some(m) = &sdl.ty(*g).model {
                model_to_string(sdl, m, buf, level);
            }
        }
        ModelKind::GroupRef(_) => {}
    }
}

/// `type_to_string`.
#[must_use]
pub fn type_to_string(sdl: &Sdl, t: TypeId) -> String {
    let mut buf = String::new();
    type_to_string_into(sdl, t, &mut buf, 0);
    buf
}

fn extra<'a>(ty: &'a SdlType, attr_key: &str, ext_key: &str) -> Option<&'a ExtraAttribute> {
    ty.attributes
        .as_ref()?
        .get(attr_key)?
        .extra
        .as_ref()?
        .get(ext_key)
}

#[allow(clippy::too_many_lines)]
fn type_to_string_into(sdl: &Sdl, t: TypeId, buf: &mut String, level: usize) {
    let ty = sdl.ty(t);
    let spaces = " ".repeat(level);
    buf.push_str(&spaces);
    match ty.kind {
        TypeKind::Simple => {
            match ty.encode {
                Some(e) => {
                    buf.push_str(&enc_type_str(sdl, Some(e)).unwrap_or_default());
                    buf.push(' ');
                }
                None => buf.push_str("anyType "),
            }
            buf.push_str(ty.name_str());
            if let Some(en) = ty
                .restrictions
                .as_ref()
                .and_then(|r| r.enumeration.as_ref())
            {
                buf.push_str(" {");
                let mut first = true;
                for (k, _) in en.iter() {
                    if let Some(k) = k.as_str() {
                        if !first {
                            buf.push_str(", ");
                        }
                        first = false;
                        buf.push_str(k);
                    }
                }
                buf.push('}');
            }
        }
        TypeKind::List => {
            buf.push_str("list ");
            buf.push_str(ty.name_str());
            if let Some(els) = &ty.elements {
                buf.push_str(" {");
                for &e in els.values() {
                    buf.push_str(sdl.ty(e).name_str());
                }
                buf.push('}');
            }
        }
        TypeKind::Union => {
            buf.push_str("union ");
            buf.push_str(ty.name_str());
            if let Some(els) = &ty.elements {
                buf.push_str(" {");
                for &e in els.values() {
                    // C bug kept: the separator is emitted before every item.
                    buf.push(',');
                    buf.push_str(sdl.ty(e).name_str());
                }
                buf.push('}');
            }
        }
        TypeKind::Complex | TypeKind::Restriction | TypeKind::Extension => {
            let is_array = matches!(enc_type_id(sdl, ty.encode), Some(IS_ARRAY | SOAP_ENC_ARRAY));
            if is_array {
                if let Some(ext) = extra(
                    ty,
                    &format!("{SOAP_1_1_ENC_NAMESPACE}:arrayType"),
                    &format!("{WSDL_NAMESPACE}:arrayType"),
                ) {
                    let (head, end) = match ext.val.find('[') {
                        Some(p) => (&ext.val[..p], Some(&ext.val[p..])),
                        None => (ext.val.as_str(), None),
                    };
                    if head.is_empty() {
                        buf.push_str("anyType");
                    } else {
                        buf.push_str(head);
                    }
                    buf.push(' ');
                    buf.push_str(ty.name_str());
                    if let Some(end) = end {
                        buf.push_str(end);
                    }
                } else {
                    if let Some(ext) = extra(
                        ty,
                        &format!("{SOAP_1_2_ENC_NAMESPACE}:itemType"),
                        &format!("{WSDL_NAMESPACE}:itemType"),
                    ) {
                        buf.push_str(&ext.val);
                        buf.push(' ');
                    } else if let Some(t) = ty
                        .elements
                        .as_ref()
                        .filter(|e| e.len() == 1)
                        .and_then(|e| e.first())
                        .and_then(|&e| enc_type_str(sdl, sdl.ty(e).encode))
                    {
                        buf.push_str(&t);
                        buf.push(' ');
                    } else {
                        buf.push_str("anyType ");
                    }
                    buf.push_str(ty.name_str());
                    if let Some(ext) = extra(
                        ty,
                        &format!("{SOAP_1_2_ENC_NAMESPACE}:arraySize"),
                        &format!("{WSDL_NAMESPACE}:itemType"),
                    ) {
                        buf.push('[');
                        buf.push_str(&ext.val);
                        buf.push(']');
                    } else {
                        buf.push_str("[]");
                    }
                }
            } else {
                buf.push_str("struct ");
                buf.push_str(ty.name_str());
                buf.push_str(" {\n");
                if matches!(ty.kind, TypeKind::Restriction | TypeKind::Extension) {
                    if let Some(first) = ty.encode {
                        let mut enc = Some(first);
                        while let Some(e) = enc {
                            let Some(st) = sdl.encoder(e).and_then(|x| x.sdl_type) else {
                                break;
                            };
                            let st_ty = sdl.ty(st);
                            if Some(e) == st_ty.encode
                                || matches!(
                                    st_ty.kind,
                                    TypeKind::Simple | TypeKind::List | TypeKind::Union
                                )
                            {
                                break;
                            }
                            enc = st_ty.encode;
                        }
                        if enc.is_some() {
                            buf.push_str(&spaces);
                            buf.push(' ');
                            buf.push_str(&enc_type_str(sdl, Some(first)).unwrap_or_default());
                            buf.push_str(" _;\n");
                        }
                    }
                }
                if let Some(m) = &ty.model {
                    model_to_string(sdl, m, buf, level + 1);
                }
                if let Some(attrs) = &ty.attributes {
                    for a in attrs.values() {
                        buf.push_str(&spaces);
                        buf.push(' ');
                        match enc_type_str(sdl, a.encode) {
                            Some(t) => {
                                buf.push_str(&t);
                                buf.push(' ');
                            }
                            None => buf.push_str("UNKNOWN "),
                        }
                        buf.push_str(a.name.as_deref().unwrap_or(""));
                        buf.push_str(";\n");
                    }
                }
                buf.push_str(&spaces);
                buf.push('}');
            }
        }
    }
}

/// `__getFunctions()`.
#[must_use]
pub fn get_functions(sdl: &Sdl) -> Vec<String> {
    sdl.functions()
        .map(|f| function_to_string(sdl, f))
        .collect()
}

/// `__getTypes()`.
#[must_use]
pub fn get_types(sdl: &Sdl) -> Vec<String> {
    sdl.types
        .as_ref()
        .map(|ts| ts.iter().map(|&t| type_to_string(sdl, t)).collect())
        .unwrap_or_default()
}
