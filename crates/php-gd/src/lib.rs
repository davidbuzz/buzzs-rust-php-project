//! `ext/gd` for phprust: a libgd binding with PHP's `ext/gd` semantics
//! (ref: ext/gd/gd.c, ext/gd/gd.stub.php, ext/gd/libgd/ for the bundled
//! deviations from upstream libgd). VM-independent; `php-ext-gd` binds it.

/// The libgd version this crate was built against.
#[must_use]
pub fn gd_version() -> &'static str {
    "2.3.3"
}
