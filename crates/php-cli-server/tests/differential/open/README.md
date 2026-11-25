# Open differences

Scenarios here differ from the C implementation and are not fixed yet.

- `closed_resource_listed`: `get_resources()` after `fclose($f)` still
  lists the closed resource (type `Unknown`) in C while `$f` refers to it;
  ours drops it (CLI too). Under CGI and FastCGI, C also lists resource #1,
  the primary script's stream (`php_fopen_primary_script()`), which ours
  numbers past but does not list.
- `header_callback_exit`: `exit("text")` inside the `header_register_callback()`
  callable. C prints the exit text, then (CGI, FastCGI) sends the header
  block a second time and goes on with the script's next output; the
  built-in server differs again. Needs a reading of how `unwind_exit`
  leaves `sapi_send_headers()` before matching it.
