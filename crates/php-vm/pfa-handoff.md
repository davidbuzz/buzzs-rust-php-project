# Partial function application infrastructure handoff

Base: `1c63a52`, worktree `/home/buzz/phprust-wt-goal-pfa`.

The requested 97.8% -> 100% goal is **blocked**, not achieved or remeasured.
No selectors were excluded, no baseline/percentage files were changed, and
no preload substitution or observer output was added. Runtime code is unchanged.
The new regressions are unexecuted, single-request VM coverage; they are not
preloading or observer conformance evidence.

## Exact remaining selectors

All reference paths below are under the read-only
`/home/buzz/phprust/modules/php-src`.

- `Zend/tests/partial_application/constexpr_015.phpt`
- `Zend/tests/partial_application/preloading.phpt`
- `Zend/tests/partial_application/observers.phpt`

### Preloading (first two selectors)

Both PHPTs configure `opcache.preload` to their sibling `.inc`, enable
OPcache for CLI, and require normal script execution to see its declarations.
`preloading.inc` also executes its PFA during startup, producing the first
`int(2)`; the request produces the second. `constexpr_015.inc` declares PFA
class constants, class/method attributes and a method parameter default.

Source evidence:

- `ext/opcache/ZendAccelerator.c:4964-5000`: startup shutdown functions,
  destructors and output flushing, executor-value cleanup, removal of
  nonpersistent constants, a fresh symbol table, then declaration linking.
- `ext/opcache/ZendAccelerator.c:5036-5053`: move functions/classes into a
  persistent script, optimize, persist and load it.
- `ext/opcache/ZendAccelerator.c:4761` (`preload_load`): install declarations
  into process tables.
- `ext/opcache/zend_persist.c:197-209`: persist callable-conversion AST
  metadata and mark the partial as shared-cacheable.
- `Zend/zend_partial.c:1050-1070,1134-1182`: cache/retrieve compiled partial
  bodies, create a scoped closure and bind its arguments.

Current implementation:

- `crates/php-cli/src/main.rs:657-735` creates a fresh VM for a request.
  `new_vm` at line 573 registers extensions/SAPI, without a preload phase.
- `crates/php-vm/src/lib.rs:252-335` has request startup/shutdown and ordinary
  auto-prepend/append, not persistent declaration promotion or startup
  executor-state isolation.
- `crates/php-cli/src/startup.rs:180-188` merely accepts an `opcache` Zend
  extension name. That is not OPcache or preloading implementation.
- `crates/php-vm/src/include.rs:157-190` retains units/declarations/globals
  within the same VM; running a second unit is not a new preload/request
  boundary.

Genuine minimum support requires a startup execution phase with its own
request state, normal startup output/shutdown behavior, declaration
promotion/linking and retained code/constant-expression metadata, followed
by a fresh request state. Runtime globals, user constants, shutdown
callbacks and initialized runtime values must not leak across that
boundary. PFA code and scopes must remain valid, including lazily evaluated
attribute/default expressions and closures first created in the request.
Physical shared-memory machinery is not itself necessary for CLI language
semantics, but declaration persistence and request isolation are.

CLI coordination needed: configure and execute preload once before normal
request creation/execution, propagate startup failures and output, and
install the resulting persistent declarations in request VMs. There is no
existing VM preload API to call; its ownership/representation needs an
infrastructure implementation. Do not map `opcache.preload` to
`auto_prepend_file`, concatenate sources, or reuse a shutdown VM unchanged.

### Observers (third selector)

Source evidence:

- `ext/zend_test/observer.c:75-99,209-233,420-428`: output is produced by
  registered function-call init/begin/end callbacks, not PFA-specific text.
- The fixture observes the file, generated PFA closure, target `f` and
  native `var_dump`, with no extra generated-source execution event.
- `crates/php-ext-zendtest/src/lib.rs:27-28,64-65` gates the printer behind
  `vm-hooks`; `crates/php-cli/Cargo.toml` does not enable that feature.
- `crates/php-ext-zendtest/src/observer.rs:11,306-324` requires
  `ObservedFrame`, `ObserverHooks`, `Vm::set_observer` and
  `Vm::current_exception`. These are absent from compiled VM source.
- `crates/php-ext-zendtest/proposals/vm-hooks.diff:1-30` describes an
  unapplied cross-cutting observer proposal, not a usable API.
- `crates/php-vm/src/pfa.rs:624-697` compiles and executes a synthetic
  `FrameKind::Eval` to construct the closure. Future hooks must not report
  that implementation frame as a user eval/file invocation.

Genuine minimum support is an engine-level call-observer lifecycle:
per-function/file initialization, balanced begin/end around user and native
calls and main/include/eval execution, return/exception information,
reentrancy protection and correct unwinding. Generator/fiber suspension and
resume must not unbalance active observations; synthetic PFA construction
and other VM-only frames must be distinguished from actual user calls.
Existing generated closure identity must be used, not an observer-specific
hardcoded name.

CLI/extension coordination needed: after the VM API is implemented and
reviewed, enable the existing `php-ext-zendtest` `vm-hooks` feature.
That feature also gates internal-attribute support, so enabling it requires
the companion APIs documented by the proposal; toggling it alone is not a
working observer implementation. Do not blindly apply the older proposal
against the current pooled-frame/unwinding implementation.

## Feasible VM coverage delivered

`crates/php-vm/tests/pfa_lifecycle.rs` uses existing `Vm::run_main` and
`Vm::shutdown` APIs, with two compilation units and one request shutdown:

- `pfa_values_and_factories_survive_the_declaring_unit`: retained function
  partials, factories, a captured closure target, and no target invocation
  during PFA construction.
- `pfa_constexpr_values_survive_the_declaring_unit`: an already evaluated
  class constant plus later class/method attribute and default evaluation,
  preserving closure values after their declaring execution frame is gone.

These isolate PFA lifetime prerequisites from the missing infrastructure;
they deliberately do not pretend to exercise preload/request isolation.

## Verification and next selectors

Only static source/diff inspection was allowed. No cargo, build, test,
format, clippy, PHPT or performance commands were executed.
The bounded oracle version probe
`/usr/bin/php -d memory_limit=1G -r 'echo PHP_VERSION, "\n";'`
reported `8.3.6`, which cannot oracle PHP 8.7 PFA syntax.

Future validation selectors, not executed here:

- VM integration target `pfa_lifecycle`, both exact test names above.
- The three exact remaining PHPT paths above, with their INI/extensions
  honored and unchanged scope.
- Full `Zend/tests/partial_application` after infrastructure integration.

The architect/infrastructure owners need to implement persistent startup
declarations/request isolation and the engine observer API. Until then,
retain the reported 97.8% and all three blockers; do not claim 100%.
