--TEST--
stdout and stderr are merged in order
--FILE--
<?php
echo "a\n";
fwrite(STDERR, "b\n");
echo "c\n";
?>
--EXPECT--
a
b
c
