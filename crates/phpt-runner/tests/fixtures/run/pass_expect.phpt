--TEST--
EXPECT with trailing whitespace normalisation
--FILE--
<?php
echo "hello\r\nworld\n\n   ";
?>
--EXPECT--
hello
world
