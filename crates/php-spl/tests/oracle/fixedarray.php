<?php
$a = new SplFixedArray(3);
$a[0] = 'a';
$a["1"] = 'b';
$a[2.0] = 'c';
foreach ([0, "1", 1.5, true, false, "x", -1, 3, null, [], new stdClass] as $k) {
    try { var_dump($a[$k]); } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
}
var_dump(isset($a[0]), isset($a[5]), empty($a[1]));
$a->setSize(5);
var_dump($a->toArray(), $a->getSize());
$a->setSize(1);
var_dump($a);
try { $a[] = 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { new SplFixedArray(-1); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
var_dump(SplFixedArray::fromArray([3 => 'x', 1 => 'y']));
var_dump(SplFixedArray::fromArray([3 => 'x', 1 => 'y'], false));
try { SplFixedArray::fromArray(['a' => 1]); } catch (InvalidArgumentException $e) { echo $e->getMessage(), "\n"; }
try { SplFixedArray::fromArray([-1 => 1]); } catch (InvalidArgumentException $e) { echo $e->getMessage(), "\n"; }
echo json_encode(SplFixedArray::fromArray([1, 2])), "\n";
echo serialize(SplFixedArray::fromArray([1, 'a'])), "\n";
var_dump(unserialize('O:13:"SplFixedArray":2:{i:0;i:5;i:1;i:6;}'));
$c = clone SplFixedArray::fromArray([1, 2, 3]);
foreach ($c as $k => $v) echo "$k:$v ";
echo "\n", count($c), "\n";
