<?php
// Every zend_object_iterator takes an object handle (zend_iterator_init):
// the inner iterator of a dual iterator, one per RecursiveIteratorIterator
// level, the foreach iterator, and the ones iterator_*() use.
function id() { return spl_object_id(new stdClass); }
$it = new IteratorIterator(new ArrayIterator([1]));
echo "IteratorIterator: ", id(), "\n";
$r = new RecursiveIteratorIterator(new RecursiveArrayIterator([1, [2, [3]], 4]));
echo "RecursiveIteratorIterator: ", id(), "\n";
foreach ($r as $v) {
    echo $v, " at depth ", $r->getDepth(), ": ", id(), "\n";
}
echo "after foreach: ", id(), "\n";
foreach ($r as $v) {
    if ($v == 3) {
        echo "break at depth ", $r->getDepth(), "\n";
        break;
    }
}
echo "after break: depth ", $r->getDepth(), ", ", id(), "\n";
$a = new stdClass;
$b = new stdClass;
$c = new stdClass;
echo spl_object_id($a), " ", spl_object_id($b), " ", spl_object_id($c), "\n";
$r->rewind();
$r->next();
$r->next();
echo "manual: depth ", $r->getDepth(), ", ", id(), "\n";
$r->next();
echo "manual: depth ", $r->getDepth(), ", ", id(), "\n";
$f = SplFixedArray::fromArray([1]);
$g = $f->getIterator();
echo "getIterator: ", spl_object_id($f), " ", spl_object_id($g), " ", id(), "\n";
echo "iterator_count: ", iterator_count($it), " ", iterator_count($r), ", ", id(), "\n";
