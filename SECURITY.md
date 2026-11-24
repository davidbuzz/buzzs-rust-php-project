# Reporting Security Issues

Please report security vulnerabilities privately to the owner of this
repository. Do not open a public issue or pull request for them.

Vulnerability reports remain private until a fix is published.

# Classification

This project implements PHP's semantics, so it classifies issues as php-src
does. Issues commonly reported that are _not_ considered security issues
include (but are not limited to):

- Invocation of specially crafted, malicious code intended to cause memory
  violations. This commonly includes malicious error handlers, destructors or
  `__toString()` functions. PHP does not offer sandboxing, and the execution of
  untrusted code is always considered unsafe. Such issues are bugs, but not
  security issues. They may still be reported, though please avoid reporting
  the known issues.

- Passing malicious arguments to functions clearly not intended to receive
  unsanitized values, e.g. `mysqli_query()`. `escapeshellarg()` on the other
  hand should clearly be hardened against unsafe inputs.

- The use of legacy APIs or settings known to be insecure, particularly those
  documented as such, or those with a secure alternative.

- The use of FFI.

- `open_basedir` or `disable_functions` bypasses.

- Malicious `unserialize()` inputs.

- Memory exhaustion from a size the input declares, where `memory_limit`
  refuses the allocation and only the current request dies.

When creating reports, please **skip** the theatrics. Drop the impact essay,
send a short reproducer with the few lines that matter, and make each point
once. This allows us to triage and respond to your report quickly.

# Vulnerability Policy

The classification follows php-src's policy:
https://github.com/php/policies/blob/main/security-classification.rst

---

Adapted from php-src's `SECURITY.md`: Copyright © The PHP Group and
Contributors, under the Modified BSD License (BSD-3-Clause).
