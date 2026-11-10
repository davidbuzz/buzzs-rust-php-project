//! `ext/simplexml` for the phprust VM: binds the `php-simplexml` library
//! (`SimpleXMLElement`, `SimpleXMLIterator`, `simplexml_*()`) through the
//! `php_vm::ext` extension API.
//!
//! * Every object carries its [`php_simplexml::Sxe`] state (attached by
//!   the `create` hook, replaced when the library wraps a node).
//! * All object handlers are [`php_vm::ext::ClassHooks`] (see [`hooks`]);
//!   the fetch type the C handlers receive comes from the VM's
//!   property/dimension contexts. In write context `read_property`
//!   implements `get_property_ptr_ptr` (creating missing elements) followed
//!   by `read_property(BP_VAR_W)`.
//! * Parsing, loading and saving run inside
//!   [`php_ext_xml::libxml::run`] (stream hooks, entity loader, libxml2
//!   diagnostics and the `libxml_use_internal_errors()` list).

mod hooks;
mod host;
mod methods;
mod xsl;

use php_vm::ext::{acc, class_flags, ClassBuilder, Extension, ModuleBuilder};

use methods as m;

/// The `simplexml` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct SimpleXml;

impl Extension for SimpleXml {
    fn name(&self) -> &'static str {
        "SimpleXML"
    }

    fn register(&self, mb: &mut ModuleBuilder<'_>) {
        let pb = acc::PUBLIC;
        let sxe = ClassBuilder::new("SimpleXMLElement")
            .flags(class_flags::NOT_SERIALIZABLE)
            .implements(mb.vm(), &["Stringable", "Countable", "RecursiveIterator"])
            .method("xpath(string $expression): array|null|false", pb, m::xpath)
            .method(
                "registerXPathNamespace(string $prefix, string $namespace): bool",
                pb,
                m::register_xpath_namespace,
            )
            .method("asXML(?string $filename = null): string|bool", pb, m::as_xml)
            .method("saveXML(?string $filename = null): string|bool", pb, m::as_xml)
            .method("getNamespaces(bool $recursive = false): array", pb, m::get_namespaces)
            .method(
                "getDocNamespaces(bool $recursive = false, bool $fromRoot = true): array|false",
                pb,
                m::get_doc_namespaces,
            )
            .method(
                "children(?string $namespaceOrPrefix = null, bool $isPrefix = false): ?SimpleXMLElement",
                pb,
                m::children,
            )
            .method(
                "attributes(?string $namespaceOrPrefix = null, bool $isPrefix = false): ?SimpleXMLElement",
                pb,
                m::attributes,
            )
            .method(
                "__construct(string $data, int $options = 0, bool $dataIsURL = false, string $namespaceOrPrefix = \"\", bool $isPrefix = false)",
                pb,
                m::construct,
            )
            .method(
                "addChild(string $qualifiedName, ?string $value = null, ?string $namespace = null): ?SimpleXMLElement",
                pb,
                m::add_child,
            )
            .method(
                "addAttribute(string $qualifiedName, string $value, ?string $namespace = null): void",
                pb,
                m::add_attribute,
            )
            .method("getName(): string", pb, m::get_name)
            .method("__toString(): string", pb, m::to_string)
            .method("__debugInfo(): array", pb, m::debug_info)
            .method("count(): int", pb, m::count)
            .method("rewind(): void", pb, m::rewind)
            .method("valid(): bool", pb, m::valid)
            .method("current(): SimpleXMLElement", pb, m::current)
            .method("key(): string", pb, m::key)
            .method("next(): void", pb, m::next)
            .method("hasChildren(): bool", pb, m::has_children)
            .method("getChildren(): ?SimpleXMLElement", pb, m::get_children)
            .hooks(hooks::class_hooks())
            .register(mb.vm());
        let _ = sxe;
        php_ext_xsl::access::register(&xsl::ACCESS);
        ClassBuilder::new("SimpleXMLIterator")
            .flags(class_flags::NOT_SERIALIZABLE)
            .extends(mb.vm(), "SimpleXMLElement")
            .register(mb.vm());
        mb.function_static(
            "simplexml_load_file(string $filename, ?string $class_name = SimpleXMLElement::class, int $options = 0, string $namespace_or_prefix = \"\", bool $is_prefix = false): SimpleXMLElement|false",
            m::load_file,
        )
        .function_static(
            "simplexml_load_string(string $data, ?string $class_name = SimpleXMLElement::class, int $options = 0, string $namespace_or_prefix = \"\", bool $is_prefix = false): SimpleXMLElement|false",
            m::load_string,
        )
        .function_static(
            "simplexml_import_dom(object $node, ?string $class_name = SimpleXMLElement::class): ?SimpleXMLElement",
            m::import_dom,
        );
    }
}
