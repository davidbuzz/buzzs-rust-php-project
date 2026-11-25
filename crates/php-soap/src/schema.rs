// Derived from php-src ext/soap/php_schema.c. Similarity metric to original: 12%
//   Copyright © The PHP Group and Contributors.
//   Authors: Brad Lafountain <rodif_bl@yahoo.com>
//            Shane Caraveo <shane@caraveo.com>
//            Dmitry Stogov <dmitry@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! XML Schema loading (`php_schema.c`): turns `<xsd:schema>` elements into
//! [`SdlType`]s, encoders, elements, groups and attributes, then resolves
//! references in a second pass (`schema_pass2`).

use std::rc::Rc;

use php_runtime::numeric::{is_numeric_string_ex, Number};

use crate::bail;
use crate::consts::*;
use crate::encoders::{conv, EncRef, Encoder};
use crate::error::SoapResult;
use crate::sdl::*;
use crate::wsdl::LoadCtx;
use crate::xml::{parse_qname, whitespace_collapse, Doc, NodeId};
use crate::zhash::{ZHash, ZKey};

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `node_is_equal_xsd`.
fn is_xsd(doc: &Doc, n: NodeId, name: &str) -> bool {
    if doc.name(n) != name.as_bytes() || !doc.is_element(n) {
        return false;
    }
    doc.node_find_ns(n).is_some_and(|ns| {
        [XSD_NAMESPACE, XSD_DRAFT_2000_NAMESPACE, XSD_1999_NAMESPACE]
            .iter()
            .any(|x| ns.href == x.as_bytes())
    })
}

fn attr(doc: &Doc, n: NodeId, name: &str) -> Option<String> {
    doc.attr_value(n, name, None).map(lossy)
}

/// C `strtol(s, NULL, 10)` with `errno == ERANGE` detection.
fn strtol(s: &[u8]) -> (i64, bool) {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let mut v: i128 = 0;
    let mut range = false;
    while i < s.len() && s[i].is_ascii_digit() {
        v = v * 10 + i128::from(s[i] - b'0');
        if v > i128::from(i64::MAX) + 1 {
            range = true;
            v = i128::from(i64::MAX) + 1;
        }
        i += 1;
    }
    if neg {
        v = -v;
    }
    if v > i128::from(i64::MAX) {
        return (i64::MAX, true);
    }
    if v < i128::from(i64::MIN) {
        return (i64::MIN, true);
    }
    (i64::try_from(v).unwrap_or(0), range)
}

/// `schema_parse_int`.
fn parse_int(value: &[u8], name: &str, allow_negative: bool) -> SoapResult<i32> {
    let parsed = is_numeric_string_ex(value, true);
    let lval = match parsed {
        Some(n) if matches!(n.value, Number::Int(_)) => match n.value {
            Number::Int(i) => i,
            Number::Float(_) => 0,
        },
        other => {
            let oflow = other.is_some_and(|n| n.overflow != 0);
            let (l, range) = strtol(value);
            if oflow || (range && l != 0) {
                bail!("Parsing Schema: {name} value is out of range");
            }
            l
        }
    };
    if i32::try_from(lval).is_err() || (!allow_negative && lval < 0) {
        bail!("Parsing Schema: {name} value is out of range");
    }
    Ok(i32::try_from(lval).unwrap_or(0))
}

/// `create_encoder`: (re)binds the encoder registered as `ns:type` to
/// `cur_type`.
fn create_encoder(sdl: &mut Sdl, cur_type: Option<TypeId>, ns: &str, type_: &str) -> EncRef {
    let nscat = format!("{ns}:{type_}");
    let fresh = Encoder::sdl(ns, type_, cur_type);
    if let Some(&EncRef::Sdl(i)) = sdl.encoders.get(&nscat) {
        sdl.enc_arena[i as usize] = fresh;
        return EncRef::Sdl(i);
    }
    let e = sdl.new_encoder(fresh);
    sdl.encoders.update(&nscat, e);
    e
}

/// `get_create_encoder`.
fn get_create_encoder(sdl: &mut Sdl, cur_type: Option<TypeId>, ns: &str, type_: &str) -> EncRef {
    get_encoder_mut(sdl, ns, type_).unwrap_or_else(|| create_encoder(sdl, cur_type, ns, type_))
}

/// Resolves the `QName` in attribute `value` of `node` to `(href, local)`.
fn resolve_qname(doc: &Doc, node: NodeId, value: &[u8]) -> Option<(String, String)> {
    let (prefix, local) = parse_qname(value);
    doc.search_ns(node, prefix)
        .map(|ns| (lossy(&ns.href), lossy(local)))
}

fn anonymous_name(sdl: &Sdl) -> String {
    format!("anonymous{}", sdl.types.as_ref().map_or(0, Vec::len))
}

/// `schema_location_construct_uri`.
pub fn location_uri(doc: &Doc, node: NodeId, location: &[u8]) -> String {
    let loc = lossy(location);
    match node_base(doc, node) {
        Some(base) => build_uri(&loc, Some(&base)),
        None => build_uri(&loc, doc.url.as_deref()),
    }
}

/// `xmlNodeGetBase` without the document URL fallback merged in.
fn node_base(doc: &Doc, node: NodeId) -> Option<String> {
    let mut oldbase: Option<String> = None;
    let mut cur = Some(node);
    while let Some(c) = cur {
        if doc.is_element(c) {
            if let Some(b) = doc.attr_value(c, "base", Some(XML_NAMESPACE)) {
                let b = lossy(b);
                let nb = match &oldbase {
                    Some(old) => build_uri(old, Some(&b)),
                    None => b,
                };
                if nb.starts_with("http://") || nb.starts_with("ftp://") || nb.starts_with("urn:") {
                    return Some(nb);
                }
                oldbase = Some(nb);
            }
        }
        cur = doc.parent(c);
    }
    match (&doc.url, oldbase) {
        (Some(url), None) => Some(url.clone()),
        (Some(url), Some(old)) => Some(build_uri(&old, Some(url))),
        (None, old) => old,
    }
}

fn has_scheme(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || !b[0].is_ascii_alphabetic() {
        return false;
    }
    for &c in &b[1..] {
        if c == b':' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.')) {
            return false;
        }
    }
    false
}

fn remove_dot_segments(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    let segs: Vec<&str> = path.split('/').collect();
    let last = segs.len().saturating_sub(1);
    for (i, seg) in segs.iter().enumerate() {
        match *seg {
            "." => {
                if i == last {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > usize::from(absolute) {
                    out.pop();
                }
                if i == last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let mut r = out.join("/");
    if absolute && !r.starts_with('/') {
        r.insert(0, '/');
    }
    r
}

/// `xmlBuildURI` (RFC 3986 reference resolution, also for plain paths).
#[must_use]
pub fn build_uri(uri: &str, base: Option<&str>) -> String {
    let Some(base) = base else {
        return uri.to_owned();
    };
    if has_scheme(uri) || base.is_empty() {
        return uri.to_owned();
    }
    if uri.is_empty() {
        return base.to_owned();
    }
    let (prefix, base_path) = if has_scheme(base) {
        let colon = base.find(':').unwrap_or(0);
        let after = &base[colon + 1..];
        if let Some(rest) = after.strip_prefix("//") {
            let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            (
                format!("{}//{}", &base[..=colon], &rest[..end]),
                rest[end..].to_owned(),
            )
        } else {
            (base[..=colon].to_owned(), after.to_owned())
        }
    } else {
        (String::new(), base.to_owned())
    };
    if uri.starts_with("//") {
        let scheme_end = prefix.find(':').map_or(0, |p| p + 1);
        return format!("{}{uri}", &prefix[..scheme_end]);
    }
    let base_path = base_path.split(['?', '#']).next().unwrap_or("").to_owned();
    let merged = if uri.starts_with('/') {
        uri.to_owned()
    } else {
        let dir = match base_path.rfind('/') {
            Some(p) => &base_path[..=p],
            None => {
                if prefix.contains("//") {
                    "/"
                } else {
                    ""
                }
            }
        };
        format!("{dir}{uri}")
    };
    let (path, tail) = match merged.find(['?', '#']) {
        Some(p) => (merged[..p].to_owned(), merged[p..].to_owned()),
        None => (merged, String::new()),
    };
    format!("{prefix}{}{tail}", remove_dot_segments(&path))
}

impl LoadCtx<'_> {
    /// `load_schema`.
    pub fn load_schema(&mut self, doc: &Rc<Doc>, schema: NodeId) -> SoapResult<()> {
        if self.sdl.types.is_none() {
            self.sdl.types = Some(Vec::new());
        }
        if self.attributes.is_none() {
            self.attributes = Some(ZHash::new());
        }
        if self.attribute_groups.is_none() {
            self.attribute_groups = Some(ZHash::new());
        }
        // `prepare_schema` already stored a (possibly empty) targetNamespace.
        let tns = attr(doc, schema, "targetNamespace").unwrap_or_default();

        let mut trav = doc.first_child(schema);
        while let Some(t) = trav {
            if is_xsd(doc, t, "include") || is_xsd(doc, t, "redefine") {
                let what = if is_xsd(doc, t, "include") {
                    "include"
                } else {
                    "redefine"
                };
                let Some(location) = doc.attr_value(t, "schemaLocation", None) else {
                    bail!("Parsing Schema: {what} has no 'schemaLocation' attribute");
                };
                let uri = location_uri(doc, t, location);
                self.schema_load_file(None, Some(uri), &tns, false)?;
            } else if is_xsd(doc, t, "import") {
                let ns = attr(doc, t, "namespace");
                let location = doc.attr_value(t, "schemaLocation", None);
                if ns.as_deref() == Some(tns.as_str()) {
                    if let Some(loc) = location {
                        bail!(
                            "Parsing Schema: can't import schema from '{}', namespace must not match the enclosing schema 'targetNamespace'",
                            lossy(loc)
                        );
                    }
                    bail!("Parsing Schema: can't import schema. Namespace must not match the enclosing schema 'targetNamespace'");
                }
                let uri = location.map(|l| location_uri(doc, t, l));
                self.schema_load_file(ns, uri, &tns, true)?;
            } else if !is_xsd(doc, t, "annotation") {
                break;
            }
            trav = doc.next(t);
        }

        while let Some(t) = trav {
            if is_xsd(doc, t, "simpleType") {
                self.simple_type(&tns, doc, t, None)?;
            } else if is_xsd(doc, t, "complexType") {
                self.complex_type(&tns, doc, t, None)?;
            } else if is_xsd(doc, t, "group") {
                self.group(&tns, doc, t, None, None)?;
            } else if is_xsd(doc, t, "attributeGroup") {
                self.attribute_group(&tns, doc, t, None)?;
            } else if is_xsd(doc, t, "element") {
                self.element(&tns, doc, t, None, None)?;
            } else if is_xsd(doc, t, "attribute") {
                self.attribute(&tns, doc, t, None)?;
            } else if !is_xsd(doc, t, "notation") && !is_xsd(doc, t, "annotation") {
                bail!(
                    "Parsing Schema: unexpected <{}> in schema",
                    lossy(doc.name(t))
                );
            }
            trav = doc.next(t);
        }
        Ok(())
    }

    /// `schema_load_file`.
    #[allow(clippy::needless_pass_by_value)]
    fn schema_load_file(
        &mut self,
        ns: Option<String>,
        location: Option<String>,
        tns: &str,
        import: bool,
    ) -> SoapResult<()> {
        let Some(location) = location else {
            return Ok(());
        };
        if self.docs.contains(&location) {
            return Ok(());
        }
        let Some(mut doc) = self.fetch_doc(&location) else {
            bail!("Parsing Schema: can't import schema from '{location}'");
        };
        let Some(schema) = doc.get_node(doc.first_top(), "schema", None) else {
            bail!("Parsing Schema: can't import schema from '{location}'");
        };
        let new_tns = attr(&doc, schema, "targetNamespace");
        if import {
            if let Some(ns) = &ns {
                match &new_tns {
                    None => bail!(
                        "Parsing Schema: can't import schema from '{location}', missing 'targetNamespace', expected '{ns}'"
                    ),
                    Some(t) if t != ns => bail!(
                        "Parsing Schema: can't import schema from '{location}', unexpected 'targetNamespace'='{t}', expected '{ns}'"
                    ),
                    _ => {}
                }
            }
            if ns.is_none() {
                if let Some(t) = &new_tns {
                    bail!(
                        "Parsing Schema: can't import schema from '{location}', unexpected 'targetNamespace'='{t}', expected no 'targetNamespace'"
                    );
                }
            }
        } else if new_tns.is_none() {
            doc.set_prop(schema, b"targetNamespace", tns.as_bytes());
        } else if new_tns.as_deref() != Some(tns) {
            bail!("Parsing Schema: can't include schema from '{location}', different 'targetNamespace'");
        }
        prepare_schema(&mut doc, schema);
        let doc = Rc::new(doc);
        self.docs.add(&location, Rc::clone(&doc));
        self.load_schema(&doc, schema)
    }

    /// `schema_simpleType`.
    fn simple_type(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur_type: Option<TypeId>,
    ) -> SoapResult<()> {
        let ns = attr(doc, node, "targetNamespace").unwrap_or_else(|| tns.to_owned());
        let name = attr(doc, node, "name");
        let cur = if let Some(ct) = cur_type {
            // Anonymous type inside <element> or <restriction>.
            let (n, nns) = match &name {
                Some(n) => (Some(n.clone()), Some(ns.clone())),
                None => (self.sdl.ty(ct).name.clone(), self.sdl.ty(ct).namens.clone()),
            };
            let new_type = self.sdl.new_type(SdlType {
                kind: TypeKind::Simple,
                name: n.clone(),
                namens: nns.clone(),
                ..SdlType::default()
            });
            self.sdl.types.get_or_insert_with(Vec::new).push(new_type);
            let mut enc = Encoder::sdl(
                nns.as_deref().unwrap_or(""),
                n.as_deref().unwrap_or(""),
                Some(new_type),
            );
            if nns.is_none() {
                enc.ns = None;
            }
            let e = self.sdl.new_encoder(enc);
            self.sdl.encoders.push(e);
            self.sdl.ty_mut(ct).encode = Some(e);
            new_type
        } else if let Some(name) = &name {
            let new_type = self.sdl.new_type(SdlType {
                kind: TypeKind::Simple,
                name: Some(name.clone()),
                namens: Some(ns.clone()),
                ..SdlType::default()
            });
            self.sdl.types.get_or_insert_with(Vec::new).push(new_type);
            create_encoder(&mut self.sdl, Some(new_type), &ns, name);
            new_type
        } else {
            bail!("Parsing Schema: simpleType has no 'name' attribute");
        };

        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        let Some(t) = trav else {
            bail!("Parsing Schema: expected <restriction>, <list> or <union> in simpleType");
        };
        if is_xsd(doc, t, "restriction") {
            self.restriction_simple_content(tns, doc, t, cur, true)?;
        } else if is_xsd(doc, t, "list") {
            self.sdl.ty_mut(cur).kind = TypeKind::List;
            self.list(tns, doc, t, cur)?;
        } else if is_xsd(doc, t, "union") {
            self.sdl.ty_mut(cur).kind = TypeKind::Union;
            self.union(tns, doc, t, cur)?;
        } else {
            bail!(
                "Parsing Schema: unexpected <{}> in simpleType",
                lossy(doc.name(t))
            );
        }
        if let Some(t2) = doc.next(t) {
            bail!(
                "Parsing Schema: unexpected <{}> in simpleType",
                lossy(doc.name(t2))
            );
        }
        Ok(())
    }

    fn push_element(&mut self, cur: TypeId, t: TypeId) {
        self.sdl
            .ty_mut(cur)
            .elements
            .get_or_insert_with(ZHash::new)
            .push(t);
    }

    /// `schema_list`.
    fn list(&mut self, tns: &str, doc: &Doc, node: NodeId, cur: TypeId) -> SoapResult<()> {
        let item_type = doc.attr_value(node, "itemType", None).map(<[u8]>::to_vec);
        if let Some(it) = &item_type {
            if let Some((href, local)) = resolve_qname(doc, node, it) {
                let new_type = self.sdl.new_type(SdlType {
                    name: Some(local.clone()),
                    namens: Some(href.clone()),
                    ..SdlType::default()
                });
                let e = get_create_encoder(&mut self.sdl, Some(new_type), &href, &local);
                self.sdl.ty_mut(new_type).encode = Some(e);
                self.push_element(cur, new_type);
            }
        }
        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        if let Some(t) = trav.filter(|&t| is_xsd(doc, t, "simpleType")) {
            if item_type.is_some() {
                bail!("Parsing Schema: element has both 'itemType' attribute and subtype");
            }
            let new_type = self.sdl.new_type(SdlType {
                name: Some(anonymous_name(&self.sdl)),
                namens: Some(tns.to_owned()),
                ..SdlType::default()
            });
            self.push_element(cur, new_type);
            self.simple_type(tns, doc, t, Some(new_type))?;
            trav = doc.next(t);
        }
        if let Some(t) = trav {
            bail!(
                "Parsing Schema: unexpected <{}> in list",
                lossy(doc.name(t))
            );
        }
        Ok(())
    }

    /// `schema_union`.
    fn union(&mut self, tns: &str, doc: &Doc, node: NodeId, cur: TypeId) -> SoapResult<()> {
        if let Some(mt) = doc.attr_value(node, "memberTypes", None) {
            let collapsed = whitespace_collapse(mt);
            for item in collapsed.split(|&c| c == b' ').filter(|s| !s.is_empty()) {
                if let Some((href, local)) = resolve_qname(doc, node, item) {
                    let new_type = self.sdl.new_type(SdlType {
                        name: Some(local.clone()),
                        namens: Some(href.clone()),
                        ..SdlType::default()
                    });
                    let e = get_create_encoder(&mut self.sdl, Some(new_type), &href, &local);
                    self.sdl.ty_mut(new_type).encode = Some(e);
                    self.push_element(cur, new_type);
                }
            }
        }
        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        while let Some(t) = trav {
            if !is_xsd(doc, t, "simpleType") {
                bail!(
                    "Parsing Schema: unexpected <{}> in union",
                    lossy(doc.name(t))
                );
            }
            let new_type = self.sdl.new_type(SdlType {
                name: Some(anonymous_name(&self.sdl)),
                namens: Some(tns.to_owned()),
                ..SdlType::default()
            });
            self.push_element(cur, new_type);
            self.simple_type(tns, doc, t, Some(new_type))?;
            trav = doc.next(t);
        }
        Ok(())
    }

    /// `schema_simpleContent`.
    fn simple_content(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur: TypeId,
    ) -> SoapResult<()> {
        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        let Some(t) = trav else {
            bail!("Parsing Schema: expected <restriction> or <extension> in simpleContent");
        };
        if is_xsd(doc, t, "restriction") {
            self.sdl.ty_mut(cur).kind = TypeKind::Restriction;
            self.restriction_simple_content(tns, doc, t, cur, false)?;
        } else if is_xsd(doc, t, "extension") {
            self.sdl.ty_mut(cur).kind = TypeKind::Extension;
            self.extension_simple_content(tns, doc, t, cur)?;
        } else {
            bail!(
                "Parsing Schema: unexpected <{}> in simpleContent",
                lossy(doc.name(t))
            );
        }
        if let Some(t2) = doc.next(t) {
            bail!(
                "Parsing Schema: unexpected <{}> in simpleContent",
                lossy(doc.name(t2))
            );
        }
        Ok(())
    }

    fn set_base_encoder(&mut self, doc: &Doc, node: NodeId, cur: TypeId, base: &[u8]) {
        if let Some((href, local)) = resolve_qname(doc, node, base) {
            let e = get_create_encoder(&mut self.sdl, Some(cur), &href, &local);
            self.sdl.ty_mut(cur).encode = Some(e);
        }
    }

    /// `schema_restriction_simpleContent`.
    fn restriction_simple_content(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur: TypeId,
        simple_type: bool,
    ) -> SoapResult<()> {
        if let Some(base) = doc.attr_value(node, "base", None) {
            self.set_base_encoder(doc, node, cur, base);
        } else if !simple_type {
            bail!("Parsing Schema: restriction has no 'base' attribute");
        }
        if self.sdl.ty(cur).restrictions.is_none() {
            self.sdl.ty_mut(cur).restrictions = Some(Restrictions::default());
        }
        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        if let Some(t) = trav.filter(|&t| is_xsd(doc, t, "simpleType")) {
            self.simple_type(tns, doc, t, Some(cur))?;
            trav = doc.next(t);
        }
        while let Some(t) = trav {
            let facet = String::from_utf8_lossy(doc.name(t)).into_owned();
            let int_facet = [
                "minExclusive",
                "minInclusive",
                "maxExclusive",
                "maxInclusive",
                "totalDigits",
                "fractionDigits",
                "length",
                "minLength",
                "maxLength",
            ];
            if int_facet.iter().any(|f| is_xsd(doc, t, f)) {
                let v = restriction_int(doc, t)?;
                let r = self
                    .sdl
                    .ty_mut(cur)
                    .restrictions
                    .get_or_insert_with(Restrictions::default);
                let slot = match facet.as_str() {
                    "minExclusive" => &mut r.min_exclusive,
                    "minInclusive" => &mut r.min_inclusive,
                    "maxExclusive" => &mut r.max_exclusive,
                    "maxInclusive" => &mut r.max_inclusive,
                    "totalDigits" => &mut r.total_digits,
                    "fractionDigits" => &mut r.fraction_digits,
                    "length" => &mut r.length,
                    "minLength" => &mut r.min_length,
                    _ => &mut r.max_length,
                };
                *slot = Some(v);
            } else if is_xsd(doc, t, "whiteSpace") || is_xsd(doc, t, "pattern") {
                let v = restriction_char(doc, t)?;
                let r = self
                    .sdl
                    .ty_mut(cur)
                    .restrictions
                    .get_or_insert_with(Restrictions::default);
                if facet == "whiteSpace" {
                    r.white_space = Some(v);
                } else {
                    r.pattern = Some(v);
                }
            } else if is_xsd(doc, t, "enumeration") {
                let v = restriction_char(doc, t)?;
                let r = self
                    .sdl
                    .ty_mut(cur)
                    .restrictions
                    .get_or_insert_with(Restrictions::default);
                let key = v.value.clone();
                r.enumeration.get_or_insert_with(ZHash::new).add(&key, v);
            } else {
                break;
            }
            trav = doc.next(t);
        }
        if !simple_type {
            while let Some(t) = trav {
                if is_xsd(doc, t, "attribute") {
                    self.attribute(tns, doc, t, Some(cur))?;
                } else if is_xsd(doc, t, "attributeGroup") {
                    self.attribute_group(tns, doc, t, Some(cur))?;
                } else if is_xsd(doc, t, "anyAttribute") {
                    trav = doc.next(t);
                    break;
                } else {
                    bail!(
                        "Parsing Schema: unexpected <{}> in restriction",
                        lossy(doc.name(t))
                    );
                }
                trav = doc.next(t);
            }
        }
        if let Some(t) = trav {
            bail!(
                "Parsing Schema: unexpected <{}> in restriction",
                lossy(doc.name(t))
            );
        }
        Ok(())
    }

    /// The optional `(group | all | choice | sequence)` particle of a
    /// complex type definition; returns the next node.
    #[allow(clippy::option_option)]
    fn particle(
        &mut self,
        tns: &str,
        doc: &Doc,
        t: NodeId,
        cur: TypeId,
    ) -> SoapResult<Option<Option<NodeId>>> {
        // Outer `None`: no particle; inner: the node after it.
        if is_xsd(doc, t, "group") {
            self.group(tns, doc, t, Some(cur), None)?;
        } else if is_xsd(doc, t, "all") {
            let m = self.all(tns, doc, t, Some(cur))?;
            self.sdl.ty_mut(cur).model = Some(m);
        } else if is_xsd(doc, t, "choice") {
            let m = self.choice(tns, doc, t, Some(cur))?;
            self.sdl.ty_mut(cur).model = Some(m);
        } else if is_xsd(doc, t, "sequence") {
            let m = self.sequence(tns, doc, t, Some(cur))?;
            self.sdl.ty_mut(cur).model = Some(m);
        } else {
            return Ok(None);
        }
        Ok(Some(doc.next(t)))
    }

    /// Trailing `(attribute | attributeGroup)*, anyAttribute?`.
    fn attribute_decls(
        &mut self,
        tns: &str,
        doc: &Doc,
        mut trav: Option<NodeId>,
        cur: TypeId,
        ctx_name: &str,
    ) -> SoapResult<()> {
        while let Some(t) = trav {
            if is_xsd(doc, t, "attribute") {
                self.attribute(tns, doc, t, Some(cur))?;
            } else if is_xsd(doc, t, "attributeGroup") {
                self.attribute_group(tns, doc, t, Some(cur))?;
            } else if is_xsd(doc, t, "anyAttribute") {
                trav = doc.next(t);
                break;
            } else {
                bail!(
                    "Parsing Schema: unexpected <{}> in {ctx_name}",
                    lossy(doc.name(t))
                );
            }
            trav = doc.next(t);
        }
        if let Some(t) = trav {
            bail!(
                "Parsing Schema: unexpected <{}> in {ctx_name}",
                lossy(doc.name(t))
            );
        }
        Ok(())
    }

    /// `schema_restriction_complexContent`.
    fn restriction_complex_content(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur: TypeId,
    ) -> SoapResult<()> {
        let Some(base) = doc.attr_value(node, "base", None) else {
            bail!("Parsing Schema: restriction has no 'base' attribute");
        };
        self.set_base_encoder(doc, node, cur, base);
        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        if let Some(t) = trav {
            if let Some(next) = self.particle(tns, doc, t, cur)? {
                trav = next;
            }
        }
        self.attribute_decls(tns, doc, trav, cur, "restriction")
    }

    /// `schema_extension_simpleContent`.
    fn extension_simple_content(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur: TypeId,
    ) -> SoapResult<()> {
        let Some(base) = doc.attr_value(node, "base", None) else {
            bail!("Parsing Schema: extension has no 'base' attribute");
        };
        self.set_base_encoder(doc, node, cur, base);
        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        self.attribute_decls(tns, doc, trav, cur, "extension")
    }

    /// `schema_extension_complexContent`.
    fn extension_complex_content(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur: TypeId,
    ) -> SoapResult<()> {
        let Some(base) = doc.attr_value(node, "base", None) else {
            bail!("Parsing Schema: extension has no 'base' attribute");
        };
        self.set_base_encoder(doc, node, cur, base);
        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        if let Some(t) = trav {
            if let Some(next) = self.particle(tns, doc, t, cur)? {
                trav = next;
            }
        }
        self.attribute_decls(tns, doc, trav, cur, "extension")
    }

    /// `schema_all`.
    fn all(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur: Option<TypeId>,
    ) -> SoapResult<Model> {
        let (min, max) = min_max(doc, node)?;
        let mut content = Vec::new();
        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        while let Some(t) = trav {
            if is_xsd(doc, t, "element") {
                self.element(tns, doc, t, cur, Some(&mut content))?;
            } else {
                bail!("Parsing Schema: unexpected <{}> in all", lossy(doc.name(t)));
            }
            trav = doc.next(t);
        }
        Ok(Model {
            kind: ModelKind::All(content),
            min_occurs: min,
            max_occurs: max,
        })
    }

    /// `schema_group`.
    fn group(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur_type: Option<TypeId>,
        parent: Option<&mut Vec<Model>>,
    ) -> SoapResult<()> {
        let ns = attr(doc, node, "targetNamespace").unwrap_or_else(|| tns.to_owned());
        let name = doc.attr_value(node, "name", None);
        let ref_ = if name.is_none() {
            doc.attr_value(node, "ref", None)
        } else {
            None
        };
        if name.is_none() && ref_.is_none() {
            bail!("Parsing Schema: group has no 'name' nor 'ref' attributes");
        }
        let (mut model, key) = if let Some(r) = ref_ {
            let (prefix, local) = parse_qname(r);
            let mut key = match doc.search_ns(node, prefix) {
                Some(found) => lossy(&found.href),
                None => ns.clone(),
            };
            key.push(':');
            key.push_str(&lossy(local));
            (
                Model {
                    kind: ModelKind::GroupRef(key.clone()),
                    min_occurs: 1,
                    max_occurs: 1,
                },
                key,
            )
        } else {
            (
                Model {
                    kind: ModelKind::Sequence(Vec::new()),
                    min_occurs: 1,
                    max_occurs: 1,
                },
                format!("{ns}:{}", lossy(name.unwrap_or_default())),
            )
        };
        let cur = if let Some(c) = cur_type {
            c
        } else {
            let t = self.sdl.new_type(SdlType::default());
            if !self.sdl.groups.get_or_insert_with(ZHash::new).add(&key, t) {
                bail!("Parsing Schema: group '{key}' already defined");
            }
            t
        };
        let (min, max) = min_max(doc, node)?;
        model.min_occurs = min;
        model.max_occurs = max;

        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        if let Some(t) = trav {
            let inner = if is_xsd(doc, t, "choice") {
                if ref_.is_some() {
                    bail!("Parsing Schema: group has both 'ref' attribute and subcontent");
                }
                let m = self.choice(tns, doc, t, Some(cur))?;
                model.kind = ModelKind::Choice(Vec::new());
                m
            } else if is_xsd(doc, t, "sequence") {
                if ref_.is_some() {
                    bail!("Parsing Schema: group has both 'ref' attribute and subcontent");
                }
                let m = self.sequence(tns, doc, t, Some(cur))?;
                model.kind = ModelKind::Sequence(Vec::new());
                m
            } else if is_xsd(doc, t, "all") {
                if ref_.is_some() {
                    bail!("Parsing Schema: group has both 'ref' attribute and subcontent");
                }
                let m = self.all(tns, doc, t, Some(cur))?;
                model.kind = ModelKind::All(Vec::new());
                m
            } else {
                bail!(
                    "Parsing Schema: unexpected <{}> in group",
                    lossy(doc.name(t))
                );
            };
            if let Some(c) = model.content_mut() {
                c.push(inner);
            }
            if let Some(t2) = doc.next(t) {
                bail!(
                    "Parsing Schema: unexpected <{}> in group",
                    lossy(doc.name(t2))
                );
            }
        }
        match parent {
            Some(p) => p.push(model),
            None => self.sdl.ty_mut(cur).model = Some(model),
        }
        Ok(())
    }

    /// Children of `choice`/`sequence`.
    fn nested_particles(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur: Option<TypeId>,
        content: &mut Vec<Model>,
        what: &str,
    ) -> SoapResult<()> {
        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        while let Some(t) = trav {
            if is_xsd(doc, t, "element") {
                self.element(tns, doc, t, cur, Some(content))?;
            } else if is_xsd(doc, t, "group") {
                self.group(tns, doc, t, cur, Some(content))?;
            } else if is_xsd(doc, t, "choice") {
                let m = self.choice(tns, doc, t, cur)?;
                content.push(m);
            } else if is_xsd(doc, t, "sequence") {
                let m = self.sequence(tns, doc, t, cur)?;
                content.push(m);
            } else if is_xsd(doc, t, "any") {
                let (min, max) = min_max(doc, t)?;
                content.push(Model {
                    kind: ModelKind::Any,
                    min_occurs: min,
                    max_occurs: max,
                });
            } else {
                bail!(
                    "Parsing Schema: unexpected <{}> in {what}",
                    lossy(doc.name(t))
                );
            }
            trav = doc.next(t);
        }
        Ok(())
    }

    /// `schema_choice`.
    fn choice(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur: Option<TypeId>,
    ) -> SoapResult<Model> {
        let (min, max) = min_max(doc, node)?;
        let mut content = Vec::new();
        self.nested_particles(tns, doc, node, cur, &mut content, "choice")?;
        Ok(Model {
            kind: ModelKind::Choice(content),
            min_occurs: min,
            max_occurs: max,
        })
    }

    /// `schema_sequence`.
    fn sequence(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur: Option<TypeId>,
    ) -> SoapResult<Model> {
        let (min, max) = min_max(doc, node)?;
        let mut content = Vec::new();
        self.nested_particles(tns, doc, node, cur, &mut content, "sequence")?;
        Ok(Model {
            kind: ModelKind::Sequence(content),
            min_occurs: min,
            max_occurs: max,
        })
    }

    /// `schema_complexContent`.
    fn complex_content(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur: TypeId,
    ) -> SoapResult<()> {
        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        let Some(t) = trav else {
            bail!("Parsing Schema: <restriction> or <extension> expected in complexContent");
        };
        if is_xsd(doc, t, "restriction") {
            self.sdl.ty_mut(cur).kind = TypeKind::Restriction;
            self.restriction_complex_content(tns, doc, t, cur)?;
        } else if is_xsd(doc, t, "extension") {
            self.sdl.ty_mut(cur).kind = TypeKind::Extension;
            self.extension_complex_content(tns, doc, t, cur)?;
        } else {
            bail!(
                "Parsing Schema: unexpected <{}> in complexContent",
                lossy(doc.name(t))
            );
        }
        if let Some(t2) = doc.next(t) {
            bail!(
                "Parsing Schema: unexpected <{}> in complexContent",
                lossy(doc.name(t2))
            );
        }
        Ok(())
    }

    /// `schema_complexType`.
    fn complex_type(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur_type: Option<TypeId>,
    ) -> SoapResult<()> {
        let ns = attr(doc, node, "targetNamespace").unwrap_or_else(|| tns.to_owned());
        let name = attr(doc, node, "name");
        let cur = if let Some(ct) = cur_type {
            let (n, nns) = match &name {
                Some(n) => (Some(n.clone()), Some(ns.clone())),
                None => (self.sdl.ty(ct).name.clone(), self.sdl.ty(ct).namens.clone()),
            };
            let new_type = self.sdl.new_type(SdlType {
                kind: TypeKind::Complex,
                name: n.clone(),
                namens: nns.clone(),
                ..SdlType::default()
            });
            self.sdl.types.get_or_insert_with(Vec::new).push(new_type);
            let mut enc = Encoder::sdl(
                nns.as_deref().unwrap_or(""),
                n.as_deref().unwrap_or(""),
                Some(new_type),
            );
            if nns.is_none() {
                enc.ns = None;
            }
            let e = self.sdl.new_encoder(enc);
            self.sdl.encoders.push(e);
            self.sdl.ty_mut(ct).encode = Some(e);
            new_type
        } else if let Some(name) = &name {
            let new_type = self.sdl.new_type(SdlType {
                kind: TypeKind::Complex,
                name: Some(name.clone()),
                namens: Some(ns.clone()),
                ..SdlType::default()
            });
            self.sdl.types.get_or_insert_with(Vec::new).push(new_type);
            create_encoder(&mut self.sdl, Some(new_type), &ns, name);
            new_type
        } else {
            bail!("Parsing Schema: complexType has no 'name' attribute");
        };

        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        if let Some(t) = trav {
            if is_xsd(doc, t, "simpleContent") {
                self.simple_content(tns, doc, t, cur)?;
                trav = doc.next(t);
            } else if is_xsd(doc, t, "complexContent") {
                self.complex_content(tns, doc, t, cur)?;
                trav = doc.next(t);
            } else {
                if let Some(next) = self.particle(tns, doc, t, cur)? {
                    trav = next;
                }
                return self.attribute_decls(tns, doc, trav, cur, "complexType");
            }
        }
        if let Some(t) = trav {
            bail!(
                "Parsing Schema: unexpected <{}> in complexType",
                lossy(doc.name(t))
            );
        }
        Ok(())
    }

    /// `schema_element`.
    fn element(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur_type: Option<TypeId>,
        model: Option<&mut Vec<Model>>,
    ) -> SoapResult<()> {
        let ns = attr(doc, node, "targetNamespace").unwrap_or_else(|| tns.to_owned());
        let name = doc.attr_value(node, "name", None);
        let ref_ = if name.is_none() {
            doc.attr_value(node, "ref", None)
        } else {
            None
        };
        if name.is_none() && ref_.is_none() {
            bail!("Parsing Schema: element has no 'name' nor 'ref' attributes");
        }
        let mut new_type = SdlType::default();
        if let Some(r) = ref_ {
            let (prefix, local) = parse_qname(r);
            let mut nscat = match doc.search_ns(node, prefix) {
                Some(found) => {
                    new_type.namens = Some(lossy(&found.href));
                    lossy(&found.href)
                }
                None => ns.clone(),
            };
            nscat.push(':');
            nscat.push_str(&lossy(local));
            new_type.name = Some(lossy(local));
            new_type.ref_ = Some(nscat);
        } else {
            new_type.name = name.map(lossy);
            new_type.namens = Some(ns.clone());
        }
        let key_name = new_type.name.clone().unwrap_or_default();
        let key_ns = new_type.namens.clone().unwrap_or_default();
        let id = self.sdl.new_type(new_type);
        if let Some(ct) = cur_type {
            let elements = self.sdl.ty_mut(ct).elements.get_or_insert_with(ZHash::new);
            if !elements.add(&key_name, id) {
                elements.push(id);
            }
        } else {
            let key = format!("{key_ns}:{key_name}");
            if !self
                .sdl
                .elements
                .get_or_insert_with(ZHash::new)
                .add(&key, id)
            {
                bail!("Parsing Schema: element '{key}' already defined");
            }
        }
        if let Some(m) = model {
            let (min, max) = min_max(doc, node)?;
            m.push(Model {
                kind: ModelKind::Element(id),
                min_occurs: min,
                max_occurs: max,
            });
        }
        let cur = id;
        let is_ref = ref_.is_some();

        if let Some(v) = doc.attr_value(node, "nillable", None) {
            if is_ref {
                bail!("Parsing Schema: element has both 'ref' and 'nillable' attributes");
            }
            self.sdl.ty_mut(cur).nillable = v.eq_ignore_ascii_case(b"true") || v == b"1";
        }
        if let Some(v) = attr(doc, node, "fixed") {
            if is_ref {
                bail!("Parsing Schema: element has both 'ref' and 'fixed' attributes");
            }
            self.sdl.ty_mut(cur).fixed = Some(v);
        }
        if let Some(v) = attr(doc, node, "default") {
            if is_ref {
                bail!("Parsing Schema: element has both 'default' and 'fixed' attributes");
            }
            self.sdl.ty_mut(cur).def = Some(v);
        }
        let mut form = match doc.attr_value(node, "form", None) {
            Some(b"qualified") => Form::Qualified,
            Some(b"unqualified") => Form::Unqualified,
            _ => Form::Default,
        };
        if form == Form::Default {
            form = default_form(doc, node, "elementFormDefault");
        }
        self.sdl.ty_mut(cur).form = form;

        let type_attr = doc.attr_value(node, "type", None);
        if let Some(ty) = type_attr {
            if is_ref {
                bail!("Parsing Schema: element has both 'ref' and 'type' attributes");
            }
            if let Some((href, local)) = resolve_qname(doc, node, ty) {
                let e = get_create_encoder(&mut self.sdl, Some(cur), &href, &local);
                self.sdl.ty_mut(cur).encode = Some(e);
            }
        }

        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        if let Some(t) = trav {
            if is_xsd(doc, t, "simpleType") || is_xsd(doc, t, "complexType") {
                if is_ref {
                    bail!("Parsing Schema: element has both 'ref' attribute and subtype");
                } else if type_attr.is_some() {
                    bail!("Parsing Schema: element has both 'type' attribute and subtype");
                }
                if is_xsd(doc, t, "simpleType") {
                    self.simple_type(tns, doc, t, Some(cur))?;
                } else {
                    self.complex_type(tns, doc, t, Some(cur))?;
                }
                trav = doc.next(t);
            }
        }
        while let Some(t) = trav {
            if !(is_xsd(doc, t, "unique") || is_xsd(doc, t, "key") || is_xsd(doc, t, "keyref")) {
                bail!(
                    "Parsing Schema: unexpected <{}> in element",
                    lossy(doc.name(t))
                );
            }
            trav = doc.next(t);
        }
        Ok(())
    }

    /// `schema_attribute`.
    fn attribute(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur_type: Option<TypeId>,
    ) -> SoapResult<()> {
        let name = doc.attr_value(node, "name", None);
        let ref_ = if name.is_none() {
            doc.attr_value(node, "ref", None)
        } else {
            None
        };
        if name.is_none() && ref_.is_none() {
            bail!("Parsing Schema: attribute has no 'name' nor 'ref' attributes");
        }
        let mut new_attr = SdlAttribute::default();
        let key = if let Some(r) = ref_ {
            let (prefix, local) = parse_qname(r);
            let mut key = String::new();
            if let Some(found) = doc.search_ns(node, prefix) {
                key.push_str(&lossy(&found.href));
                new_attr.namens = Some(lossy(&found.href));
            } else {
                key.push_str(&attr(doc, node, "targetNamespace").unwrap_or_else(|| tns.to_owned()));
            }
            key.push(':');
            key.push_str(&lossy(local));
            new_attr.ref_ = Some(key.clone());
            key
        } else {
            let ns = attr(doc, node, "targetNamespace").unwrap_or_else(|| tns.to_owned());
            new_attr.namens = Some(ns.clone());
            format!("{ns}:{}", lossy(name.unwrap_or_default()))
        };

        let type_attr = doc.attr_value(node, "type", None);
        if let Some(ty) = type_attr {
            if ref_.is_some() {
                bail!("Parsing Schema: attribute has both 'ref' and 'type' attributes");
            }
            if let Some((href, local)) = resolve_qname(doc, node, ty) {
                new_attr.encode = Some(get_create_encoder(&mut self.sdl, cur_type, &href, &local));
            }
        }

        for a in doc.attrs(node) {
            if a.ns.is_none() {
                match a.name.as_slice() {
                    b"default" => new_attr.def = Some(lossy(&a.value)),
                    b"fixed" => new_attr.fixed = Some(lossy(&a.value)),
                    b"form" => {
                        new_attr.form = match a.value.as_slice() {
                            b"qualified" => Form::Qualified,
                            b"unqualified" => Form::Unqualified,
                            _ => Form::Default,
                        };
                    }
                    b"name" => new_attr.name = Some(lossy(&a.value)),
                    b"use" => {
                        new_attr.use_ = match a.value.as_slice() {
                            b"prohibited" => AttrUse::Prohibited,
                            b"required" => AttrUse::Required,
                            b"optional" => AttrUse::Optional,
                            _ => AttrUse::Default,
                        };
                    }
                    _ => {}
                }
            } else if let Some(ans) = &a.ns {
                if ans.href != XSD_NAMESPACE.as_bytes() {
                    let (prefix, value) = parse_qname(&a.value);
                    let ext = match doc.search_ns(node, prefix) {
                        Some(found) => ExtraAttribute {
                            ns: Some(lossy(&found.href)),
                            val: lossy(value),
                        },
                        None => ExtraAttribute {
                            ns: None,
                            val: lossy(&a.value),
                        },
                    };
                    let k = format!("{}:{}", lossy(&ans.href), lossy(&a.name));
                    new_attr.extra.get_or_insert_with(ZHash::new).add(&k, ext);
                }
            }
        }
        if new_attr.form == Form::Default {
            new_attr.form = default_form(doc, node, "attributeFormDefault");
        }

        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        let mut pending_subtype = None;
        if let Some(t) = trav.filter(|&t| is_xsd(doc, t, "simpleType")) {
            if ref_.is_some() {
                bail!("Parsing Schema: attribute has both 'ref' attribute and subtype");
            } else if type_attr.is_some() {
                bail!("Parsing Schema: attribute has both 'type' attribute and subtype");
            }
            pending_subtype = Some(t);
            trav = doc.next(t);
        }
        // Register the attribute before parsing the anonymous type (C
        // inserts it into the table first).
        let table = match cur_type {
            None => self.attributes.get_or_insert_with(ZHash::new),
            Some(ct) => self
                .sdl
                .ty_mut(ct)
                .attributes
                .get_or_insert_with(ZHash::new),
        };
        if table.contains(&key) {
            bail!("Parsing Schema: attribute '{key}' already defined");
        }
        table.add(&key, new_attr);
        if let Some(t) = pending_subtype {
            let dummy = self.sdl.new_type(SdlType {
                name: Some(anonymous_name(&self.sdl)),
                namens: Some(tns.to_owned()),
                ..SdlType::default()
            });
            self.simple_type(tns, doc, t, Some(dummy))?;
            let enc = self.sdl.ty(dummy).encode;
            let table = match cur_type {
                None => self.attributes.get_or_insert_with(ZHash::new),
                Some(ct) => self
                    .sdl
                    .ty_mut(ct)
                    .attributes
                    .get_or_insert_with(ZHash::new),
            };
            if let Some(a) = table.get_mut(&key) {
                a.encode = enc;
            }
        }
        if let Some(t) = trav {
            bail!(
                "Parsing Schema: unexpected <{}> in attribute",
                lossy(doc.name(t))
            );
        }
        Ok(())
    }

    /// `schema_attributeGroup`.
    fn attribute_group(
        &mut self,
        tns: &str,
        doc: &Doc,
        node: NodeId,
        cur_type: Option<TypeId>,
    ) -> SoapResult<()> {
        let name = doc.attr_value(node, "name", None);
        let ref_ = if name.is_none() {
            doc.attr_value(node, "ref", None)
        } else {
            None
        };
        if name.is_none() && ref_.is_none() {
            bail!("Parsing Schema: attributeGroup has no 'name' nor 'ref' attributes");
        }
        let mut target = cur_type;
        if cur_type.is_none() {
            let ns = attr(doc, node, "targetNamespace").unwrap_or_else(|| tns.to_owned());
            let n = lossy(name.or(ref_).unwrap_or_default());
            let key = format!("{ns}:{n}");
            let t = self.sdl.new_type(SdlType {
                name: Some(n),
                namens: Some(ns),
                ..SdlType::default()
            });
            if !self
                .attribute_groups
                .get_or_insert_with(ZHash::new)
                .add(&key, t)
            {
                bail!("Parsing Schema: attributeGroup '{key}' already defined");
            }
            target = Some(t);
        } else if let Some(r) = ref_ {
            let (prefix, local) = parse_qname(r);
            let mut key = String::new();
            if let Some(found) = doc.search_ns(node, prefix) {
                key.push_str(&lossy(&found.href));
            }
            key.push(':');
            key.push_str(&lossy(local));
            if let Some(ct) = cur_type {
                self.sdl
                    .ty_mut(ct)
                    .attributes
                    .get_or_insert_with(ZHash::new)
                    .push(SdlAttribute {
                        ref_: Some(key),
                        ..SdlAttribute::default()
                    });
            }
            target = None;
        }

        let mut trav = doc.first_child(node);
        if trav.is_some_and(|t| is_xsd(doc, t, "annotation")) {
            trav = trav.and_then(|t| doc.next(t));
        }
        while let Some(t) = trav {
            if is_xsd(doc, t, "attribute")
                || is_xsd(doc, t, "attributeGroup")
                || is_xsd(doc, t, "anyAttribute")
            {
                if ref_.is_some() {
                    bail!(
                        "Parsing Schema: attributeGroup has both 'ref' attribute and subattribute"
                    );
                }
                if is_xsd(doc, t, "attribute") {
                    self.attribute(tns, doc, t, target)?;
                } else if is_xsd(doc, t, "attributeGroup") {
                    self.attribute_group(tns, doc, t, target)?;
                } else {
                    trav = doc.next(t);
                    break;
                }
            } else {
                bail!(
                    "Parsing Schema: unexpected <{}> in attributeGroup",
                    lossy(doc.name(t))
                );
            }
            trav = doc.next(t);
        }
        if let Some(t) = trav {
            bail!(
                "Parsing Schema: unexpected <{}> in attributeGroup",
                lossy(doc.name(t))
            );
        }
        Ok(())
    }

    // ---- pass 2 ---------------------------------------------------------------

    /// `schema_pass2`.
    pub fn schema_pass2(&mut self) -> SoapResult<()> {
        if let Some(mut attrs) = self.attributes.take() {
            // The fixup of a global attribute reads the other global
            // attributes: run it against a snapshot.
            let snapshot = attrs.clone();
            for a in attrs.values_mut() {
                Self::attribute_fixup_with(&snapshot, a);
            }
            self.attributes = Some(attrs);
        }
        let groups: Vec<TypeId> = self
            .attribute_groups
            .as_ref()
            .map(|g| g.values().copied().collect())
            .unwrap_or_default();
        for t in groups {
            self.type_fixup(t)?;
        }
        let elements: Vec<TypeId> = self
            .sdl
            .elements
            .as_ref()
            .map(|g| g.values().copied().collect())
            .unwrap_or_default();
        for t in elements {
            self.type_fixup(t)?;
        }
        let groups: Vec<TypeId> = self
            .sdl
            .groups
            .as_ref()
            .map(|g| g.values().copied().collect())
            .unwrap_or_default();
        for t in groups {
            self.type_fixup(t)?;
        }
        let types: Vec<TypeId> = self.sdl.types.clone().unwrap_or_default();
        for t in types {
            self.type_fixup(t)?;
        }
        self.attributes = None;
        self.attribute_groups = None;
        Ok(())
    }

    /// `schema_attribute_fixup` against the global attribute table.
    fn attribute_fixup_with(globals: &ZHash<SdlAttribute>, attr: &mut SdlAttribute) {
        let Some(r) = attr.ref_.clone() else {
            return;
        };
        if let Some(tmp) = find_by_ref(globals, &r) {
            let mut tmp = tmp.clone();
            Self::attribute_fixup_with(globals, &mut tmp);
            if attr.name.is_none() {
                attr.name.clone_from(&tmp.name);
            }
            if attr.namens.is_none() {
                attr.namens.clone_from(&tmp.namens);
            }
            if attr.def.is_none() {
                attr.def.clone_from(&tmp.def);
            }
            if attr.fixed.is_none() {
                attr.fixed.clone_from(&tmp.fixed);
            }
            if attr.form == Form::Default {
                attr.form = tmp.form;
            }
            if attr.use_ == AttrUse::Default {
                attr.use_ = tmp.use_;
            }
            if tmp.extra.is_some() {
                attr.extra.clone_from(&tmp.extra);
            }
            attr.encode = tmp.encode;
        }
        if attr.name.is_none() {
            attr.name = Some(match r.rfind(':') {
                Some(p) => r[p + 1..].to_owned(),
                None => r.clone(),
            });
        }
        attr.ref_ = None;
    }

    fn global_attributes(&self) -> ZHash<SdlAttribute> {
        self.attributes.clone().unwrap_or_default()
    }

    /// `schema_attributegroup_fixup`: copies the attributes of the referenced
    /// group into `out`.
    fn attribute_group_fixup(
        &mut self,
        ref_: &str,
        out: &mut Vec<(String, SdlAttribute)>,
    ) -> SoapResult<()> {
        let Some(groups) = self.attribute_groups.as_ref() else {
            return Ok(());
        };
        let Some(&tmp) = find_by_ref(groups, ref_) else {
            return Ok(());
        };
        if self.fixup_in_progress.contains(&tmp) {
            bail!("Parsing Schema: recursive attributeGroup 'ref' attribute '{ref_}'");
        }
        self.fixup_in_progress.insert(tmp);
        let globals = self.global_attributes();
        let mut pos = 0;
        loop {
            let entry = self.sdl.ty(tmp).attributes.as_ref().and_then(|a| {
                // next live slot at or after pos
                (pos..a.slots()).find_map(|p| a.slot(p).map(|(k, v)| (p, k.clone(), v.clone())))
            });
            let Some((p, key, mut a)) = entry else {
                break;
            };
            pos = p + 1;
            match key {
                ZKey::Str(k) => {
                    Self::attribute_fixup_with(&globals, &mut a);
                    if let Some(slot) = self
                        .sdl
                        .ty_mut(tmp)
                        .attributes
                        .as_mut()
                        .and_then(|t| t.get_mut(&k))
                    {
                        slot.clone_from(&a);
                    }
                    out.push((k, a));
                }
                ZKey::Int(i) => {
                    if let Some(r) = a.ref_.clone() {
                        self.attribute_group_fixup(&r, out)?;
                    }
                    if let Some(t) = self.sdl.ty_mut(tmp).attributes.as_mut() {
                        t.remove(&ZKey::Int(i));
                    }
                }
            }
        }
        self.fixup_in_progress.remove(&tmp);
        Ok(())
    }

    /// `schema_content_model_fixup`.
    fn model_fixup(&mut self, model: &mut Model) -> SoapResult<()> {
        match &mut model.kind {
            ModelKind::GroupRef(r) => {
                let r = r.clone();
                let Some(tmp) = self.sdl.groups.as_ref().and_then(|g| g.get(&r)).copied() else {
                    bail!("Parsing Schema: unresolved group 'ref' attribute '{r}'");
                };
                if self.fixup_in_progress.contains(&tmp) {
                    bail!("Parsing Schema: recursive group 'ref' attribute '{r}'");
                }
                self.type_fixup(tmp)?;
                model.kind = ModelKind::Group(tmp);
            }
            ModelKind::Choice(_) | ModelKind::Sequence(_) | ModelKind::All(_) => {
                if let ModelKind::Choice(content) = &mut model.kind {
                    if model.max_occurs != 1 {
                        for m in content.iter_mut() {
                            m.min_occurs = 0;
                            m.max_occurs = model.max_occurs;
                        }
                        let content = std::mem::take(content);
                        model.kind = ModelKind::All(content);
                        model.min_occurs = 1;
                        model.max_occurs = 1;
                    }
                }
                if let Some(content) = model.content_mut() {
                    for m in content.iter_mut() {
                        self.model_fixup(m)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// `schema_type_fixup`.
    fn type_fixup(&mut self, t: TypeId) -> SoapResult<()> {
        self.fixup_in_progress.insert(t);
        if let Some(r) = self.sdl.ty(t).ref_.clone() {
            if let Some(elements) = self.sdl.elements.as_ref() {
                if let Some(&tmp) = find_by_ref(elements, &r) {
                    let src = self.sdl.ty(tmp).clone();
                    let ty = self.sdl.ty_mut(t);
                    ty.kind = src.kind;
                    ty.encode = src.encode;
                    if src.nillable {
                        ty.nillable = true;
                    }
                    if src.fixed.is_some() {
                        ty.fixed = src.fixed;
                    }
                    if src.def.is_some() {
                        ty.def = src.def;
                    }
                    ty.form = src.form;
                } else if r == format!("{XSD_NAMESPACE}:schema") {
                    self.sdl.ty_mut(t).encode = Some(conv(XSD_ANYXML));
                } else {
                    bail!("Parsing Schema: unresolved element 'ref' attribute '{r}'");
                }
            }
            self.sdl.ty_mut(t).ref_ = None;
        }
        let elements: Vec<TypeId> = self
            .sdl
            .ty(t)
            .elements
            .as_ref()
            .map(|e| e.values().copied().collect())
            .unwrap_or_default();
        for e in elements {
            self.type_fixup(e)?;
        }
        if let Some(mut model) = self.sdl.ty_mut(t).model.take() {
            let r = self.model_fixup(&mut model);
            self.sdl.ty_mut(t).model = Some(model);
            r?;
        }
        if self.sdl.ty(t).attributes.is_some() {
            let globals = self.global_attributes();
            let mut pos = 0;
            loop {
                let entry = self.sdl.ty(t).attributes.as_ref().and_then(|a| {
                    (pos..a.slots()).find_map(|p| a.slot(p).map(|(k, v)| (p, k.clone(), v.clone())))
                });
                let Some((p, key, mut a)) = entry else {
                    break;
                };
                pos = p + 1;
                match key {
                    ZKey::Str(k) => {
                        Self::attribute_fixup_with(&globals, &mut a);
                        if let Some(slot) = self
                            .sdl
                            .ty_mut(t)
                            .attributes
                            .as_mut()
                            .and_then(|tb| tb.get_mut(&k))
                        {
                            *slot = a;
                        }
                    }
                    ZKey::Int(i) => {
                        let mut copied = Vec::new();
                        if let Some(r) = a.ref_.clone() {
                            self.attribute_group_fixup(&r, &mut copied)?;
                        }
                        if let Some(tb) = self.sdl.ty_mut(t).attributes.as_mut() {
                            for (k, v) in copied {
                                tb.add(&k, v);
                            }
                            tb.remove(&ZKey::Int(i));
                        }
                    }
                }
            }
        }
        self.fixup_in_progress.remove(&t);
        Ok(())
    }
}

/// `schema_find_by_ref`: exact key, then the `:local` suffix.
fn find_by_ref<'a, V>(ht: &'a ZHash<V>, r: &str) -> Option<&'a V> {
    ht.get(r)
        .or_else(|| r.rfind(':').and_then(|p| ht.get(&r[p..])))
}

/// `schema_restriction_var_int`.
fn restriction_int(doc: &Doc, node: NodeId) -> SoapResult<RestrictionInt> {
    let fixed = matches!(doc.attr_value(node, "fixed", None), Some(b"true" | b"1"));
    let Some(value) = doc.attr_value(node, "value", None) else {
        bail!("Parsing Schema: missing restriction value");
    };
    let name = lossy(doc.name(node));
    Ok(RestrictionInt {
        value: parse_int(value, &name, true)?,
        fixed,
    })
}

/// `schema_restriction_var_char`.
fn restriction_char(doc: &Doc, node: NodeId) -> SoapResult<RestrictionChar> {
    let fixed = matches!(doc.attr_value(node, "fixed", None), Some(b"true" | b"1"));
    let Some(value) = doc.attr_value(node, "value", None) else {
        bail!("Parsing Schema: missing restriction value");
    };
    Ok(RestrictionChar {
        value: lossy(value),
        fixed,
    })
}

/// `schema_min_max`.
fn min_max(doc: &Doc, node: NodeId) -> SoapResult<(i32, i32)> {
    let min = match doc.attr_value(node, "minOccurs", None) {
        Some(v) => parse_int(v, "minOccurs", false)?,
        None => 1,
    };
    let max = match doc.attr_value(node, "maxOccurs", None) {
        Some(b"unbounded") => -1,
        Some(v) => parse_int(v, "maxOccurs", false)?,
        None => 1,
    };
    Ok((min, max))
}

/// The `elementFormDefault`/`attributeFormDefault` of the enclosing schema.
fn default_form(doc: &Doc, node: NodeId, attr_name: &str) -> Form {
    let mut parent = doc.parent(node);
    while let Some(p) = parent {
        if is_xsd(doc, p, "schema") {
            return if doc.attr_value(p, attr_name, None) == Some(b"qualified") {
                Form::Qualified
            } else {
                Form::Unqualified
            };
        }
        parent = doc.parent(p);
    }
    Form::Unqualified
}

/// The mutations `load_schema` applies to a schema element lacking a
/// `targetNamespace`: an empty one is set and an empty default namespace
/// is declared.
pub fn prepare_schema(doc: &mut Doc, schema: NodeId) {
    if doc.attr_value(schema, "targetNamespace", None).is_none() {
        doc.set_prop(schema, b"targetNamespace", b"");
        doc.new_ns(schema, b"", None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_building() {
        assert_eq!(build_uri("b.xsd", Some("/tmp/x/a.wsdl")), "/tmp/x/b.xsd");
        assert_eq!(build_uri("../b.xsd", Some("/tmp/x/a.wsdl")), "/tmp/b.xsd");
        assert_eq!(
            build_uri("b.xsd", Some("http://h/p/a.wsdl")),
            "http://h/p/b.xsd"
        );
        assert_eq!(
            build_uri("/b.xsd", Some("http://h/p/a.wsdl")),
            "http://h/b.xsd"
        );
        assert_eq!(build_uri("http://o/x", Some("/tmp/a")), "http://o/x");
        assert_eq!(build_uri("b.xsd", Some("a.wsdl")), "b.xsd");
        assert_eq!(build_uri("b.xsd", None), "b.xsd");
    }

    #[test]
    fn int_parsing() {
        assert_eq!(parse_int(b"5", "x", false).unwrap(), 5);
        assert_eq!(parse_int(b"1.5", "x", false).unwrap(), 1);
        assert!(parse_int(b"-1", "minOccurs", false).is_err());
        assert!(parse_int(b"99999999999", "maxOccurs", false).is_err());
        assert!(parse_int(b"99999999999999999999999", "maxOccurs", false).is_err());
        assert_eq!(parse_int(b"abc", "x", false).unwrap(), 0);
    }
}
