--TEST--
EXPECTREGEX
--FILE--
<?php echo "abc123"; ?>
--EXPECTREGEX--
[a-c]+\d{3}
