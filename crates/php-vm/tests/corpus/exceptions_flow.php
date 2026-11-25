<?php
class E1 extends Exception {} class E2 extends E1 {}
function fin($i) { $log = []; try { try { $log[] = 'try'; if ($i % 2) { throw new E2("e$i"); } return $log; } finally { $log[] = 'inner-finally'; } } catch (E1 $e) { $log[] = get_class($e) . ':' . $e->getMessage(); } finally { $log[] = 'outer-finally'; } return $log; }
function retFinally($i) { $r = "none"; try { return "try$i"; } finally { $r = "finally$i"; echo "$r\n"; } }
function loopEx($n) { $c = 0; $msgs = []; for ($i = 0; $i < $n; $i++) { try { if ($i % 17 == 0) { throw new RuntimeException("r$i"); } $c += $i; } catch (RuntimeException $e) { $msgs[] = $e->getMessage(); continue; } finally { $c++; } } return [$c, $msgs]; }
function chain($i) { try { try { throw new LogicException("first$i"); } catch (LogicException $e) { throw new E1("second$i", 5, $e); } } catch (E1 $e) { return [$e->getMessage(), $e->getCode(), $e->getPrevious()->getMessage(), $e->getLine(), get_class($e->getPrevious())]; } }
function finallyOverride() { try { throw new E1('kept'); } catch (E1 $e) { return $e->getMessage(); } finally { echo "fo\n"; } }
function nestedGen() { try { yield 1; yield 2; } finally { echo "gen finally\n"; } }
function genEarly() { foreach (nestedGen() as $v) { if ($v == 1) { return "early$v"; } } }
function dtorOnThrow($i) { $o = new class { function __destruct() { echo "dtor\n"; } }; if ($i) { throw new E2('with dtor'); } return 'no throw'; }
function typedRet($v): int { return $v; }
for ($i = 0; $i < 4; $i++) {
  var_export(fin($i)); var_export(retFinally($i)); var_export(chain($i)); var_export(finallyOverride()); echo "\n";
  echo genEarly(), "\n";
  try { echo dtorOnThrow($i % 2), "\n"; } catch (E2 $e) { echo "caught ", $e->getMessage(), "\n"; }
  foreach ([1, '2', 3.0, 3.5, 'x', null] as $v) { try { var_export(typedRet($v)); } catch (TypeError $e) { echo $e->getMessage(); } echo "\n"; }
}
var_export(loopEx(160)); echo "\n";
try { (function () { throw new E2('closure'); })(); } catch (Exception $e) { echo get_class($e), $e->getLine(), "\n"; }
