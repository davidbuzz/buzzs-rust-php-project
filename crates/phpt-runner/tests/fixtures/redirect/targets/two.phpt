--TEST--
redirected two
--SKIPIF--
<?php if (!getenv('REDIR_VALUE')) die('skip not redirected'); ?>
--FILE--
<?php echo "ok"; ?>
--EXPECT--
ok
