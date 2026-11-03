<?php
$l = new SplDoublyLinkedList();
foreach ([1, 2, 3, 4] as $v) $l->push($v);
$l->unshift(0);
$l->add(2, 'x');
$l->add(6, 'end');
foreach ($l as $k => $v) echo "$k=>$v ";
echo "\n";
$l->setIteratorMode(SplDoublyLinkedList::IT_MODE_LIFO);
foreach ($l as $k => $v) echo "$k=>$v ";
echo "\n";
var_dump($l->offsetGet(0), $l->offsetExists(10), $l->getIteratorMode());
$l->offsetUnset(0);
var_dump(count($l), $l->top(), $l->bottom());
try { $l->offsetGet(100); } catch (OutOfRangeException $e) { echo $e->getMessage(), "\n"; }
try { $l->add(100, 1); } catch (OutOfRangeException $e) { echo $e->getMessage(), "\n"; }
$l->setIteratorMode(SplDoublyLinkedList::IT_MODE_FIFO | SplDoublyLinkedList::IT_MODE_DELETE);
foreach ($l as $k => $v) echo "$k=>$v ";
echo "\n", count($l), "\n";
$s = new SplStack();
try { $s->setIteratorMode(SplDoublyLinkedList::IT_MODE_FIFO); } catch (RuntimeException $e) { echo $e->getMessage(), "\n"; }
$s->push(1); $s->push(2); $s[] = 3;
echo $s->serialize(), "\n";
var_dump($s);
echo serialize($s), "\n";
$q = new SplQueue();
$q->enqueue('a'); $q->enqueue('b');
echo $q->dequeue(), count($q), "\n";
$l = new SplDoublyLinkedList();
$l->push(1); $l->push(2); $l->push(3);
$l->rewind();
$l->next();
$l->offsetUnset(1);
var_dump($l->valid(), $l->current(), $l->key());
$l->next();
var_dump($l->valid(), $l->current());
$u = new SplDoublyLinkedList();
$u->unserialize('i:0;:i:1;:s:1:"a";');
var_dump($u);
try { $u->unserialize('i:0;:x'); } catch (UnexpectedValueException $e) { echo $e->getMessage(), "\n"; }
