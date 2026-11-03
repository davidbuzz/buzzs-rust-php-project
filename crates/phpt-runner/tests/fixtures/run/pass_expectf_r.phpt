--TEST--
EXPECTF with %r regex sections
--FILE--
<?php echo PHP_INT_SIZE === 8 ? "int(9223372036854775807)" : "int(2147483647)"; ?>
--EXPECTF--
int(%r(9223372036854775807|2147483647)%r)
