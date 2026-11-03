//! The file and stream functions of `ext/standard` for the phprust VM:
//! binds the `php-streams` library through the `php_vm::ext` extension API.
//!
//! * [`files`] — `file.c` (`fopen`, `fread`, `file_get_contents`, `unlink`, ...);
//! * [`stat`] — `filestat.c` (stat family, chmod, touch, disk space);
//! * [`dirs`] — `dir.c` (opendir, scandir, glob, `Directory`);
//! * [`streams`] — `streamsfuncs.c` (meta data, contexts, select);
//! * [`userwrap`] / [`userfilter`] — user-space wrappers and filters;
//! * [`http`] — the `http://` wrapper's VM services, `get_headers()`;
//! * [`io_poll`] — master's `Io\\Poll` classes;
//! * [`net`] — sockets; [`xport`] — the transport/crypto registry other
//!   extensions (openssl) plug into; [`process`] — `popen()`.
//!
//! [`bridge`] connects the library's diagnostics and callbacks to the VM,
//! [`res`] owns the stream and context resources.

// Native handlers must match `NativeFn` and return `VmResult` even when
// they cannot fail.
#![allow(clippy::unnecessary_wraps)]

pub mod bridge;
pub mod dirs;
pub mod errors;
pub mod files;
pub mod http;
pub mod include;
pub mod ini;
pub mod io_poll;
pub mod meta;
pub mod net;
pub mod process;
pub mod res;
pub mod stat;
pub mod streams;
pub mod userfilter;
pub mod userwrap;
pub mod xport;

use php_runtime::{Resource, Value};
use php_vm::ext::{Extension, ModuleBuilder};
use php_vm::Vm;

/// The streams/file part of `ext/standard`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Streams;

/// Opens `php://stdin` & co. as the CLI's `STDIN`/`STDOUT`/`STDERR`.
fn register_std_streams(m: &mut ModuleBuilder<'_>) {
    for (i, (name, url, mode)) in [
        ("STDIN", "php://stdin", "rb"),
        ("STDOUT", "php://stdout", "wb"),
        ("STDERR", "php://stderr", "wb"),
    ]
    .into_iter()
    .enumerate()
    {
        let s = bridge::enter(m.vm(), || {
            php_streams::open::fopen(url.as_bytes(), mode, false, None)
        });
        let _ = php_streams::diag::take();
        // cli_register_file_handles(no_close = false): an explicit
        // fclose(STDIN) really closes descriptor 0 (`files::fclose` clears
        // the flag), but dropping the resources at VM teardown (or in an
        // embedding such as `php -S`'s workers) must leave the process's
        // descriptors alone.
        if let Some(mut s) = s {
            s.add_flags(php_streams::stream::flags::NO_CLOSE);
            let handle = i64::try_from(i).unwrap_or(0) + 1;
            m.constant(name, res::new_stream_with_handle(s, handle));
        }
    }
    Resource::set_next_handle(4);
}

fn shutdown(vm: &mut Vm) {
    res::shutdown(vm);
    http::shutdown();
}

impl Extension for Streams {
    fn name(&self) -> &'static str {
        "standard"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        bridge::install_hooks();
        register_std_streams(m);
        errors::register(m);
        files::register(m);
        stat::register(m);
        dirs::register(m);
        streams::register(m);
        userwrap::register(m);
        userfilter::register(m);
        net::register(m);
        http::register(m);
        io_poll::register(m);
        process::register(m);
        for (n, v) in CONSTANTS {
            m.constant(n, Value::Int(*v));
        }
        m.on_request_shutdown(shutdown);
        include::install(m.vm());
        ini::register(m);
    }
}

/// Integer constants of `file.stub.php`, `userspace.stub.php` and
/// `basic_functions.stub.php` owned by this extension.
const CONSTANTS: &[(&str, i64)] = &[
    ("SEEK_SET", 0),
    ("SEEK_CUR", 1),
    ("SEEK_END", 2),
    ("LOCK_SH", 1),
    ("LOCK_EX", 2),
    ("LOCK_UN", 3),
    ("LOCK_NB", 4),
    ("STREAM_NOTIFY_CONNECT", 2),
    ("STREAM_NOTIFY_AUTH_REQUIRED", 3),
    ("STREAM_NOTIFY_AUTH_RESULT", 10),
    ("STREAM_NOTIFY_MIME_TYPE_IS", 4),
    ("STREAM_NOTIFY_FILE_SIZE_IS", 5),
    ("STREAM_NOTIFY_REDIRECTED", 6),
    ("STREAM_NOTIFY_PROGRESS", 7),
    ("STREAM_NOTIFY_FAILURE", 9),
    ("STREAM_NOTIFY_COMPLETED", 8),
    ("STREAM_NOTIFY_RESOLVE", 1),
    ("STREAM_NOTIFY_SEVERITY_INFO", 0),
    ("STREAM_NOTIFY_SEVERITY_WARN", 1),
    ("STREAM_NOTIFY_SEVERITY_ERR", 2),
    ("STREAM_FILTER_READ", 1),
    ("STREAM_FILTER_WRITE", 2),
    ("STREAM_FILTER_ALL", 3),
    ("STREAM_CRYPTO_METHOD_ANY_CLIENT", 127),
    ("STREAM_CRYPTO_METHOD_SSLv2_CLIENT", 3),
    ("STREAM_CRYPTO_METHOD_SSLv3_CLIENT", 5),
    ("STREAM_CRYPTO_METHOD_SSLv23_CLIENT", 57),
    ("STREAM_CRYPTO_METHOD_TLS_CLIENT", 121),
    ("STREAM_CRYPTO_METHOD_TLSv1_0_CLIENT", 9),
    ("STREAM_CRYPTO_METHOD_TLSv1_1_CLIENT", 17),
    ("STREAM_CRYPTO_METHOD_TLSv1_2_CLIENT", 33),
    ("STREAM_CRYPTO_METHOD_TLSv1_3_CLIENT", 65),
    ("STREAM_CRYPTO_METHOD_ANY_SERVER", 126),
    ("STREAM_CRYPTO_METHOD_SSLv2_SERVER", 2),
    ("STREAM_CRYPTO_METHOD_SSLv3_SERVER", 4),
    ("STREAM_CRYPTO_METHOD_SSLv23_SERVER", 120),
    ("STREAM_CRYPTO_METHOD_TLS_SERVER", 120),
    ("STREAM_CRYPTO_METHOD_TLSv1_0_SERVER", 8),
    ("STREAM_CRYPTO_METHOD_TLSv1_1_SERVER", 16),
    ("STREAM_CRYPTO_METHOD_TLSv1_2_SERVER", 32),
    ("STREAM_CRYPTO_METHOD_TLSv1_3_SERVER", 64),
    ("STREAM_CRYPTO_PROTO_SSLv3", 4),
    ("STREAM_CRYPTO_PROTO_TLSv1_0", 8),
    ("STREAM_CRYPTO_PROTO_TLSv1_1", 16),
    ("STREAM_CRYPTO_PROTO_TLSv1_2", 32),
    ("STREAM_CRYPTO_PROTO_TLSv1_3", 64),
    ("STREAM_CRYPTO_STATUS_NONE", 0),
    ("STREAM_CRYPTO_STATUS_WANT_READ", 1),
    ("STREAM_CRYPTO_STATUS_WANT_WRITE", 2),
    ("FILE_USE_INCLUDE_PATH", 1),
    ("FILE_IGNORE_NEW_LINES", 2),
    ("FILE_SKIP_EMPTY_LINES", 4),
    ("FILE_APPEND", 8),
    ("FILE_NO_DEFAULT_CONTEXT", 16),
    ("FILE_TEXT", 0),
    ("FILE_BINARY", 0),
    ("FNM_NOESCAPE", 2),
    ("FNM_PATHNAME", 1),
    ("FNM_PERIOD", 4),
    ("FNM_CASEFOLD", 16),
    ("PATHINFO_DIRNAME", 1),
    ("PATHINFO_BASENAME", 2),
    ("PATHINFO_EXTENSION", 4),
    ("PATHINFO_FILENAME", 8),
    ("PATHINFO_ALL", 15),
    ("STREAM_USE_PATH", 1),
    ("STREAM_IGNORE_URL", 2),
    ("STREAM_REPORT_ERRORS", 8),
    ("STREAM_MUST_SEEK", 16),
    ("STREAM_URL_STAT_LINK", 1),
    ("STREAM_URL_STAT_QUIET", 2),
    ("STREAM_MKDIR_RECURSIVE", 1),
    ("STREAM_IS_URL", 1),
    ("STREAM_OPTION_BLOCKING", 1),
    ("STREAM_OPTION_READ_TIMEOUT", 4),
    ("STREAM_OPTION_READ_BUFFER", 2),
    ("STREAM_OPTION_WRITE_BUFFER", 3),
    ("STREAM_BUFFER_NONE", 0),
    ("STREAM_BUFFER_LINE", 1),
    ("STREAM_BUFFER_FULL", 2),
    ("STREAM_CAST_AS_STREAM", 0),
    ("STREAM_CAST_FOR_SELECT", 3),
    ("STREAM_META_TOUCH", 1),
    ("STREAM_META_OWNER", 3),
    ("STREAM_META_OWNER_NAME", 2),
    ("STREAM_META_GROUP", 5),
    ("STREAM_META_GROUP_NAME", 4),
    ("STREAM_META_ACCESS", 6),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_unique() {
        let mut names: Vec<&str> = CONSTANTS.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len());
    }
}
