// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `SimpleXML` provider of XML documents for `XSLTProcessor`
//! (`php_ext_xsl::access::DomAccess`): `importStylesheet()`/`transformTo*()`
//! accept `SimpleXMLElement`s and `transformToDoc($doc, $returnClass)`
//! can produce one.

use std::rc::Rc;

use php_ext_xsl::access::{DomAccess, ImportedNode, XmlSource};
use php_runtime::{Obj, Value};
use php_simplexml::Sxe;
use php_vm::class::Class;
use php_vm::{Vm, VmResult};
use php_xsl::ResultDocument;

use crate::hooks::direct;
use crate::host::sxe_of;

/// The provider (registered at module startup).
#[derive(Debug, Clone, Copy, Default)]
pub struct SimpleXmlAccess;

impl DomAccess for SimpleXmlAccess {
    fn name(&self) -> &'static str {
        "simplexml"
    }

    fn root_classes(&self) -> &'static [&'static str] {
        &["SimpleXMLElement"]
    }

    fn import(&self, vm: &mut Vm, obj: &Obj) -> VmResult<Option<ImportedNode>> {
        let Some(st) = sxe_of(obj) else {
            return Ok(None);
        };
        let node = direct(vm, |_| Sxe::export_node(&st))?;
        Ok(Some(match node {
            Some(n) => ImportedNode::Node(XmlSource::Node(n)),
            None => ImportedNode::Detached,
        }))
    }

    fn wrap_document(
        &self,
        vm: &mut Vm,
        doc: ResultDocument,
        class: Option<&Rc<Class>>,
    ) -> VmResult<Value> {
        let doc = doc.into_document();
        let class = class.map(|c| c.name.clone());
        // PHP attaches the object to the result document node itself.
        direct(vm, |h| Sxe::from_document_node(h, doc, class.as_ref()))
    }
}

/// The provider instance.
pub static ACCESS: SimpleXmlAccess = SimpleXmlAccess;
