--TEST--
STDIN section
--STDIN--
line1
line2
--FILE--
<?php echo strtoupper(stream_get_contents(STDIN)); ?>
--EXPECT--
LINE1
LINE2
