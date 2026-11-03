<?php
$s = unserialize(base64_decode(getenv("GO_SPEC")));
$r = $s[1] === null ? getopt($s[0], [], $ri) : getopt($s[0], $s[1], $ri);
echo bin2hex(serialize([$r, $ri]));
