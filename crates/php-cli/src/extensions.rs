//! The native extensions linked into the `php` binary.
//!
//! Binding crates (`crates/php-ext-*`) add one line here (and a dependency
//! in `crates/php-cli/Cargo.toml`); see `crates/php-vm/EXTENSIONS.md`.

use php_vm::ext::Extension;

/// All linked extensions, in registration order.
pub fn all() -> Vec<Box<dyn Extension>> {
    vec![
        Box::new(php_ext_stdstr::StdStr),
        Box::new(php_ext_streams::Streams),
        Box::new(php_ext_date::Date),
        // Box::new(php_ext_ctype::Ctype),
        Box::new(php_ext_json::Json),
        Box::new(php_ext_hash::Hash),
        Box::new(php_ext_random::Random),
        Box::new(php_ext_filter::Filter),
        Box::new(php_ext_pcre::Pcre),
        Box::new(php_ext_stdarr::StdArray),
        Box::new(php_ext_stdmisc::StdMisc),
        Box::new(php_ext_spl::Spl),
        Box::new(php_ext_pdo::Pdo),
    ]
}
