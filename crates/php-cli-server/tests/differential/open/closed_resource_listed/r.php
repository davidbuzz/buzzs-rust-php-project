<?php
$f = fopen("php://memory","r"); fclose($f);
foreach (get_resources() as $k => $r) echo $k, " ", get_resource_type($r), "\n";
