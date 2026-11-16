# Code review: tape performance series (`bd05a529..e642f664`) + uncommitted call-entry work

Reviewed 2026-11-13; re-verified 2026-11-16 (no new commits; working tree unchanged).

**Verdict: the series under review — committed and uncommitted — is good to ship.**
Seven test failures exist on `main`, but all are pre-existing (they fail identically at
HEAD with the uncommitted changes stashed) and trace to Tuesday-or-earlier merges,
not to this series.

---

## 1. Commits reviewed

| Commit | What it does | Assessment |
|---|---|---|
| `4c7da497` | Builtin calls by name run on the tape (`call_native`), `send_following`, `plain_call` caching, `release_native` | Sound |
| `c558cf73` | `/ % << >> \| & ^` and numeric comparisons on the tape | Sound |
| `bd05a529` | Out-of-line tape handlers called with the frame alone | Sound |
| `065f7bfd` | Property reads into a variable; no per-access op copy | Sound |
| `f1bc66c0` | Property read cache hit run directly via `read_hit` | Sound |
| `f8426f91` | Array dim read/write fast paths (`FetchDimIC/SC`, `AssignDimIC`) | Sound |
| `e642f664` | Number ops compute two ints by operator index (`num_result`/`int_result`) | Sound |

### Cross-checks performed

- **`e642f664`** (`crates/php-vm/src/exec/tape/scalar.rs`): `int_result` correctly
  declines `PHP_INT_MIN / -1` (checked path returns `None`); the unit test
  cross-checks every operator against `exec::num_bin`.
- **`f8426f91`** (`crates/php-vm/src/exec/tape/dims.rs`): fast paths correctly handle
  aliasing, shared arrays (`get_mut_unshared`), reference/non-plain elements
  (`is_plain`), and symbol tables; anything else declines to the generic op.
- **`f1bc66c0`/`065f7bfd`** (`crates/php-vm/src/exec/tape/props.rs`): the `read_hit`
  route matches the spec handler `prop_op_in` (`crates/php-vm/src/exec/spec/props.rs`),
  which likewise only honors `List` mode — tape and spec cannot diverge. Tape decline
  falls back to the generic op.
- **`4c7da497`**: special cases (by-ref params, coercion, deprecated, `#[\NoDiscard]`,
  frameless, observers) still route through the generic `PendingCall`; the
  `hot::THREW` sentinel keeps `run_tape`'s result two registers wide.
- Commit messages document Ir/iter measurements vs C PHP; every fast path documents
  its decline conditions. High-quality series.

## 2. Uncommitted working-tree changes (`crates/php-vm/src/exec/tape.rs`, `crates/php-vm/src/vm.rs`)

Early call-entry route (`send_and_enter` / `ENTERED`, the `WHY_CALL` arm) plus inline
arg-buffer clear in `Frame::release_direct` and `#[inline(always)]` on `keeps_function`.

- Mirrors the main loop's DoCall-entry handling: fresh/switched frames, safe points,
  ip/code reload; the native `Err` path falls back to prep → loop.
- The inline arg clear is behaviorally equivalent to `clear_arg_buffer`.
- Compiles; passes everything the committed tree passes: SPL phpt suite green (1.8 s),
  php-vm lib, php-stdstr, php-spl, php-hash, php-runtime suites green.

## 3. Pre-existing test failures on main (7) — need follow-up

All reproduce at HEAD with the uncommitted changes stashed; deterministic.

### Stale expectations from the opcode-shape merge (`d7f4856b` + explicit-END implicit returns)

1. `compiler::optimizer::dump::tests::ssa_dump_after_dfa_pass_infers_ranges` —
   dump now carries an extra unreachable trailing `RETURN null` BB (`lines=6` vs 5).
2. `compiler::optimizer::tests::sccp_partial_array_join_folds_shared_element` —
   asserts on old `Op::Fetch`/`Op::Free`; compiler now emits `FetchDimCv`/`AssignDimCv`.
   The folds themselves still fire.
3. `compiler::optimizer::tests::sccp_partial_array_from_dim_writes_of_undefined_local` —
   same stale opcode variants (dump shows `Echo(Lit(6))`, i.e. the fold works).
4. `compiler::optimizer::tests::dce_drops_foreach_over_empty_literal_array` —
   **more than stale**: the foreach-over-`[]` drop is no longer applied
   (full `FeReset/FeFetch/FeFree` loop survives). Likely a genuine optimization
   regression introduced by the opcode-shape change; investigate.

### Stale counts

5. `special_ops::defined_in_array_array_key_exists_chr_ord_compile_by_their_literals` —
   expects 13 `DoCall`s, gets 4. Literal folding made these better; re-baseline.

### Broken on landing (never could have passed as checked in)

6. `ucalls::precomputed_first_op_matches_every_argument_count` — calls `json_encode`
   on the bare test VM, which does not link ext/json (the file's own comment at
   `crates/php-vm/tests/ucalls.rs:498` says so).
7. `ucalls::static_closures_with_a_cached_object_take_the_plain_call_path` — calls
   `array_sum`, likewise not registered on the bare VM. Whatever gate landed these
   (Tue commits `3cc469f8`/`94462d3e`) did not run them.

## 4. Environment note: the two SPL phpt "regressions" are not code

`bug70561.phpt` and `dit_006.phpt` harness-timeout because those tests scan
`__DIR__ . '/..'` = `/tmp`, which on this machine holds ~273k entries (~66 GB, incl.
`/tmp/409923` scratch dirs). With a clean temp dir they pass:

```sh
TMPDIR=/tmp/opencode/cleantmp cargo test -p php-spl --test phpt   # 1.7 s, all green
```

Cleaning `/tmp` is recommended regardless.

## 5. Suggested follow-ups

1. Re-baseline the four optimizer test expectations (or fix the dce_drops_foreach
   regression first — it is the only behavior change among them).
2. Fix the two broken `ucalls` tests: stub `array_sum`/`json_encode` in the test VM
   or rewrite the scripts to avoid them.
3. Re-baseline the `special_ops` DoCall count (4, post-folding).
4. Clean `/tmp` (or keep redirecting `TMPDIR` for phpt runs).
5. Commit the uncommitted call-entry work once its perf numbers are recorded,
   following the series' commit-message convention.
