--TEST--
SKIPIF skip
--SKIPIF--
<?php die("skip not today"); ?>
--FILE--
<?php echo "never"; ?>
--EXPECT--
x
