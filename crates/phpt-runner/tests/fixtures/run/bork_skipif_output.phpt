--TEST--
SKIPIF printing garbage
--SKIPIF--
<?php echo "garbage"; ?>
--FILE--
<?php ?>
--EXPECT--
