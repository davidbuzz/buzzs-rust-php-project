--TEST--
Timeout
--FILE--
<?php while (true) { usleep(1000); } ?>
--EXPECT--
never
