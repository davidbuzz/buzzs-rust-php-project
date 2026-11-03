<?php
$s = new SplObjectStorage();
$o1 = new stdClass; $o2 = new stdClass; $o3 = new stdClass;
$s[$o1] = 'one';
$s->offsetSet($o2, [2]);
$s[$o3] = null;
var_dump(count($s), isset($s[$o3]), empty($s[$o3]), $s[$o1], $s->offsetExists($o2));
$s->rewind();
while ($s->valid()) { var_dump($s->key(), $s->getInfo()); $s->setInfo('new' . $s->key()); $s->next(); }
var_dump($s);
$t = new SplObjectStorage();
$t[$o2] = 'x';
$s->removeAll($t);
var_dump(count($s));
$s->addAll($t);
var_dump(count($s));
$s->removeAllExcept($t);
var_dump(count($s));
try { $s[new stdClass]; } catch (UnexpectedValueException $e) { echo $e->getMessage(), "\n"; }
try { $s['x'] = 1; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
var_dump($s->__serialize());
$m = new MultipleIterator(MultipleIterator::MIT_KEYS_ASSOC | MultipleIterator::MIT_NEED_ANY);
$m->attachIterator(new ArrayIterator([1, 2, 3]), 'a');
$m->attachIterator(new ArrayIterator([4, 5]), 'b');
foreach ($m as $k => $v) { var_dump($k, $v); }
try { $m->attachIterator(new ArrayIterator([]), 'a'); } catch (InvalidArgumentException $e) { echo $e->getMessage(), "\n"; }
$m->setFlags(MultipleIterator::MIT_NEED_ALL);
$m->rewind();
var_dump($m->valid(), $m->countIterators());
