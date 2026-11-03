--TEST--
trivial with skipif
--SKIPIF--
<?php if (0) die("skip");
--FILE--
<?php echo "ok";
--CLEAN--
<?php echo "";
--EXPECT--
ok
