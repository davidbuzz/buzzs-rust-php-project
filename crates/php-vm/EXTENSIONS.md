# Writing phprust extensions

Native PHP extensions (`ext/*`) are bound to the VM in their own crates
(`crates/php-ext-<name>`), using only the public API in `php_vm::ext`.
Binding crates never edit `crates/php-vm`. The pure algorithm lives in a
library crate (`php-ctype`, `php-json`, ...); the binding crate translates
between PHP values and that library.

## 1. The crate

```toml
# crates/php-ext-ctype/Cargo.toml
[package]
name = "php-ext-ctype"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
php-vm = { path = "../php-vm" }
php-runtime = { path = "../php-runtime" }
php-ctype = { path = "../php-ctype" }

[lints]
workspace = true
```

The crate must be listed in the workspace `members` (ask the coordinator)
and then linked into the CLI by adding **one line** to
`crates/php-cli/src/extensions.rs` plus the dependency in
`crates/php-cli/Cargo.toml`:

```rust
Box::new(php_ext_ctype::Ctype),
```

## 2. The extension

```rust
use php_ctype::{ctype_call, CtypeClass};
use php_runtime::Value;
use php_vm::ext::{Args, Extension, ModuleBuilder, VmResult};
use php_vm::Vm;

pub struct Ctype;

fn call(vm: &mut Vm, a: &Args, class: CtypeClass) -> VmResult<Value> {
    let out = ctype_call(class, &a.at(0));
    if let Some(msg) = &out.deprecation {
        vm.php_deprecated(msg)?;          // "ctype_digit(): Argument of type int ..."
    }
    Ok(Value::Bool(out.result))
}

fn digit(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    call(vm, a, CtypeClass::Digit)
}

impl Extension for Ctype {
    fn name(&self) -> &'static str { "ctype" }       // get_loaded_extensions()

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        m.function("ctype_digit(mixed $text): bool", digit);
        // ... the other ten
    }
}
```

The complete example is `crates/php-ext-ctype/src/lib.rs`.

## 3. Native functions

`ModuleBuilder::function(signature, handler)` registers a function. The
signature is a PHP stub declaration:

```
name(type $param, ?type $opt = default, type &$byref = null, type ...$rest): ret
```

* **Types** use stub syntax: `int`, `float`, `string`, `bool`, `array`,
  `object`, `callable`, `iterable`, `mixed`, `null`, `false`, class names,
  unions (`array|string`), nullable (`?int`). An untyped parameter (`$x`) is
  passed through unchanged.
* **Defaults**: a parameter with `= ...` is optional. Simple literals
  (`0`, `-1`, `"x"`, `null`, `true`, `[]`, `PHP_INT_MAX`) are filled in
  both when the argument was omitted and when it was skipped by named
  arguments (`a.has(n)` stays `false`, `a.at(n)`/`a.int(n)` see the
  default). Anything else (`UNKNOWN`, expressions) leaves `Value::Null`
  and the handler decides with `a.has(n)`.
* **`callable` parameters** produce PHP's ZPP text on failure:
  `f(): Argument #1 ($cb) must be a valid callback, function "nope" not
  found or invalid function name` (`?callable` adds "or null").
* **By-reference** (`&$x`): the handler receives `Value::Reference`; write
  back with `a.set_ref(n, value)` or `a.reference(n)`.
* **Variadic** (`...$rest`): all remaining arguments are in `a.values`
  (`a.named` holds extra named arguments).

Before calling the handler the VM performs PHP's ZPP: arity errors
(`ArgumentCountError: f() expects exactly 1 argument, 0 given`), named
argument resolution, scalar coercion according to the caller's
`strict_types`, the `Passing null to parameter #1 ($x) of type string is
deprecated` deprecation, and `TypeError: f(): Argument #1 ($x) must be of
type string, array given`. Declared-type arguments are therefore already
of the right PHP type:

| accessor | for declared type |
|---|---|
| `a.str(n) -> ZStr` | `string` |
| `a.int(n) -> i64` | `int` |
| `a.float(n) -> f64` | `float` |
| `a.bool(n) -> bool` | `bool` |
| `a.array(n) -> ZArray` | `array` |
| `a.at(n) -> Value` | anything (dereferenced, `Null` if missing) |
| `a.get(n) -> Option<Value>` | passed or not |
| `a.opt(n) -> Option<Value>` | passed and not null |
| `a.has(n) -> bool` | whether passed |
| `a.this` / `a.this_obj()` | `$this` for methods |
| `a.called` | late static binding class for static methods |

Return `Ok(value)`; to throw, `return Err(vm.throw("ValueError", "msg"))`
or the argument helpers `a.value_error(vm, n, "must be ...")` /
`a.type_error(vm, n, "must be ...")` which build
`func(): Argument #n ($name) must be ...`.

## 4. Errors, warnings, callbacks

| API | PHP equivalent |
|---|---|
| `vm.throw(class, msg) -> Throw` | `zend_throw_exception(ce, msg)` (any class name, incl. user/extension classes) |
| `vm.php_warning(msg)` / `php_notice` / `php_deprecated` | `php_error_docref(NULL, E_*, ...)` — prefixed `func(): ` |
| `vm.raise(level, msg)` | `zend_error(level, ...)` (no prefix) |
| `vm.php_warning_bytes(&[u8])`, `php_error_bytes(level, &[u8])`, `raise_bytes` | binary-safe variants (messages are not UTF-8 converted) |
| `vm.call_value(&callable, args)` | `call_user_function` |
| `vm.is_callable(&v)` | `zend_is_callable` |
| `vm.value_to_string/int/float/bool(&v)` | `zval_get_string/long/double`, `zend_is_true` |
| `vm.output(bytes)` | `php_output_write` |
| `vm.ini_string/ini_long/ini_bool(name)` | `INI_STR/INI_INT/INI_BOOL` |
| `vm.lookup_class(&name)` / `vm.find_class(bytes)` | `zend_lookup_class` (with / without autoload) |
| `vm.new_object(&class)`, `new_object_with_args` | `object_init_ex` (+ constructor) |
| `vm.get_property/set_property/set_property_raw` | `zend_read_property`/`zend_update_property` |
| `vm.call_method_by_name(&obj, name, args)` | `zend_call_method` |

Every fallible call returns `VmResult<T>`; propagate with `?` so user
exceptions, `exit()` and fatal errors unwind correctly. Warnings can throw
too (a user error handler may throw), hence `?` on them.

## 5. Constants and INI

```rust
m.constant("JSON_PRETTY_PRINT", Value::Int(128));
m.ini(IniDirective::new("json.foo", Some("1"), Access::ALL, ModifyHandler::Bool, "json"));
m.on_request_shutdown(|vm| { /* RSHUTDOWN */ });
```

`IniDirective`, `Access` and `ModifyHandler` come from
`php_support::ini`. `-d` values given on the command line apply to
directives registered later.

## 6. Classes

```rust
use php_vm::ext::{acc, class_flags, ClassBuilder, ClassHooks};

let cls = ClassBuilder::new("HashContext")
    .flags(class_flags::FINAL)
    .implements(m.vm(), &["Countable"])        // interfaces by name
    .extends(m.vm(), "Exception")                // parent by name (optional)
    .constant("FOO", Value::Int(1))
    .prop("algo", acc::PUBLIC | acc::READONLY, Some("string"), None)
    .static_prop("count", acc::PUBLIC, Value::Int(0))
    .method("__construct(string $algo)", acc::PUBLIC, ctor)
    .method("create(): static", acc::PUBLIC | acc::STATIC, create)
    .hooks(ClassHooks { create: Some(init_state), clone: Some(copy_state), ..Default::default() })
    .register(m.vm());
```

Register classes in dependency order (parents and interfaces first;
classes from other extensions are found by name if those extensions were
registered earlier).

**Native state**: attach any Rust value to an object with
`php_vm::object::set_internal(&obj, state)` (typically in the `create`
hook) and access it with `php_vm::object::internal::<T, _>(&obj, |s| ...)`.

**Object handlers** (`ClassHooks`, all optional, inherited by subclasses
including user classes; `Ok(None)`/`Ok(false)` means "default
behaviour"): `create`, `clone`, `properties_for` (var_dump / `(array)` /
serialize property table), `compare`, `cast`, `do_operation` (operator
overloading), `count`, `read_property` / `write_property` /
`has_property` / `unset_property`, `read_dimension` / `write_dimension` /
`has_dimension` / `unset_dimension` (`$obj[$k]` without `ArrayAccess`),
`get_iterator` (returns a `Box<dyn NativeIterator>` for `foreach`),
`serialize` / `unserialize`.

## 7. Resources

Use `php_runtime::Resource::new("stream", Some(Box::new(state)))` and
return `Value::Resource(r)`; read the payload with
`r.data().as_ref().and_then(|d| d.downcast_ref::<T>())`.

## 8. Testing

Build `target/debug/php` and run the extension's phpt directory:

```
cargo build -p php-cli && target/debug/phpt-runner -q modules/php-src/ext/ctype/tests
```
