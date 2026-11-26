<?php
// The call-protocol micro (TODO.md §B): one user call, exactly.
// Run at two loop counts N and 11N; (difference) / 10N is the per-iteration
// cost with startup cancelled; subtract the `none` mode for the loop body.

/* 
It's a measurement ruler, nothing more. Specifically:

**What it isolates.** User function/method calls are the single biggest performance gap in the interpreter — the census in the retired CALLS.md showed a plain user call costing ~1,000 instructions against ~100 for everything else in the same loop. Wall-clock timing is unreliable on this machine (NPR noise can add 500 ms to an empty process), so the metric is `perf stat -e instructions:u` at two loop counts, N and 11N: the difference divided by 10N cancels startup, and the `none` mode (identical loop, no call) cancels the loop body itself. That leaves the instruction cost of exactly one user call. The three modes and the two-count setup are easy to get wrong ad hoc — the file *is* the correct procedure, so you can't accidentally measure something else.

**What it's for going forward.** The call-protocol item in TODO.md §B is a sequence of small steps — (a) cold state behind a pointer, (b) one slot block per frame, (c) `send` writing straight into the callee's CVs. Each step is judged solely by this number: did the one-call cost drop, and did Laravel and the PHPT suite confirm it. That's how the item got from 1,274/call (user) and 853 (function) down to 178/142, with a target band of ~400–511 per call after the remaining steps. Without a fixed program, "the call got faster" becomes an unfalsifiable claim — the file turns it into one number, comparable across every change and across machines. It's also how the C reference (23/28 instructions via php-src's `zend_execute`) was measured for the same comparison.

**Why it lives in the repo rather than only in prose.** It's the source of the 853→142 and 1,274→178 figures now quoted in TODO.md and FEATURE_GAP.md. Anyone revisiting those numbers — you, a future contributor, a future session — needs the exact program that produced them. It follows the existing convention of `perf/interp/`, which already holds loop-micro.php, concat-micro.php, isset-micro.php and friends for exactly this role.

**What it is not.** It is not a regression test — nothing runs it automatically, and it must never grow: the whole value is that it stays two calls and an empty loop while everything around it gets rewritten. If you don't want the file, the honest cost is that those quoted numbers lose their reproducible source and the call-protocol item loses its named acceptance criterion.

**it doesn't integrate with anything today**
    
*/
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

bench((int)$argv[1], $argv[2]);   // php call-micro.php 200000 f|m|none
