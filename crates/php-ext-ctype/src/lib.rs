//! `ext/ctype` for the phprust VM: binds the `php-ctype` library through
//! the `php_vm::ext` extension API.

use php_ctype::{ctype_call, CtypeClass};
use php_runtime::Value;
use php_vm::ext::{Args, Extension, ModuleBuilder, VmResult};
use php_vm::Vm;

/// The `ctype` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ctype;

fn call(vm: &mut Vm, a: &Args, class: CtypeClass) -> VmResult<Value> {
    // `mixed $text`: no coercion; php-ctype decides and reports the
    // deprecation, which we emit with the function-name prefix.
    let out = ctype_call(class, &a.at(0));
    if let Some(msg) = &out.deprecation {
        vm.php_deprecated(msg)?;
    }
    Ok(Value::Bool(out.result))
}

macro_rules! ctype_fn {
    ($($name:ident => $class:ident),* $(,)?) => {
        $(fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            call(vm, a, CtypeClass::$class)
        })*
    };
}

ctype_fn! {
    alnum => Alnum, alpha => Alpha, cntrl => Cntrl, digit => Digit,
    graph => Graph, lower => Lower, print => Print, punct => Punct,
    space => Space, upper => Upper, xdigit => Xdigit,
}

impl Extension for Ctype {
    fn name(&self) -> &'static str {
        "ctype"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        m.function("ctype_alnum(mixed $text): bool", alnum)
            .function("ctype_alpha(mixed $text): bool", alpha)
            .function("ctype_cntrl(mixed $text): bool", cntrl)
            .function("ctype_digit(mixed $text): bool", digit)
            .function("ctype_graph(mixed $text): bool", graph)
            .function("ctype_lower(mixed $text): bool", lower)
            .function("ctype_print(mixed $text): bool", print)
            .function("ctype_punct(mixed $text): bool", punct)
            .function("ctype_space(mixed $text): bool", space)
            .function("ctype_upper(mixed $text): bool", upper)
            .function("ctype_xdigit(mixed $text): bool", xdigit);
    }
}
