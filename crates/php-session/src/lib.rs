//! PHP's session extension (`ext/session`) for phprust.
//!
//! * [`state`] — the per-request [`SessionState`] (`PS()` globals) and the
//!   internal lifecycle: ID acquisition (cookie → GET/POST → generated),
//!   strict-mode validation, cookie and cache-limiter headers, lazy writes,
//!   GC, destroy/abort/reset, request startup/shutdown.
//! * [`api`] — every `session_*()` function with PHP's diagnostics.
//! * [`ini`] — the `session.*` directives with their defaults, access and
//!   on-modify validation, plus a store implementing `ini_set`/`ini_get`/
//!   `ini_restore`/deactivation for them.
//! * [`serializer`] — the `php`, `php_binary` and `php_serialize`
//!   serializers, byte-exact (shared back-reference numbering included).
//! * [`handler`] — the [`SaveModule`]/[`SaveHandler`] traits for native
//!   storage modules and the module dispatch; [`files`] — the `files`
//!   handler (`mod_files.c`); [`user`] — the `user` handler bridging to
//!   `session_set_save_handler()` callables.
//! * [`class`] — `SessionHandler` methods, the interface declarations and
//!   the `PHP_SESSION_*` constants for the VM to register.
//! * [`rfc1867`] — upload progress tracking hooks for the multipart parser.
//! * [`sid`] — session ID generation/validation; [`http`] — cookie and
//!   cache-limiter header formatting.
//!
//! The embedder implements [`SessionHost`]; see its documentation for the
//! re-entrancy model (user handlers call back into the module).
//!
//! Reference: `ext/session/{session.c,mod_files.c,mod_user.c,
//! mod_user_class.c,session.stub.php}`.

pub mod api;
pub mod class;
pub mod files;
pub mod handler;
pub mod host;
pub mod http;
pub mod ini;
pub mod rfc1867;
pub mod serializer;
pub mod sid;
pub mod state;
pub mod user;

pub use handler::{ModRef, SaveHandler, SaveModule};
pub use host::SessionHost;
pub use serializer::Serializer;
pub use state::{module_startup, request_shutdown, request_startup, SessionState, SessionStatus};
