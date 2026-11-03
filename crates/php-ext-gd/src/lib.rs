//! `ext/gd` for the phprust VM: binds the `php-gd` library (GdImage,
//! GdFont, the `image*` functions and the GD constants) through the
//! `php_vm::ext` extension API. Ref: ext/gd/gd.c, ext/gd/gd.stub.php.

use php_vm::ext::{Extension, ModuleBuilder};

/// The `gd` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Gd;

impl Extension for Gd {
    fn name(&self) -> &'static str {
        "gd"
    }

    fn register(&self, _m: &mut ModuleBuilder<'_>) {}
}
