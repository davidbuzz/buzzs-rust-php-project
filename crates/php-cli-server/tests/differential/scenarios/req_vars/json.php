<?php
$a = file_get_contents('php://input');
$b = file_get_contents('php://input');
var_dump($a === $b, json_decode($a, true), $_POST);
$f = fopen('php://input', 'r'); var_dump(fread($f, 3), stream_get_contents($f)); fclose($f);
