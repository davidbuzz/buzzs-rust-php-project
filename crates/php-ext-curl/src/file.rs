//! `CURLFile` and `CURLStringFile` (`curl_file.c`).

use php_runtime::Value;
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ModuleBuilder, VmResult};
use php_vm::Vm;

/// `curlfile_ctor()`: sets `name` and, when given, `mime`/`postname`.
fn init(vm: &mut Vm, o: &php_runtime::Obj, a: &Args) -> VmResult<()> {
    let name = a.str(0);
    if name.as_bytes().contains(&0) {
        return Err(a.value_error(vm, 1, "must not contain any null bytes"));
    }
    vm.set_property(o, "name", Value::Str(name))?;
    if let Some(Value::Str(m)) = a.opt(1) {
        vm.set_property(o, "mime", Value::Str(m))?;
    }
    if let Some(Value::Str(p)) = a.opt(2) {
        vm.set_property(o, "postname", Value::Str(p))?;
    }
    Ok(())
}

fn ctor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    init(vm, &o, a)?;
    Ok(Value::Null)
}

fn curl_file_create(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(cls) = vm.find_class(b"CURLFile") else {
        return Err(vm.throw("Error", "Class \"CURLFile\" not found"));
    };
    let o = vm.new_object(&cls)?;
    init(vm, &o, a)?;
    Ok(Value::Object(o))
}

fn get(vm: &mut Vm, a: &Args, name: &str) -> VmResult<Value> {
    let o = a.this_obj();
    Ok(vm.get_property(&o, name)?.deref())
}

fn get_filename(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    get(vm, a, "name")
}

fn get_mime_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    get(vm, a, "mime")
}

fn get_post_filename(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    get(vm, a, "postname")
}

fn set_mime_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    vm.set_property(&o, "mime", Value::Str(a.str(0)))?;
    Ok(Value::Null)
}

fn set_post_filename(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    vm.set_property(&o, "postname", Value::Str(a.str(0)))?;
    Ok(Value::Null)
}

fn string_file_ctor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    vm.set_property(&o, "data", Value::Str(a.str(0)))?;
    vm.set_property(&o, "postname", Value::Str(a.str(1)))?;
    let mime = if a.has(2) {
        Value::Str(a.str(2))
    } else {
        Value::from("application/octet-stream")
    };
    vm.set_property(&o, "mime", mime)?;
    Ok(Value::Null)
}

/// Registers both classes and `curl_file_create()`.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    ClassBuilder::new("CURLFile")
        .flags(class_flags::NOT_SERIALIZABLE)
        .prop("name", acc::PUBLIC, Some("string"), Some(Value::from("")))
        .prop("mime", acc::PUBLIC, Some("string"), Some(Value::from("")))
        .prop("postname", acc::PUBLIC, Some("string"), Some(Value::from("")))
        .method(
            "__construct(string $filename, ?string $mime_type = null, ?string $posted_filename = null)",
            acc::PUBLIC,
            ctor,
        )
        .method("getFilename(): string", acc::PUBLIC, get_filename)
        .method("getMimeType(): string", acc::PUBLIC, get_mime_type)
        .method("getPostFilename(): string", acc::PUBLIC, get_post_filename)
        .method("setMimeType(string $mime_type): void", acc::PUBLIC, set_mime_type)
        .method("setPostFilename(string $posted_filename): void", acc::PUBLIC, set_post_filename)
        .register(m.vm());
    ClassBuilder::new("CURLStringFile")
        .prop("data", acc::PUBLIC, Some("string"), None)
        .prop("postname", acc::PUBLIC, Some("string"), None)
        .prop("mime", acc::PUBLIC, Some("string"), None)
        .method(
            "__construct(string $data, string $postname, string $mime = \"application/octet-stream\")",
            acc::PUBLIC,
            string_file_ctor,
        )
        .register(m.vm());
    m.function_static(
        "curl_file_create(string $filename, ?string $mime_type = null, ?string $posted_filename = null): CURLFile",
        curl_file_create,
    );
}
