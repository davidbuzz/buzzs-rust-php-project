//! `ext/curl` for the phprust VM: binds the `php-curl` libcurl wrapper
//! through the `php_vm::ext` extension API (ref: `ext/curl/interface.c`,
//! `multi.c`, `share.c`, `curl_file.c`).
//!
//! * `CurlHandle`, `CurlMultiHandle`, `CurlShareHandle`,
//!   `CurlSharePersistentHandle`: final opaque classes whose native state
//!   lives in [`state`].
//! * Transfer callbacks run PHP code through [`dispatch::Dispatch`], the
//!   [`php_curl::Handler`] lent to libcurl for the duration of
//!   `curl_exec()`/`curl_multi_exec()`.
//! * `CURLOPT_FILE`/`INFILE`/`WRITEHEADER`/`STDERR` use php-ext-streams
//!   stream resources.

mod dispatch;
mod easy;
mod file;
mod mime;
mod multi;
mod setopt;
mod share;
mod state;
mod streams;

use php_support::ini::{Access, IniDirective, ModifyHandler};
use php_runtime::Value;
use php_vm::ext::{Extension, ModuleBuilder};

/// The `curl` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Curl;

impl Extension for Curl {
    fn name(&self) -> &'static str {
        "curl"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        php_curl::easy::global_init();
        m.ini(IniDirective::new("curl.cainfo", Some(""), Access::SYSTEM, ModifyHandler::None, "curl"));
        for &(name, value, deprecated) in php_curl::consts::PHP_CONSTANTS {
            match deprecated {
                Some((since, msg)) => m.constant_deprecated(name, Value::Int(value), since, msg),
                None => m.constant(name, Value::Int(value)),
            };
        }
        easy::register_class(m);
        multi::register_class(m);
        share::register_classes(m);
        file::register(m);
        easy::register_functions(m);
        multi::register_functions(m);
        share::register_functions(m);
    }
}
