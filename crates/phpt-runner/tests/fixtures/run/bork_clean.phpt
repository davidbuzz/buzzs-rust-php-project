--TEST--
CLEAN output borks a passing test
--FILE--
<?php echo "ok"; ?>
--CLEAN--
<?php echo "garbage"; ?>
--EXPECT--
ok
