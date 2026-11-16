// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/xmlwriter`: the `XMLWriter` class (methods and the static
//! `toMemory()` / `toStream()` / `toUri()` constructors) and the
//! procedural `xmlwriter_*()` functions over `php_libxml::xmlwriter`.
//!
//! Every operation runs inside [`crate::libxml::run`] so output written to
//! PHP streams (URIs opened through the stream hooks, `toStream()`
//! resources, `php://output`) can reach the VM.

use std::io::Write;
use std::rc::Rc;

use php_ext_streams::res;
use php_libxml::xmlwriter::{FlushOutput, NameError, OpenUriError, XmlWriter};
use php_runtime::{Obj, Resource, Value, ZStr};
use php_vm::ext::{acc, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::{object, Vm};

/// The native state of an `XMLWriter` object (`ze_xmlwriter_object`).
#[derive(Default)]
struct WriterSlot(Option<Rc<XmlWriter>>);

fn init_slot(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    object::set_internal(o, WriterSlot::default());
    Ok(())
}

fn set_writer(o: &Obj, w: XmlWriter) {
    object::set_internal(o, WriterSlot(Some(Rc::new(w))));
}

/// The writer of `$this` (`off == 0`) or of argument 0 (`off == 1`).
fn writer_of(vm: &mut Vm, a: &Args, off: usize) -> VmResult<Rc<XmlWriter>> {
    let obj = if off == 0 {
        a.this.clone()
    } else {
        match a.at(0) {
            Value::Object(o) => Some(o),
            _ => None,
        }
    };
    obj.and_then(|o| object::internal::<WriterSlot, _>(&o, |s| s.0.clone()).flatten())
        .ok_or_else(|| vm.throw("Error", "Invalid or uninitialized XMLWriter object"))
}

type OpResult = Result<Value, NameError>;

/// Runs an operation on the writer; maps [`NameError`] to PHP's
/// `ValueError` with the argument number of the calling form.
fn with_writer(
    vm: &mut Vm,
    args: &Args,
    off: usize,
    op: impl FnOnce(&XmlWriter) -> OpResult,
) -> VmResult<Value> {
    let a = args;
    let writer = writer_of(vm, a, off)?;
    match crate::libxml::run(vm, a, || op(&writer))? {
        Ok(v) => Ok(v),
        Err(e) => {
            let n = usize::try_from(e.position).unwrap_or(1) - 1 + off;
            Err(a.value_error(vm, n, &e.detail()))
        }
    }
}

fn s(a: &Args, n: usize) -> Vec<u8> {
    a.str(n).as_bytes().to_vec()
}

fn os(a: &Args, n: usize) -> Option<Vec<u8>> {
    match a.opt(n) {
        Some(Value::Str(z)) => Some(z.as_bytes().to_vec()),
        _ => None,
    }
}

#[allow(clippy::unnecessary_wraps)]
fn b(v: bool) -> OpResult {
    Ok(Value::Bool(v))
}

macro_rules! ops {
    ($( $func:ident / $meth:ident => |$w:ident, $a:ident, $o:ident| $body:expr; )*) => {
        $(
            fn $func(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
                let $a: &Args = a;
                let $o = 1usize;
                with_writer(vm, $a, $o, |$w| $body)
            }
            fn $meth(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
                let $a: &Args = a;
                let $o = 0usize;
                with_writer(vm, $a, $o, |$w| $body)
            }
        )*
    };
}

ops! {
    f_set_indent / m_set_indent => |w, a, o| b(w.set_indent(a.bool(o)));
    f_set_indent_string / m_set_indent_string => |w, a, o| b(w.set_indent_string(&s(a, o)));
    f_start_comment / m_start_comment => |w, a, o| { let _ = (a, o); b(w.start_comment()) };
    f_end_comment / m_end_comment => |w, a, o| { let _ = (a, o); b(w.end_comment()) };
    f_start_attribute / m_start_attribute => |w, a, o| w.start_attribute(&s(a, o)).map(Value::Bool);
    f_end_attribute / m_end_attribute => |w, a, o| { let _ = (a, o); b(w.end_attribute()) };
    f_write_attribute / m_write_attribute => |w, a, o| w.write_attribute(&s(a, o), &s(a, o + 1)).map(Value::Bool);
    f_start_attribute_ns / m_start_attribute_ns => |w, a, o|
        w.start_attribute_ns(os(a, o).as_deref(), &s(a, o + 1), os(a, o + 2).as_deref()).map(Value::Bool);
    f_write_attribute_ns / m_write_attribute_ns => |w, a, o|
        w.write_attribute_ns(os(a, o).as_deref(), &s(a, o + 1), os(a, o + 2).as_deref(), &s(a, o + 3)).map(Value::Bool);
    f_start_element / m_start_element => |w, a, o| w.start_element(&s(a, o)).map(Value::Bool);
    f_end_element / m_end_element => |w, a, o| { let _ = (a, o); b(w.end_element()) };
    f_full_end_element / m_full_end_element => |w, a, o| { let _ = (a, o); b(w.full_end_element()) };
    f_start_element_ns / m_start_element_ns => |w, a, o|
        w.start_element_ns(os(a, o).as_deref(), &s(a, o + 1), os(a, o + 2).as_deref()).map(Value::Bool);
    f_write_element / m_write_element => |w, a, o| w.write_element(&s(a, o), os(a, o + 1).as_deref()).map(Value::Bool);
    f_write_element_ns / m_write_element_ns => |w, a, o|
        w.write_element_ns(os(a, o).as_deref(), &s(a, o + 1), os(a, o + 2).as_deref(), os(a, o + 3).as_deref()).map(Value::Bool);
    f_start_pi / m_start_pi => |w, a, o| w.start_pi(&s(a, o)).map(Value::Bool);
    f_end_pi / m_end_pi => |w, a, o| { let _ = (a, o); b(w.end_pi()) };
    f_write_pi / m_write_pi => |w, a, o| w.write_pi(&s(a, o), &s(a, o + 1)).map(Value::Bool);
    f_start_cdata / m_start_cdata => |w, a, o| { let _ = (a, o); b(w.start_cdata()) };
    f_end_cdata / m_end_cdata => |w, a, o| { let _ = (a, o); b(w.end_cdata()) };
    f_write_cdata / m_write_cdata => |w, a, o| b(w.write_cdata(&s(a, o)));
    f_text / m_text => |w, a, o| b(w.text(&s(a, o)));
    f_write_raw / m_write_raw => |w, a, o| b(w.write_raw(&s(a, o)));
    f_start_document / m_start_document => |w, a, o|
        b(w.start_document(os(a, o).as_deref(), os(a, o + 1).as_deref(), os(a, o + 2).as_deref()));
    f_end_document / m_end_document => |w, a, o| { let _ = (a, o); b(w.end_document()) };
    f_write_comment / m_write_comment => |w, a, o| b(w.write_comment(&s(a, o)));
    f_start_dtd / m_start_dtd => |w, a, o|
        b(w.start_dtd(&s(a, o), os(a, o + 1).as_deref(), os(a, o + 2).as_deref()));
    f_end_dtd / m_end_dtd => |w, a, o| { let _ = (a, o); b(w.end_dtd()) };
    f_write_dtd / m_write_dtd => |w, a, o|
        b(w.write_dtd(&s(a, o), os(a, o + 1).as_deref(), os(a, o + 2).as_deref(), os(a, o + 3).as_deref()));
    f_start_dtd_element / m_start_dtd_element => |w, a, o| w.start_dtd_element(&s(a, o)).map(Value::Bool);
    f_end_dtd_element / m_end_dtd_element => |w, a, o| { let _ = (a, o); b(w.end_dtd_element()) };
    f_write_dtd_element / m_write_dtd_element => |w, a, o| w.write_dtd_element(&s(a, o), &s(a, o + 1)).map(Value::Bool);
    f_start_dtd_attlist / m_start_dtd_attlist => |w, a, o| w.start_dtd_attlist(&s(a, o)).map(Value::Bool);
    f_end_dtd_attlist / m_end_dtd_attlist => |w, a, o| { let _ = (a, o); b(w.end_dtd_attlist()) };
    f_write_dtd_attlist / m_write_dtd_attlist => |w, a, o| w.write_dtd_attlist(&s(a, o), &s(a, o + 1)).map(Value::Bool);
    f_start_dtd_entity / m_start_dtd_entity => |w, a, o| w.start_dtd_entity(&s(a, o), a.bool(o + 1)).map(Value::Bool);
    f_end_dtd_entity / m_end_dtd_entity => |w, a, o| { let _ = (a, o); b(w.end_dtd_entity()) };
    f_write_dtd_entity / m_write_dtd_entity => |w, a, o|
        w.write_dtd_entity(&s(a, o), &s(a, o + 1), a.bool(o + 2), os(a, o + 3).as_deref(), os(a, o + 4).as_deref(), os(a, o + 5).as_deref()).map(Value::Bool);
    f_output_memory / m_output_memory => |w, a, o| Ok(Value::Str(ZStr::from(w.output_memory(a.bool(o)))));
    f_flush / m_flush => |w, a, o| Ok(match w.flush(a.bool(o)) {
        FlushOutput::Bytes(v) => Value::Str(ZStr::from(v)),
        FlushOutput::Written(n) => Value::Int(n),
    });
}

// ---------------------------------------------------------------------
// Opening writers
// ---------------------------------------------------------------------

fn new_writer_object(vm: &mut Vm, w: XmlWriter) -> VmResult<Value> {
    let class = vm
        .find_class(b"XMLWriter")
        .ok_or_else(|| vm.throw("Error", "Class \"XMLWriter\" not found"))?;
    let obj = vm.new_object(&class)?;
    set_writer(&obj, w);
    Ok(Value::Object(obj))
}

/// `xml_writer_create_static`: instantiates the called class through its
/// constructor and attaches the writer.
fn create_static(vm: &mut Vm, a: &Args, w: XmlWriter) -> VmResult<Value> {
    let class = match &a.called {
        Some(c) => Rc::clone(c),
        None => vm
            .find_class(b"XMLWriter")
            .ok_or_else(|| vm.throw("Error", "Class \"XMLWriter\" not found"))?,
    };
    let obj = vm.new_object_with_args(&class, Vec::new())?;
    set_writer(&obj, w);
    Ok(Value::Object(obj))
}

fn check_path(vm: &mut Vm, a: &Args, uri: &[u8]) -> VmResult<()> {
    if uri.contains(&0) {
        return Err(a.value_error(vm, 1, "must not contain any null bytes"));
    }
    if uri.is_empty() {
        return Err(a.value_error(vm, 1, "must not be empty"));
    }
    Ok(())
}

fn cwd() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"))
}

fn open_uri(vm: &mut Vm, a: &Args) -> VmResult<Option<XmlWriter>> {
    let uri = s(a, 0);
    check_path(vm, a, &uri)?;
    let dir = cwd();
    match crate::libxml::run(vm, a, || XmlWriter::open_uri(&uri, &dir))? {
        Ok(w) => Ok(Some(w)),
        Err(OpenUriError::Unresolvable) => {
            vm.php_warning("Unable to resolve file path")?;
            Ok(None)
        }
        Err(OpenUriError::Empty) => Err(a.value_error(vm, 1, "must not be empty")),
        Err(OpenUriError::Failed) => Ok(None),
    }
}

fn f_open_uri(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match open_uri(vm, a)? {
        Some(w) => new_writer_object(vm, w),
        None => Ok(Value::Bool(false)),
    }
}

fn m_open_uri(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    match open_uri(vm, a)? {
        Some(w) => {
            replace_writer(vm, a, &this, w)?;
            Ok(Value::Bool(true))
        }
        None => Ok(Value::Bool(false)),
    }
}

/// Replaces the writer of `this`, dropping (and thus flushing) the old one
/// with the VM reachable.
fn replace_writer(vm: &mut Vm, a: &Args, this: &Obj, w: XmlWriter) -> VmResult<()> {
    let old = object::internal::<WriterSlot, _>(this, |s| s.0.take()).flatten();
    crate::libxml::run(vm, a, move || drop(old))?;
    set_writer(this, w);
    Ok(())
}

fn f_open_memory(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    match XmlWriter::memory() {
        Some(w) => new_writer_object(vm, w),
        None => Ok(Value::Bool(false)),
    }
}

fn m_open_memory(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    match XmlWriter::memory() {
        Some(w) => {
            replace_writer(vm, a, &this, w)?;
            Ok(Value::Bool(true))
        }
        None => Ok(Value::Bool(false)),
    }
}

fn to_uri(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let uri = s(a, 0);
    check_path(vm, a, &uri)?;
    let dir = cwd();
    match crate::libxml::run(vm, a, || XmlWriter::open_uri(&uri, &dir))? {
        Ok(w) => create_static(vm, a, w),
        Err(OpenUriError::Empty) => Err(a.value_error(vm, 1, "must not be empty")),
        Err(OpenUriError::Unresolvable) => {
            Err(a.value_error(vm, 1, "must resolve to a valid file path"))
        }
        Err(OpenUriError::Failed) => Err(vm.throw("Error", "Could not construct libxml writer")),
    }
}

fn to_memory(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match XmlWriter::memory() {
        Some(w) => create_static(vm, a, w),
        None => Err(vm.throw("Error", "Could not construct libxml writer")),
    }
}

/// Writes to a stream resource without owning it; fails once the stream
/// was closed (`resource->ptr == NULL`).
struct ResourceSink {
    cell: Rc<res::StreamCell>,
    _keep: Resource,
}

impl Write for ResourceSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.cell.get() {
            Some(mut st) => st
                .write(buf)
                .map_err(|_| std::io::Error::other("stream write failed")),
            None => Err(std::io::Error::other("stream closed")),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn to_stream(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Value::Resource(r) = a.at(0) else {
        let given = a.at(0).type_name();
        return Err(a.type_error(vm, 1, &format!("must be of type resource, {given} given")));
    };
    let cell = match res::cell_of(&r) {
        Some(c) if !r.is_closed() && !c.is_closed() => c,
        _ => {
            return Err(vm.throw(
                "TypeError",
                "XMLWriter::toStream(): supplied resource is not a valid stream resource",
            ))
        }
    };
    let sink = ResourceSink { cell, _keep: r };
    match XmlWriter::to_sink(Box::new(sink)) {
        Some(w) => create_static(vm, a, w),
        None => Err(vm.throw("Error", "Could not construct libxml writer")),
    }
}

// ---------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------

/// `(procedural signature, method signature, function, method)`.
type Pair = (
    &'static str,
    &'static str,
    php_vm::ext::NativeFn,
    php_vm::ext::NativeFn,
);

const PAIRS: &[Pair] = &[
    ("xmlwriter_set_indent(XMLWriter $writer, bool $enable): bool", "setIndent(bool $enable): bool", f_set_indent, m_set_indent),
    ("xmlwriter_set_indent_string(XMLWriter $writer, string $indentation): bool", "setIndentString(string $indentation): bool", f_set_indent_string, m_set_indent_string),
    ("xmlwriter_start_comment(XMLWriter $writer): bool", "startComment(): bool", f_start_comment, m_start_comment),
    ("xmlwriter_end_comment(XMLWriter $writer): bool", "endComment(): bool", f_end_comment, m_end_comment),
    ("xmlwriter_start_attribute(XMLWriter $writer, string $name): bool", "startAttribute(string $name): bool", f_start_attribute, m_start_attribute),
    ("xmlwriter_end_attribute(XMLWriter $writer): bool", "endAttribute(): bool", f_end_attribute, m_end_attribute),
    ("xmlwriter_write_attribute(XMLWriter $writer, string $name, string $value): bool", "writeAttribute(string $name, string $value): bool", f_write_attribute, m_write_attribute),
    ("xmlwriter_start_attribute_ns(XMLWriter $writer, ?string $prefix, string $name, ?string $namespace): bool", "startAttributeNs(?string $prefix, string $name, ?string $namespace): bool", f_start_attribute_ns, m_start_attribute_ns),
    ("xmlwriter_write_attribute_ns(XMLWriter $writer, ?string $prefix, string $name, ?string $namespace, string $value): bool", "writeAttributeNs(?string $prefix, string $name, ?string $namespace, string $value): bool", f_write_attribute_ns, m_write_attribute_ns),
    ("xmlwriter_start_element(XMLWriter $writer, string $name): bool", "startElement(string $name): bool", f_start_element, m_start_element),
    ("xmlwriter_end_element(XMLWriter $writer): bool", "endElement(): bool", f_end_element, m_end_element),
    ("xmlwriter_full_end_element(XMLWriter $writer): bool", "fullEndElement(): bool", f_full_end_element, m_full_end_element),
    ("xmlwriter_start_element_ns(XMLWriter $writer, ?string $prefix, string $name, ?string $namespace): bool", "startElementNs(?string $prefix, string $name, ?string $namespace): bool", f_start_element_ns, m_start_element_ns),
    ("xmlwriter_write_element(XMLWriter $writer, string $name, ?string $content = null): bool", "writeElement(string $name, ?string $content = null): bool", f_write_element, m_write_element),
    ("xmlwriter_write_element_ns(XMLWriter $writer, ?string $prefix, string $name, ?string $namespace, ?string $content = null): bool", "writeElementNs(?string $prefix, string $name, ?string $namespace, ?string $content = null): bool", f_write_element_ns, m_write_element_ns),
    ("xmlwriter_start_pi(XMLWriter $writer, string $target): bool", "startPi(string $target): bool", f_start_pi, m_start_pi),
    ("xmlwriter_end_pi(XMLWriter $writer): bool", "endPi(): bool", f_end_pi, m_end_pi),
    ("xmlwriter_write_pi(XMLWriter $writer, string $target, string $content): bool", "writePi(string $target, string $content): bool", f_write_pi, m_write_pi),
    ("xmlwriter_start_cdata(XMLWriter $writer): bool", "startCdata(): bool", f_start_cdata, m_start_cdata),
    ("xmlwriter_end_cdata(XMLWriter $writer): bool", "endCdata(): bool", f_end_cdata, m_end_cdata),
    ("xmlwriter_write_cdata(XMLWriter $writer, string $content): bool", "writeCdata(string $content): bool", f_write_cdata, m_write_cdata),
    ("xmlwriter_text(XMLWriter $writer, string $content): bool", "text(string $content): bool", f_text, m_text),
    ("xmlwriter_write_raw(XMLWriter $writer, string $content): bool", "writeRaw(string $content): bool", f_write_raw, m_write_raw),
    ("xmlwriter_start_document(XMLWriter $writer, ?string $version = \"1.0\", ?string $encoding = null, ?string $standalone = null): bool", "startDocument(?string $version = \"1.0\", ?string $encoding = null, ?string $standalone = null): bool", f_start_document, m_start_document),
    ("xmlwriter_end_document(XMLWriter $writer): bool", "endDocument(): bool", f_end_document, m_end_document),
    ("xmlwriter_write_comment(XMLWriter $writer, string $content): bool", "writeComment(string $content): bool", f_write_comment, m_write_comment),
    ("xmlwriter_start_dtd(XMLWriter $writer, string $qualifiedName, ?string $publicId = null, ?string $systemId = null): bool", "startDtd(string $qualifiedName, ?string $publicId = null, ?string $systemId = null): bool", f_start_dtd, m_start_dtd),
    ("xmlwriter_end_dtd(XMLWriter $writer): bool", "endDtd(): bool", f_end_dtd, m_end_dtd),
    ("xmlwriter_write_dtd(XMLWriter $writer, string $name, ?string $publicId = null, ?string $systemId = null, ?string $content = null): bool", "writeDtd(string $name, ?string $publicId = null, ?string $systemId = null, ?string $content = null): bool", f_write_dtd, m_write_dtd),
    ("xmlwriter_start_dtd_element(XMLWriter $writer, string $qualifiedName): bool", "startDtdElement(string $qualifiedName): bool", f_start_dtd_element, m_start_dtd_element),
    ("xmlwriter_end_dtd_element(XMLWriter $writer): bool", "endDtdElement(): bool", f_end_dtd_element, m_end_dtd_element),
    ("xmlwriter_write_dtd_element(XMLWriter $writer, string $name, string $content): bool", "writeDtdElement(string $name, string $content): bool", f_write_dtd_element, m_write_dtd_element),
    ("xmlwriter_start_dtd_attlist(XMLWriter $writer, string $name): bool", "startDtdAttlist(string $name): bool", f_start_dtd_attlist, m_start_dtd_attlist),
    ("xmlwriter_end_dtd_attlist(XMLWriter $writer): bool", "endDtdAttlist(): bool", f_end_dtd_attlist, m_end_dtd_attlist),
    ("xmlwriter_write_dtd_attlist(XMLWriter $writer, string $name, string $content): bool", "writeDtdAttlist(string $name, string $content): bool", f_write_dtd_attlist, m_write_dtd_attlist),
    ("xmlwriter_start_dtd_entity(XMLWriter $writer, string $name, bool $isParam): bool", "startDtdEntity(string $name, bool $isParam): bool", f_start_dtd_entity, m_start_dtd_entity),
    ("xmlwriter_end_dtd_entity(XMLWriter $writer): bool", "endDtdEntity(): bool", f_end_dtd_entity, m_end_dtd_entity),
    ("xmlwriter_write_dtd_entity(XMLWriter $writer, string $name, string $content, bool $isParam = false, ?string $publicId = null, ?string $systemId = null, ?string $notationData = null): bool", "writeDtdEntity(string $name, string $content, bool $isParam = false, ?string $publicId = null, ?string $systemId = null, ?string $notationData = null): bool", f_write_dtd_entity, m_write_dtd_entity),
    ("xmlwriter_output_memory(XMLWriter $writer, bool $flush = true): string", "outputMemory(bool $flush = true): string", f_output_memory, m_output_memory),
    ("xmlwriter_flush(XMLWriter $writer, bool $empty = true): string|int", "flush(bool $empty = true): string|int", f_flush, m_flush),
];

/// Registers the `xmlwriter` module.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let mut class = ClassBuilder::new("XMLWriter")
        .flags(php_vm::ext::class_flags::NO_CLONE | php_vm::ext::class_flags::NOT_SERIALIZABLE)
        .hooks(ClassHooks {
            create: Some(init_slot),
            ..Default::default()
        })
        .method("openUri(string $uri): bool", acc::PUBLIC, m_open_uri)
        .method(
            "toUri(string $uri): static",
            acc::PUBLIC | acc::STATIC,
            to_uri,
        )
        .method("openMemory(): bool", acc::PUBLIC, m_open_memory)
        .method("toMemory(): static", acc::PUBLIC | acc::STATIC, to_memory)
        .method(
            "toStream($stream): static",
            acc::PUBLIC | acc::STATIC,
            to_stream,
        );
    for (_, sig, _, meth) in PAIRS {
        class = class.method(sig, acc::PUBLIC, *meth);
    }
    class.register(m.vm());
    m.function_static(
        "xmlwriter_open_uri(string $uri): XMLWriter|false",
        f_open_uri,
    )
    .function_static("xmlwriter_open_memory(): XMLWriter|false", f_open_memory);
    for &(sig, _, func, _) in PAIRS {
        m.function_static(sig, func);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_cover_every_operation() {
        assert_eq!(PAIRS.len(), 40);
        for (f, meth, _, _) in PAIRS {
            assert!(f.starts_with("xmlwriter_"), "{f}");
            assert!(!meth.contains("XMLWriter $writer"), "{meth}");
        }
    }
}
