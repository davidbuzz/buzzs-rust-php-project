//! The `typemap` option (`soap_create_typemap`).

use std::rc::Rc;

use php_runtime::array::ArrayKey;
use php_runtime::{RuntimeError, Value, ZArray};

use crate::encoders::{conv, Mapping, ToXml, ToZval};
use crate::encoding::Typemap;
use crate::error::{SoapError, SoapResult};
use crate::host::SoapHost;
use crate::sdl::{get_encoder, get_encoder_ex, resolve_static, Sdl};
use crate::consts::UNKNOWN_TYPE;

/// Builds the typemap encoders from the option array.
pub fn create_typemap(host: &mut dyn SoapHost, sdl: Option<&Sdl>, ht: &ZArray) -> SoapResult<Option<Typemap>> {
    let _ = host;
    let mut typemap: Option<Typemap> = None;
    for tmp in ht.values() {
        let Value::Array(entry) = tmp.deref() else {
            return Err(SoapError::Runtime(RuntimeError::TypeError(format!(
                "SoapHeader::__construct(): \"typemap\" option must be of type array, {} given",
                crate::client_type_name(&tmp.deref())
            ))));
        };
        let mut type_name = None;
        let mut type_ns = None;
        let mut to_xml = None;
        let mut to_zval = None;
        if !entry.is_packed() {
            for (k, v) in entry.iter() {
                let ArrayKey::Str(k) = k else {
                    continue;
                };
                match k.as_bytes() {
                    b"type_name" => {
                        if let Value::Str(s) = v.deref() {
                            type_name = Some(String::from_utf8_lossy(s.as_bytes()).into_owned());
                        }
                    }
                    b"type_ns" => {
                        if let Value::Str(s) = v.deref() {
                            type_ns = Some(String::from_utf8_lossy(s.as_bytes()).into_owned());
                        }
                    }
                    b"to_xml" => to_xml = Some(v.deref()),
                    b"from_xml" => to_zval = Some(v.deref()),
                    _ => {}
                }
            }
        }
        let Some(type_name) = type_name else {
            continue;
        };
        let enc = match &type_ns {
            Some(ns) => get_encoder(sdl, Some(ns), &type_name),
            None => get_encoder_ex(sdl, &type_name),
        };
        let base = enc.and_then(|e| resolve_static(sdl, e));
        let mut new_enc = match &base {
            Some(b) => b.clone(),
            None => {
                let mut u = resolve_static(None, conv(UNKNOWN_TYPE)).unwrap_or_else(|| unreachable!());
                u.type_id = UNKNOWN_TYPE;
                u.ns = type_ns.as_deref().map(Rc::from);
                u.type_str = Some(Rc::from(type_name.as_str()));
                u
            }
        };
        let base_map = base.as_ref().and_then(|b| b.map.clone());
        let mut map = Mapping::default();
        if let Some(x) = to_xml {
            map.to_xml = Some(x);
            new_enc.to_xml = ToXml::User;
        } else if let Some(m) = base_map.as_ref().and_then(|m| m.to_xml.clone()) {
            map.to_xml = Some(m);
        }
        if let Some(z) = to_zval {
            map.to_zval = Some(z);
            new_enc.to_zval = ToZval::User;
        } else if let Some(m) = base_map.as_ref().and_then(|m| m.to_zval.clone()) {
            map.to_zval = Some(m);
        }
        new_enc.map = Some(Rc::new(map));
        let key = match &type_ns {
            Some(ns) => format!("{ns}:{type_name}"),
            None => type_name.clone(),
        };
        let tm = typemap.get_or_insert_with(Typemap::default);
        let idx = u32::try_from(tm.encoders.len()).unwrap_or(u32::MAX);
        tm.encoders.push(new_enc);
        tm.map.update(&key, idx);
    }
    Ok(typemap)
}
