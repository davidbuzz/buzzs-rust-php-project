<?php
foreach (get_loaded_extensions() as $e) {
    $r = new ReflectionExtension($e);
    $f = array_keys($r->getFunctions()); sort($f);
    $c = $r->getClassNames(); sort($c);
    $k = array_keys($r->getConstants()); sort($k);
    echo $e, " f=", count($f), " c=", count($c), " k=", count($k), " ", md5(implode(",", $f) . "|" . implode(",", $c) . "|" . implode(",", $k)), "\n";
}
$d = get_defined_constants(true); ksort($d);
foreach ($d as $m => $cs) { echo "consts $m ", count($cs), "\n"; }
