--TEST--
SKIPIF printing xfail
--SKIPIF--
<?php echo "xfail because"; ?>
--FILE--
<?php echo "wrong"; ?>
--EXPECT--
right
