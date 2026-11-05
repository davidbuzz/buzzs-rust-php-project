# php-memlimit integration guide

For the php-vm / php-cli owner and for extension-binding authors.
API docs: `cargo doc -p php-memlimit`. Reference: `Zend/zend_alloc.c`
(`zend_mm_check_limit`, `zend_mm_delayed_error`, `zend_handle_delayed_oom`,
`zend_set_memory_limit`), `main/main.c` (`OnChangeMemoryLimit`,
`php_request_shutdown`).

## Model in one paragraph

`php_memlimit::GLOBAL` is a `CountingAlloc<MiMalloc>` (mimalloc as the base
allocator) that counts every live
byte. Crossing `memory_limit` never fails an ordinary allocation (Rust would
abort); it **records** an `OverLimit { limit, tried }` (first one wins) that
the VM turns into the PHP fatal at its next safe point — exactly PHP's
"delayed OOM". Builtins that are about to allocate a size the script controls
call `try_reserve(bytes)` first (PHP's fallible `zend_*_falloc` +
`RETURN_OOM()`), so the fatal is raised before anything is allocated. Only an
allocation crossing the **hard ceiling** (2x the limit, or limit + 1 GiB above
1 GiB; `memory_hard_limit_percent` tunes it) fails: it calls the hard-limit
handler if installed, otherwise Rust prints
`memory allocation of N bytes failed` and aborts (SIGABRT).

## 1. php-cli: install the allocator (once)

`crates/php-cli/Cargo.toml`: `php-memlimit = { path = "../php-memlimit" }`
(also add it to php-vm and to every binding that calls `try_reserve`).

`crates/php-cli/src/main.rs`, at item level:

```rust
php_memlimit::install_global_allocator!();
```

Keep `limit_memory()` (`RLIMIT_AS`) as the outermost backstop. Install a
hard-limit handler early in `main()` to get PHP's output instead of Rust's
abort:

```rust
fn hard_limit(e: php_memlimit::HardLimitExceeded) -> ! {
    // PHP: "Hard memory limit of %zu bytes exhausted (tried to allocate %zu bytes) in %s on line %d\n"
    // on stderr, then _exit(137). Runs inside the allocator with the ceiling disabled:
    // allocation is allowed, but the Vm is not reachable here, so read the
    // location from a thread-local the executor keeps (or print "Unknown" / 0).
    let (file, line) = php_vm::current_location_snapshot(); // to be provided by the VM
    let _ = std::io::Write::write_all(&mut std::io::stderr(),
        format!("{e} in {file} on line {line}\n").as_bytes());
    unsafe { libc::_exit(137) } // SAFETY: terminates without running atexit/destructors, like PHP
}
php_memlimit::set_hard_limit_handler(hard_limit);
```

Only the binary may install the allocator. Library test binaries do not see
it: there `usage()` stays at `PHP_BASELINE` and only `try_reserve` enforces
the limit, which is what binding tests want.

## 2. Request lifecycle (php-vm, `Vm::run_script` / `Vm::shutdown`)

| When | Call |
|---|---|
| after INI parsing and extension startup, **before compiling the main script** (top of `run_script`) | `php_memlimit::request_start(php_memlimit::limit_from_ini(ini memory_limit as u64))`; on `Err`, `force_limit(Some(DEFAULT_LIMIT))` (the registry has already warned) |
| after `run_main` returns (also on `Ok`) | poll once more: `oom_after_main_script.phpt` |
| start of `Vm::shutdown`, **before shutdown functions** | `php_memlimit::clear()` (PHP resets `EG(delayed_oom)` here, so a shutdown function can raise its own OOM: `oom_pending_at_shutdown.phpt`) |
| after each shutdown function / destructor batch | poll |
| end of `Vm::shutdown` (after output flush) | `php_memlimit::request_end()` |

`request_start` binds the accounting fast path to the calling thread (no
locked instructions for its allocations), resets the baseline so
`memory_get_usage()` starts at `PHP_BASELINE` (401 408; PHP 8.3 CLI reports
402–404 KB), resets the peak and clears stale violations.

## 3. Safe points: raising the fatal

The poll is one relaxed atomic load. Put it in `Vm::run` (crates/php-vm/src/exec.rs)
next to the existing per-op `object::has_pending_destructors()` check, i.e.
after every opcode — this covers loop back-edges, returns from builtins (the
`DoFcall`-style op that invoked `call_builtin`) and user calls:

```rust
if php_memlimit::is_pending() {
    if let Some(over) = php_memlimit::take() {
        // E_ERROR, attributed to the op just executed (ip - 1):
        // "PHP Fatal error:  Allowed memory size of 10485760 bytes exhausted (tried to allocate 2097152 bytes) in %s on line %d"
        self.raise(E_ERROR, &over.to_string())?; // -> Throw::Fatal, unwinds
    }
}
```

`take()` moves the record to *handled*: until `clear()` no further violation
is recorded and `is_pending()` is false, so the unwinding after the fatal
cannot fire a second one (PHP's `ZEND_OOM_STATE_HANDLED`). The message has no
`" at file:line"` part (that is debug-build only; tests match it with `%S`).
`error_get_last()['message']` is `over.to_string()`.

Also poll before calling user callbacks from builtins if convenient
(`oom_soft_limit_in_callback.phpt`); not required for correctness.

## 4. Bindings: replace estimates with `try_reserve`

Every per-binding model of `memory_limit` (search for `"Allowed memory size"`:
php-ext-stdarr `memory.rs`, php-ext-stdstr, php-ext-pcre `memory.rs`,
php-ext-json, php-ext-uri `memory.rs`, php-ext-zlib `mem.rs`,
php-ext-mbstring `mem.rs`, php-exif `budget.rs`, php-ext-hash, php-ext-iconv,
php-ext-bcmath, php-ext-filter, php-ext-stdmisc `head.rs`, php-soap) should
stop reading the `memory_limit` INI and raising the fatal itself. Instead,
before a script-sized allocation:

```rust
// str_repeat: PHP allocates zend_string header (24) + len + 1
if php_memlimit::try_reserve_n(input.len(), mult, 25).is_err() {
    return Ok(Value::Null); // RETURN_OOM(): the VM raises the fatal right after this call
}
let out = input.repeat(mult);
```

- `try_reserve(bytes)` / `try_reserve_n(count, size, extra)` compare current
  usage + bytes against the limit; on failure they **record** the violation
  (so the VM's post-call poll raises it, with the line of the call) and
  return `Err(OverLimit)`. Return any value; it is discarded. Overflow in
  `try_reserve_n` counts as `usize::MAX` bytes.
- Use PHP's own size formula (e.g. `HT_PACKED_SIZE_EX`, `zend_string` header
  24 + len + 1) where tests print exact numbers; most use `%d`.
- For Rust collections, `php_memlimit::fallible(|| vec.try_reserve_exact(n))`
  makes allocations over the soft limit return `Err` (and records the
  violation). Only `try_*` APIs may run inside `fallible`.
- Incremental builders (json_encode, preg_replace, implode, unserialize…)
  need nothing: the allocator records the violation as soon as the buffer
  crosses the limit and the VM raises it after the builtin returns. Call
  `try_reserve` only where a single allocation could exceed the hard ceiling
  (any size the script picks).
- Keep the non-limit errors (`Possible integer overflow in memory allocation`)
  in the bindings; they are not limit checks.
- Memory that php-src obtains from a library's own `malloc` (not `emalloc`)
  is invisible to `memory_limit` and `memory_get_usage()`. Run such code in
  `php_memlimit::uncounted(|| ...)`: huge blocks (> 2 MiB − 4 KiB) allocated
  inside are registered by address and stay out of usage, peak, the soft
  limit and the hard ceiling until freed (wherever that happens); smaller
  blocks are counted as usual. php-ext-gmp wraps every handler this way
  (libgmp allocates limbs with the system `malloc`). Only `RLIMIT_AS`
  bounds uncounted blocks.

## 5. Userland functions (php-vm `builtins/core.rs`)

| PHP | Implementation |
|---|---|
| `memory_get_usage(bool $real_usage = false)` | `php_memlimit::memory_get_usage(real)` |
| `memory_get_peak_usage(bool $real_usage = false)` | `php_memlimit::memory_get_peak_usage(real)` |
| `memory_reset_peak_usage()` | `php_memlimit::reset_peak()` |
| `gc_mem_caches()` | `php_memlimit::gc_mem_caches()` (always 0: no allocator caches) |

`real_usage` is the usage rounded up to 2 MiB chunks (minimum one chunk), so
a fresh script reports `memory_get_usage(true) === 2097152`, like PHP.

## 6. INI (`memory_limit`, `max_memory_limit`, `memory_hard_limit_percent`)

php-support's `IniRegistry` already implements `OnChangeMemoryLimit` /
`OnChangeMaxMemoryLimit` messages, including the clamp to
`max_memory_limit` and "Failed to set memory limit to %zd bytes (Current
memory usage is %zd bytes)". Wire it to real numbers:

1. Before `vm.ini.ini_set(...)` / `ini_restore` of `memory_limit` or
   `max_memory_limit`: `vm.ini.memory_usage = php_memlimit::real_usage() as u64`.
2. In `Vm::sync_ini` for those names, after the registry accepted the value:
   `php_memlimit::GLOBAL.force_limit(php_memlimit::limit_from_ini(vm.ini.get_long("memory_limit") as u64))`
   (`-1` → `u64::MAX` → unlimited). `force_limit` because the registry has
   already done the usage check and warned.
   (Equivalently, without the registry: `apply_memory_limit_ini(value, max)`
   returns `MemoryLimitIni::{Applied, ClampedToMax { warning }}` or the
   `SetLimitError` whose `Display` is the warning.)
3. `memory_hard_limit_percent` (registry handler is `None` today): on change
   call `php_memlimit::set_hard_limit_percent(v)` (`v` parsed with
   `parse_quantity`; unset = -1); on `Err(e)` emit `E_WARNING` `e.to_string()`
   ("memory_hard_limit_percent must be >= 100, or -1" /
   "Failed to set memory_hard_limit_percent to %d") and reject the change.

Semantics (`zend_set_memory_limit`): a limit below the current *real* usage
fails (so `ini_set('memory_limit', '1M')` fails with "Current memory usage
is 2097152 bytes"); `-1` and anything at or above `LIMIT_MAX` mean unlimited;
the limit applies to reported usage (current − request baseline +
`PHP_BASELINE`), so VM structures allocated before `request_start` do not
count.

## Approximations (documented, intentional)

- Bytes are Rust `Layout` sizes; Zend rounds to bin sizes and adds chunk
  overhead, and our values are differently sized than zvals, so absolute
  numbers differ from PHP. Tests compare deltas or use `%d`.
- `memory_get_usage(true)` is chunk-rounded usage, not mapped memory.
- `tried to allocate` is the Rust allocation (or reserved) size, not Zend's
  chunk/huge-block size.
- On clamping to `max_memory_limit` we apply the max as the limit (PHP
  leaves the heap limit unchanged).
