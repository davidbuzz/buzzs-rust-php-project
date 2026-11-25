# Open differences

Scenarios here differ from the C implementation and are not fixed yet.

- `closed_resource_listed`: `get_resources()` after `fclose($f)` still
  lists the closed resource (type `Unknown`) in C while `$f` refers to it;
  ours drops it (CLI too). Under CGI and FastCGI, C also lists resource #1,
  the primary script's stream (`php_fopen_primary_script()`), which ours
  numbers past but does not list.
