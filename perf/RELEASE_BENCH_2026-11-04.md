# Release-build comparison vs system PHP 8.3.6 (4 Nov 2026)

Box under load ~10 from concurrent agent builds; both binaries run back-to-back with the same
scripts (`modules/php-src/Zend/bench.php`, `Zend/micro_bench.php`, and a custom
int/float/recursion/string/array/object script). Our binary: `cargo build --release -p php-cli`
at HEAD 01d791a. System: `/usr/bin/php` 8.3.6 CLI default (opcache off); the JIT row used
`-d opcache.enable_cli=1 -d opcache.jit=tracing -d opcache.jit_buffer_size=64M`.

| Metric | phprust (release) | PHP 8.3.6 (no opcache) | PHP 8.3.6 + JIT | ratio (ours / php) |
|---|---|---|---|---|
| binary size | 30.5 MB (26.8 MB stripped), static, all extensions built in | 5.8 MB + 13 MB of extension .so files | same | 1.6x of php+ext |
| startup `php -r 'echo 1;'` (avg of 20) | 50.6 ms | 36.5 ms | | 1.4x |
| startup peak RSS | 43 MB | 30 MB | | 1.4x |
| Zend/bench.php total | 8.51 s | 0.455 s | 0.174 s | 18.7x |
| Zend/micro_bench.php total | 46.8 s | 3.12 s | 1.54 s | 15x |
| int loop (5M iterations) | 2.03 s | 0.146 s | 0.091 s | 14x |
| float math (2M sqrt/sin) | 4.46 s | 0.187 s | 0.164 s | 24x |
| fib(27) recursion | 0.63 s | 0.014 s | 0.008 s | 45x |
| strings (1M concat + md5 + str_replace) | 0.86 s | 0.054 s | 0.054 s | 16x |
| arrays (1M push, sort, array_map/sum) | 2.04 s | 0.474 s | 0.540 s | 4.3x |
| objects (1M new + method call) | 5.28 s | 0.162 s | 0.145 s | 33x |
| peak RSS, custom script | 193 MB | 88 MB | | 2.2x |

bench.php per test (seconds, ours / php): simple 0.196/0.012, simplecall 0.391/0.005,
simpleucall 0.472/0.018, mandel 1.379/0.079, ackermann 0.450/0.019, ary3 0.727/0.044,
fibo 1.355/0.067, hash2 0.081/0.007, heapsort 0.339/0.025, matrix 0.310/0.022,
nestedloop 0.266/0.018, sieve 0.225/0.013, strcat 0.023/0.004.

Reading: the libraries (hashing, sorting, PCRE, string algorithms) are close to C; the gap is
the interpreter loop (see README.md in this directory: ~600-800 instructions per simple opcode
in release vs ~15 in php-src) and per-call overhead (function/method calls and object
construction are the worst ratios). Reproduce with `scratchpad/bench/run.sh <binary> <label>`
(script text is in this file's git history if the scratchpad is gone).
