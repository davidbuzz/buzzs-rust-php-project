--TEST--
ARGS section
--ARGS--
one "two words" three
--FILE--
<?php var_dump(array_slice($argv, 1)); ?>
--EXPECT--
array(3) {
  [0]=>
  string(3) "one"
  [1]=>
  string(9) "two words"
  [2]=>
  string(5) "three"
}
