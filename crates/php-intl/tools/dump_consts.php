<?php
$ext = new ReflectionExtension('intl');
foreach ($ext->getConstants() as $k => $v) echo "global\t$k\t", var_export($v, true), "\n";
foreach ($ext->getClasses() as $c) {
    foreach ($c->getReflectionConstants() as $rc) {
        if ($rc->getDeclaringClass()->getName() !== $c->getName()) continue;
        echo $c->getName(), "\t", $rc->getName(), "\t", var_export($rc->getValue(), true), "\n";
    }
}
