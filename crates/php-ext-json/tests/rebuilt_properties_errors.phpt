--TEST--
Rebuilt property recursion preserves JSON exceptions, partial output and guard cleanup
--EXTENSIONS--
date
json
--INI--
zend.max_allowed_stack_size=512K
error_reporting=0
--FILE--
<?php
$o = new DateInterval('P1D');
$o->self = $o;
try {
    json_encode($o, JSON_THROW_ON_ERROR);
} catch (JsonException $e) {
    var_dump($e->getCode(), $e->getMessage());
}
$o->self = [$o];
var_dump(json_encode($o), json_last_error());
json_encode($o, JSON_PARTIAL_OUTPUT_ON_ERROR | JSON_THROW_ON_ERROR);
var_dump(json_last_error());
$o->self = null;
json_encode($o);
var_dump(json_last_error());

$plain = new stdClass;
$plain->self = $plain;
var_dump(json_encode($plain), json_last_error());
var_dump(json_encode($plain, JSON_PARTIAL_OUTPUT_ON_ERROR));

class Hooked {
    public $self { get => $this; }
}
var_dump(json_encode(new Hooked), json_last_error());
?>
--EXPECT--
int(1)
string(28) "Maximum stack depth exceeded"
bool(false)
int(6)
int(6)
int(0)
bool(false)
int(6)
string(13) "{"self":null}"
bool(false)
int(6)
