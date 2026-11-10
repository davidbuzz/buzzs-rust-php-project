--TEST--
POST_RAW multipart upload
--POST_RAW--
Content-Type: multipart/form-data; boundary=---------------------------20896060251896012921717172737
-----------------------------20896060251896012921717172737
Content-Disposition: form-data; name="field"

value
-----------------------------20896060251896012921717172737
Content-Disposition: form-data; name="file"; filename="a.txt"
Content-Type: text/plain

contents
-----------------------------20896060251896012921717172737--
--FILE--
<?php
var_dump($_POST["field"], $_FILES["file"]["name"], file_get_contents($_FILES["file"]["tmp_name"]));
?>
--EXPECT--
string(5) "value"
string(5) "a.txt"
string(8) "contents"
