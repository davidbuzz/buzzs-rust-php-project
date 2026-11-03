<?php
$ao = new ArrayObject(['b' => 2, 'a' => 1, 3 => 'c']);
$ao['x'] = 'y';
$ao[] = 'z';
unset($ao['b']);
var_dump(count($ao), $ao->getArrayCopy());
$ao->ksort();
var_dump($ao->getArrayCopy());
$ao->uasort(function ($a, $b) { return strcmp((string)$a, (string)$b); });
var_dump($ao->getArrayCopy());
var_dump(isset($ao['a']), isset($ao['nope']), $ao->offsetExists('a'));
echo @$ao['nope'], "|\n";
$ao->setFlags(ArrayObject::ARRAY_AS_PROPS);
$ao->prop = 5;
var_dump($ao->prop, $ao['prop'], isset($ao->prop));
$it = $ao->getIterator();
var_dump(get_class($it));
foreach ($it as $k => $v) echo "$k=", is_array($v) ? 'arr' : $v, ' ';
echo "\n";
$ai = new ArrayIterator([1, 2, 3, 4]);
$ai->seek(2);
var_dump($ai->current(), $ai->key());
try { $ai->seek(10); } catch (OutOfBoundsException $e) { echo $e->getMessage(), "\n"; }
foreach ($ai as $k => $v) { if ($k == 1) { $ai->offsetUnset(2); } echo "$k=>$v\n"; }
var_dump(serialize(new ArrayObject([1, 2])));
var_dump(unserialize(serialize(new ArrayObject([1, 2]))));
$r = new RecursiveArrayIterator([1, [2, [3]], 4]);
foreach (new RecursiveIteratorIterator($r, RecursiveIteratorIterator::SELF_FIRST) as $k => $v) { echo $k, '=', is_array($v) ? 'array' : $v, "\n"; }
var_dump(new ArrayObject([1]) == new ArrayObject([1]), new ArrayObject([1]) == new ArrayObject([2]));
try { $ao->setIteratorClass('stdClass'); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
var_dump((array) new ArrayObject(['k' => 'v']));
