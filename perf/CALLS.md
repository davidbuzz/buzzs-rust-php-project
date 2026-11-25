# The call protocol: why it is next, and the plan

Measured 25 Nov 2026 on AArch64 (WSL2), release builds, user-space
instruction counts. C is PHP 8.5.13 (`php:8.5-cli`, opcache off, no
JIT). Ours runs with `BUZZS_JIT=0` unless stated: the trace JIT changes
nothing on Laravel (unit tests 178.9G instructions without it, 179.4G
with it).

## Where Laravel's time goes

`artisan route:list`, run cold and then with every script loaded from
opcache's file cache, which leaves out compiling (lexing, parsing,
compiling, optimising):

| `route:list` | C 8.5 | ours | ratio |
|---|---|---|---|
| cold | 1.621G | 4.258G | 2.6× |
| scripts from the file cache | 0.687G | 2.909G | **4.2×** |
| difference: compiling, roughly | 0.934G | 1.349G | 1.4× |

Compiling is near C. The gap is in running the code.

callgrind of the file-cache run (2.907G), each function's own
instructions sorted into groups:

| group | share |
|---|---|
| calls: frames, arguments, returns, the tape's call ops | ~34% |
| interpreter dispatch (`run_tape`, `step_ext`, `Vm::run`) | ~10% |
| reference counting, drops, the cycle collector's buffer | ~8% |
| strings (hashing, copying) | ~7% |
| properties and objects | ~6% |
| hash tables | ~6% |
| allocation | ~4% |
| everything else, none above 1.5% | the rest |

About 3.2M user calls run in it. A third of 2.9G spread over 3.2M calls
is ~310 instructions of call machinery per call. C's *whole* run
averages 215 instructions per user call, the function bodies included.

## One call, exactly

A micro-benchmark: `$s = f($i)` in a loop, the same with a method, and
the empty loop, whose cost is subtracted.

```php
<?php
namespace App;
function f($a) { return $a; }
class K { public $p = 1; function m($a) { return $a; } }
function bench($n, $mode) {
    $k = new K; $s = 0;
    if ($mode === 'f') { for ($i = 0; $i < $n; $i++) { $s = f($i); } }
    elseif ($mode === 'm') { for ($i = 0; $i < $n; $i++) { $s = $k->m($i); } }
    else { for ($i = 0; $i < $n; $i++) { $s = $i; } }
    return $s;
}
bench((int)$argv[1], $argv[2]);   // php call.php 200000 f|m|none
```

| | C 8.5 | ours | gap |
|---|---|---|---|
| `f($i)` | 142 | 853 | +711 |
| `$k->m($i)` | 178 | 1,274 | +1,096 |

Where the 853 go (callgrind, each function's own instructions per call;
code inlined into a function counts as that function's):

| function | per call |
|---|---|
| `Vm::return_simple` (incl. the frame's reset) | 289 |
| `tape::run_call` | 152 |
| `calls::send` (one int argument) | 122 |
| `calls::enter` | 99 |
| `run_tape` (the call ops' dispatch) | 62 |
| `calls::ready_frame` | 58 |
| `calls::cv_value`, `compiled`, `CachedFunc::get`, drops | ~70 |

No line costs more than 8. The cost is the count: a `Frame` has about
30 independent fields (nine `Vec`s, a dozen `Option`s, flags), and each
call checks, readies and resets each of them, each check a few
instructions. php-src's `zend_execute_data` is one block on the VM
stack: a header the call fills in, then the CVs and temporaries, sized
by the op array; `RETURN` releases the CVs and moves the stack pointer
back.

## Why not a whole-function JIT first

A baseline JIT removes dispatch: about 10% of the run. The code it emits
would still call the same frame setup, argument passing and return,
which are a third. Every JIT design needs a lean call protocol under it,
so the protocol comes first either way; done first, it speeds up the
interpreter as well.

## What failed before, and how this plan avoids it

See `perf/OPTIMIZATIONS.md` ("Dead ends"). Briefly: a fused call/return
over the existing `Frame` got 712 to 602; a frame state word in the core
loop made calls 19-30% cheaper but loops 3-11% slower, because anything
added inside `run_core` shares its register allocation; threaded
dispatch lost on stable Rust. So: nothing in this plan adds code to
`run_core`; the call ops already leave it (`WHY_CALL`), and all changes
are behind that exit.

## The plan

Each step is measured against the micro-benchmarks above and the
acceptance set, and validated on the PHPT set and Laravel, before the
next one starts.

1. **A precomputed call shape.** Everything that decides whether a call
   is plain is known when the function is declared: generator, by-ref
   return or parameters, variadics, statics, `finally`, defaults,
   parameter types. The per-call checks become one precomputed word on
   `UserFunction`, with the frame sizes beside it.
2. **The clean-frame invariant.** A pooled frame above the stack top is
   always clean: no `$this`, scopes, pending calls, named arguments,
   silence state, attached symbol table or generator. The rare paths
   that set any of these clean up after themselves, so the plain call
   and return stop checking them. Return resets only what that
   function's code can have touched, by the counts in its shape.
3. **The cold state out of the frame.** The fields only rare paths use
   (named arguments, silence state, the symbol table's slot map,
   statics, the generator, the pending exception, the magic method's and
   builtin's names) move behind one pointer, `None` in a plain frame, so
   readying and resetting a frame touches a few words.
4. **One slot block per frame.** The compiled variables and the
   temporaries in one allocation, sized from the op array, the way
   php-src lays out `zend_execute_data`. The tape already addresses both
   through base pointers (`St::cvs`, `St::tmps`), so its code does not
   change.
5. **Argument passing into the callee's slots.** `send` writes the
   argument into the callee's compiled variable directly, as php-src's
   `SEND_VAL`/`SEND_VAR` do. Then `RECV` of an untyped parameter is
   nothing, and of a typed one only the check.

The target is C's 142 and 178. With steps 1 to 3, around 400 is
realistic; steps 4 and 5 are the ones that reach C's layout. After that,
a whole-function JIT is the next lever, over a call protocol it can use.

## How to measure

- `perf stat -e instructions:u` at two loop counts, the difference
  divided by the difference in counts: the per-iteration cost without
  startup.
- callgrind (`valgrind --tool=callgrind`, on a release build with
  `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`) gives exact counts per
  function and per source line; subtract a run of the empty loop.
- For C, `perf stat` in the `php:8.5-cli` container (run it with
  `--user $(id -u):$(id -g)` so perf may attach).
