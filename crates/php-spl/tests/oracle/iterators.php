<?php
$a = new ArrayIterator(range(1, 10));
foreach (new LimitIterator($a, 2, 3) as $k => $v) echo "$k=$v ";
echo "\n";
$l = new LimitIterator($a, 2, 3);
try { $l->seek(1); } catch (OutOfBoundsException $e) { echo $e->getMessage(), "\n"; }
try { $l->seek(5); } catch (OutOfBoundsException $e) { echo $e->getMessage(), "\n"; }
var_dump($l->seek(3), $l->current(), $l->getPosition());
foreach (new CallbackFilterIterator($a, fn($v, $k) => $v % 3 == 0) as $k => $v) echo "$k=$v ";
echo "\n";
$c = new CachingIterator(new ArrayIterator(['a', 'b', 'c']));
foreach ($c as $v) { echo $v, $c->hasNext() ? ',' : "\n"; }
echo (string) $c, "|\n";
$c = new CachingIterator(new ArrayIterator(['x' => 1, 'y' => 2]), CachingIterator::FULL_CACHE);
foreach ($c as $v) {}
var_dump($c->getCache(), count($c), $c['x']);
try { (string) $c; } catch (BadMethodCallException $e) { echo $e->getMessage(), "\n"; }
$ap = new AppendIterator();
$ap->append(new ArrayIterator([1, 2]));
$ap->append(new ArrayIterator([]));
$ap->append(new ArrayIterator([3]));
foreach ($ap as $k => $v) echo $ap->getIteratorIndex(), ":$k=$v ";
echo "\n";
$i = new InfiniteIterator(new ArrayIterator([1, 2]));
$n = 0;
foreach ($i as $v) { echo $v; if (++$n > 5) break; }
echo "\n";
$nr = new NoRewindIterator(new ArrayIterator([1, 2, 3]));
$nr->next();
foreach ($nr as $v) echo $v;
echo "\n";
$r = new RegexIterator(new ArrayIterator(['apple', 'banana', 'cherry', 'avocado']), '/^a/');
var_dump(iterator_to_array($r));
$r = new RegexIterator(new ArrayIterator(['a1b2', 'c3']), '/(\w)(\d)/', RegexIterator::ALL_MATCHES);
var_dump(iterator_to_array($r));
$r = new RegexIterator(new ArrayIterator(['a,b', 'c']), '/,/', RegexIterator::SPLIT);
var_dump(iterator_to_array($r));
$r = new RegexIterator(new ArrayIterator(['a1', 'b2']), '/\d/', RegexIterator::REPLACE);
$r->replacement = '#';
var_dump(iterator_to_array($r));
$r = new RegexIterator(new ArrayIterator(['k1' => 'x', 'z' => 'y']), '/\d/', RegexIterator::MATCH, RegexIterator::USE_KEY | RegexIterator::INVERT_MATCH);
var_dump(iterator_to_array($r));
$tree = new RecursiveTreeIterator(new RecursiveArrayIterator(['a' => 1, 'b' => ['c' => 2, 'd' => ['e' => 3]], 'f' => 4]));
foreach ($tree as $k => $v) echo "[$k] $v\n";
$tree = new RecursiveTreeIterator(new RecursiveArrayIterator([1, [2, 3]]), 0);
foreach ($tree as $k => $v) echo "[$k] $v\n";
$rii = new RecursiveIteratorIterator(new RecursiveArrayIterator([1, [2, [3, 4]], 5]), RecursiveIteratorIterator::CHILD_FIRST);
foreach ($rii as $k => $v) echo $rii->getDepth(), ':', is_array($v) ? 'A' : $v, ' ';
echo "\n";
$rii->setMaxDepth(0);
foreach ($rii as $v) echo is_array($v) ? 'A' : $v, ' ';
echo "\n";
var_dump($rii->getMaxDepth());
try { $rii->setMaxDepth(-5); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
var_dump(iterator_count(new ArrayIterator([1, 2, 3])), iterator_apply($a, function () { return true; }));
try { foreach (new EmptyIterator as $x) {} (new EmptyIterator)->current(); } catch (BadMethodCallException $e) { echo $e->getMessage(), "\n"; }
$p = new ParentIterator(new RecursiveArrayIterator([1, [2], 3, [4, [5]]]));
foreach (new RecursiveIteratorIterator($p, RecursiveIteratorIterator::SELF_FIRST) as $k => $v) echo "$k ";
echo "\n";
var_dump(class_implements('ArrayIterator'), class_parents(new RecursiveArrayIterator([])));
