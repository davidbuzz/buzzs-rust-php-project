--TEST--
===DONE=== ends the FILE section
--FILE--
<?php echo "done\n"; ?>
===DONE===
this text is not part of the test
--EXPECT--
done
===DONE===
