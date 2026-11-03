--TEST--
CLEAN runs after the test
--FILE--
<?php file_put_contents(__DIR__ . '/clean_marker.tmp', 'x'); echo "written"; ?>
--CLEAN--
<?php unlink(__DIR__ . '/clean_marker.tmp'); ?>
--EXPECT--
written
