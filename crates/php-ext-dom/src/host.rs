// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! [`DomHost`] for the VM: class queries, `XPath` PHP-function callbacks
//! and stream I/O. Every php-dom call runs inside a bridge scope
//! ([`crate::run`]), so the VM is reachable through
//! [`bridge::with_vm`].

use std::io::{Read, Write};
use std::rc::Rc;

use php_dom::{DomError, DomHost, HostCallable, HostStream, Node, XPathArg, XPathReturn};
use php_ext_streams::bridge;
use php_runtime::{Value, ZArray};
use php_streams::stream::Stream;
use php_streams::wrapper::REPORT_ERRORS;
use php_vm::ext::Throw;
use php_vm::Vm;

use crate::state::{bytes, dom_object, node_object};

/// The VM-backed host.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct VmHost;

/// A stream read by php-dom (closed when dropped).
struct StreamReader(Stream);

impl Read for StreamReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0
            .read(buf)
            .map_err(|_| std::io::Error::other("stream read failed"))
    }
}

impl Drop for StreamReader {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// A stream written by php-dom (closed when dropped).
struct StreamWriter(Stream);

impl Write for StreamWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .write(buf)
            .map_err(|_| std::io::Error::other("stream write failed"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush();
        Ok(())
    }
}

impl Drop for StreamWriter {
    fn drop(&mut self) {
        self.0.close();
    }
}

fn host_error(t: Throw) -> DomError {
    DomError::Host(Box::new(t))
}

/// Converts callback arguments to PHP values.
fn args_to_values(vm: &mut Vm, args: Vec<XPathArg>) -> Result<Vec<Value>, Throw> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        out.push(match a {
            XPathArg::Str(s) => bytes(s),
            XPathArg::Bool(b) => Value::Bool(b),
            XPathArg::Float(f) => Value::Float(f),
            XPathArg::Nodes(ns) => {
                let mut vals = Vec::with_capacity(ns.len());
                for n in &ns {
                    vals.push(Value::Object(node_object(vm, n)?));
                }
                Value::Array(ZArray::from_values(vals))
            }
        });
    }
    Ok(out)
}

/// Converts a callback's return value.
fn ret_from_value(vm: &mut Vm, v: &Value) -> Result<XPathReturn, Throw> {
    Ok(match v.deref() {
        Value::Object(o) => match dom_object(&o) {
            Some(php_dom::dispatch::DomObject::Node(n)) => XPathReturn::Node(n),
            _ => XPathReturn::NonNodeObject,
        },
        Value::Bool(b) => XPathReturn::Bool(b),
        other => XPathReturn::Str(vm.value_to_string(&other)?.as_bytes().to_vec()),
    })
}

fn call_with(callable: &Value, args: Vec<XPathArg>) -> Result<XPathReturn, DomError> {
    let r = bridge::with_vm(|vm| -> Result<XPathReturn, Throw> {
        crate::run::flush(vm)?;
        let vals = args_to_values(vm, args)?;
        let ret = vm.call_value(callable, vals)?;
        ret_from_value(vm, &ret)
    });
    match r {
        Some(Ok(v)) => Ok(v),
        Some(Err(t)) => Err(host_error(t)),
        None => Ok(XPathReturn::Undef),
    }
}

impl DomHost for VmHost {
    fn is_subclass_of(&self, class: &str, base: &str) -> bool {
        bridge::with_vm(|vm| {
            match (
                vm.find_class(class.as_bytes()),
                vm.find_class(base.as_bytes()),
            ) {
                (Some(c), Some(b)) => c.is_subclass_of(&b),
                _ => false,
            }
        })
        .unwrap_or_else(|| php_dom::classes::is_subclass(class, base))
    }

    fn is_abstract(&self, class: &str) -> bool {
        bridge::with_vm(|vm| {
            vm.find_class(class.as_bytes())
                .is_some_and(|c| c.is_abstract() || c.is_interface())
        })
        .unwrap_or(false)
    }

    fn canonical_name(&self, class: &str) -> Option<String> {
        bridge::with_vm(|vm| {
            vm.find_class(class.as_bytes())
                .map(|c| php_vm::object::display_name(&c.name).to_string_lossy())
        })
        .flatten()
    }

    fn update_class_constants(&self, class: &str) -> Result<(), DomError> {
        let r = bridge::with_vm(|vm| match vm.find_class(class.as_bytes()) {
            Some(c) => vm.update_class_constants(&c),
            None => Ok(()),
        });
        match r {
            Some(Err(t)) => Err(host_error(t)),
            _ => Ok(()),
        }
    }

    fn resolve_callable(&self, name: &[u8]) -> Result<HostCallable, String> {
        let v = bytes(name.to_vec());
        bridge::with_vm(|vm| {
            if vm.is_callable(&v) {
                Ok(Rc::new(v.clone()) as HostCallable)
            } else {
                Err(vm.callable_error_message(&v))
            }
        })
        .unwrap_or_else(|| Err("no VM".into()))
    }

    fn call(&self, callable: &HostCallable, args: Vec<XPathArg>) -> Result<XPathReturn, DomError> {
        let Some(v) = callable.downcast_ref::<Value>() else {
            return Ok(XPathReturn::Undef);
        };
        call_with(v, args)
    }

    fn call_by_name(&self, name: &[u8], args: Vec<XPathArg>) -> Result<XPathReturn, DomError> {
        let v = bytes(name.to_vec());
        let err = bridge::with_vm(|vm| {
            if vm.is_callable(&v) {
                None
            } else {
                Some(vm.callable_error_message(&v))
            }
        })
        .flatten();
        if let Some(e) = err {
            return Err(DomError::Error(format!(
                "Invalid callback {}, {e}",
                String::from_utf8_lossy(name)
            )));
        }
        call_with(&v, args)
    }

    fn is_dom_node(&self, node: &Node, modern: bool) -> bool {
        node.family_is_modern() == modern
    }

    fn open_read_stream(&self, path: &[u8]) -> Result<Option<HostStream>, DomError> {
        let ctx = php_ext_xml::libxml::streams_context();
        let mut opened = None;
        let Some(stream) =
            php_streams::open::open_wrapper(path, "rb", REPORT_ERRORS, &mut opened, ctx.as_ref())
        else {
            return Ok(None);
        };
        let plain = stream.wrapper.as_ref().is_some_and(|w| w.is_plain_files);
        let plain_file_path = if plain {
            opened.or_else(|| {
                std::fs::canonicalize(String::from_utf8_lossy(path).as_ref())
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned().into_bytes())
            })
        } else {
            None
        };
        // `php_libxml_sniff_charset_from_stream`: HTTP `Content-Type`.
        let charset = php_ext_xml::mime_sniff::sniff_charset_from_stream(&stream);
        Ok(Some(HostStream {
            reader: Box::new(StreamReader(stream)),
            charset,
            plain_file_path,
        }))
    }

    fn open_write_stream(&self, path: &[u8]) -> Result<Option<Box<dyn Write>>, DomError> {
        let ctx = php_ext_xml::libxml::streams_context();
        let mut opened = None;
        Ok(
            php_streams::open::open_wrapper(path, "wb", REPORT_ERRORS, &mut opened, ctx.as_ref())
                .map(|s| Box::new(StreamWriter(s)) as Box<dyn Write>),
        )
    }
}
