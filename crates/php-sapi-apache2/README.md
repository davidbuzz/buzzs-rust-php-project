# php-sapi-apache2 — buzzs-rust-php-project as an Apache 2.4 module

`apache2handler` SAPI (ref: `sapi/apache2handler`): builds
`target/<profile>/libphprust_apache2.so`, a drop-in replacement for
`mod_php` (`LoadModule php_module …`).

## Build

Requires the httpd/APR headers (`apache2-dev`: `apxs`, `apr-1-config`);
`build.rs` locates them (override with `APXS=/path/to/apxs`).

```sh
cargo build --release -p php-sapi-apache2
```

## Install (no `apxs -i`)

```sh
sudo install -m 0644 target/release/libphprust_apache2.so /usr/lib/apache2/modules/libphprust_apache2.so
```

Disable any real `mod_php` first (`a2dismod php8.x`) — both define `php_module`.

## Apache configuration

```apache
# Only mpm_prefork is supported out of the box (see "Threads" below).
LoadModule mpm_prefork_module /usr/lib/apache2/modules/mod_mpm_prefork.so
LoadModule php_module /usr/lib/apache2/modules/libphprust_apache2.so

<FilesMatch "\.php$">
    SetHandler application/x-httpd-php
</FilesMatch>
<FilesMatch "\.phps$">
    SetHandler application/x-httpd-php-source
</FilesMatch>
# or: AddHandler php-script .php
DirectoryIndex index.php index.html

# Optional: directory holding php.ini (otherwise $PHPRC; $PHP_INI_SCAN_DIR
# adds *.ini files).  Without a php.ini PHP's built-in defaults apply
# (display_errors=1, log_errors=0, max_execution_time=30, ...).
PHPINIDir /etc/phprust

<Directory /var/www/html>
    AllowOverride Options          # lets .htaccess use php_value/php_flag
    php_value  memory_limit 256M
    php_flag   display_errors Off
    php_admin_value open_basedir /var/www   # PHP_INI_SYSTEM, locks ini_set()
    php_admin_flag  log_errors On
</Directory>
```

`php_value`/`php_flag` (also in `.htaccess`, needs `AllowOverride
Options`) and `php_admin_value`/`php_admin_flag` (server config only)
follow `apache_config.c`: `none` means empty, flags are `On`/`1`, merged
per directory with admin entries winning, admin values cannot be changed
with `ini_set()`.  `php_flag engine off` hands the request back to Apache.

## Behaviour

* `PHP_SAPI`/`php_sapi_name()` = `apache2handler`; `$_SERVER` comes from
  Apache's CGI environment (`SCRIPT_FILENAME`, `PATH_INFO`, `HTTP_*`, …)
  plus `PHP_SELF`, `PHP_AUTH_USER/PW/DIGEST`, `REQUEST_TIME(_FLOAT)`.
* `header()`, `header_remove()`, `headers_list()`, `headers_sent()` (with
  the file and line where output started), `http_response_code()`,
  `setcookie()`/`setrawcookie()`; "headers already sent by (output started
  at file:line)" warnings; default `Content-Type: text/html;
  charset=UTF-8`; `X-Powered-By` and the `Server` token with `expose_php`.
  A fatal error with `display_errors` off yields `HTTP/1.0 500`.
* `header_register_callback()`: the callable runs when the headers go out,
  which is the first output (also output from inside an output handler),
  `flush()`, `virtual()` or request shutdown — as `sapi_send_headers()`
  does.  HEAD requests get the headers and no body.
* POST forms, `multipart/form-data` uploads (`$_FILES`,
  `is_uploaded_file()`, `move_uploaded_file()`, `upload_max_filesize`,
  `max_file_uploads`, `post_max_size`, `MAX_FILE_SIZE`), `php://input`;
  ext/session is linked, so `session.upload_progress.*` tracks uploads
  (`$_SESSION["upload_progress_<name>"]`) while the body is parsed.
  `request_parse_body()` parses a urlencoded or multipart body of any
  method (PUT, PATCH, …) into `[$post, $files]`, with the limits of its
  `$options` and `RequestParseBodyException`s, from whatever of the body
  request startup and `php://input` left unread.
* `flush()` → `ap_rflush`; output is written with `ap_rwrite`;
  `implicit_flush=1` / `ob_implicit_flush()` flush after every write.
* A client that went away (`ap_rwrite`/`ap_rflush` failing):
  `connection_status()` reports `CONNECTION_ABORTED`, output is discarded
  for the rest of the request and the script is stopped like `exit()`
  (shutdown functions and destructors still run) unless
  `ignore_user_abort` is on — `php_handle_aborted_connection()`.
* Errors: `display_errors` (HTML) to the page, `log_errors` to the Apache
  error log as `[php:warn] … PHP Warning:  …` (level per error type);
  `error_log()` → `[php:notice]`.
* `apache_request_headers()`/`getallheaders()`, `apache_response_headers()`,
  `apache_note()`, `apache_setenv()`/`apache_getenv()`,
  `apache_lookup_uri()`, `virtual()`, `apache_get_version()`,
  `apache_get_modules()`.
* `max_execution_time` (default 30 s) counts the request thread's CPU
  time, like php-src's `ITIMER_PROF` timer on Linux (a script waiting in
  `sleep()` or on I/O does not time out).  On expiry the executor raises
  `Fatal error: Maximum execution time of N seconds exceeded in <file> on
  line <n>` at its next opcode — displayed and/or logged like any fatal
  error, with `HTTP/1.0 500` when `display_errors` is off and nothing was
  sent yet — and `connection_status()` gains `CONNECTION_TIMEOUT`.
  `set_time_limit()` and `ini_set('max_execution_time')` restart the
  timer.  With `hard_timeout` (php.ini, default 2 s) at N > 0, a native
  call that has not returned N CPU seconds after the fatal was flagged
  ends the child with
  `Fatal error: Maximum execution time of M+N seconds exceeded
  (terminated)` on the error log and exit status 124, as
  `zend_timeout_handler` does; httpd starts a replacement child.

## Threads

The VM is single-threaded.  Each request runs on its own thread (fresh
thread-local state, 256 MiB stack) under a process-wide lock.  The module
refuses to start under `mpm_event`/`mpm_worker` unless

```apache
PhpRustAllowThreads On
```

is set; then PHP requests within one child run one at a time (throughput
scales with the number of child processes only).

## Tests

```sh
cargo test -p php-sapi-apache2                       # unit + in-memory lifecycle tests (+ tests/interrupts.rs: CPU-time timeouts and client aborts, serialised, ~6 s)
cargo build -p php-cli                               # optional: enables the CLI comparison
cargo test -p php-sapi-apache2 --test httpd -- --ignored --test-threads=1
```

The ignored tests start the system `apache2` on 127.0.0.1:8089 with a
private config under `/tmp/claude-1000/wt-apache/` and stop it afterwards.
