<?php
// Prints a canonical listing of an archive (compare with listing() in oracle.rs).
// usage: php list.php <archive> <phar|data>
$fname = $argv[1];
try {
    $p = $argv[2] === 'data' ? new PharData($fname) : new Phar($fname);
} catch (Exception $e) {
    echo "EXCEPTION ", get_class($e), ": ", $e->getMessage(), "\n";
    exit;
}
$meta = fn($o) => $o->hasMetadata() ? serialize($o->getMetadata()) : 'NONE';
echo "version: ", $p->getVersion(), "\n";
echo "alias: ", var_export($p->getAlias(), true), "\n";
echo "count: ", count($p), "\n";
$sig = $p->getSignature();
echo "signature: ", $sig ? $sig['hash_type'] . ' ' . $sig['hash'] : 'none', "\n";
echo "compressed: ", var_export($p->isCompressed(), true), "\n";
echo "format: ", $p->isFileFormat(Phar::ZIP) ? 'zip' : ($p->isFileFormat(Phar::TAR) ? 'tar' : 'phar'), "\n";
echo "metadata: ", $meta($p), "\n";
echo "stub: ", md5($p->getStub()), " ", strlen($p->getStub()), "\n";
$prefix = 'phar://' . $p->getPath() . '/';
try {
foreach (new RecursiveIteratorIterator($p, RecursiveIteratorIterator::SELF_FIRST) as $f) {
    $name = substr($f->getPathname(), strlen($prefix));
    $line = "entry $name dir=" . (int)$f->isDir();
    $line .= " size=" . $f->getSize() . " perms=" . decoct($f->getPerms());
    $line .= " mtime=" . $f->getMTime();
    $line .= " csize=" . $f->getCompressedSize();
    $line .= " gz=" . (int)$f->isCompressed(Phar::GZ) . " bz2=" . (int)$f->isCompressed(Phar::BZ2);
    $line .= " flags=" . $f->getPharFlags();
    $line .= " crc=" . (!$f->isDir() && $f->isCRCChecked() ? sprintf('%u', $f->getCRC32()) : '-');
    $line .= " meta=" . $meta($f);
    $line .= " md5=" . ($f->isDir() ? '-' : md5($f->getContent()));
    echo $line, "\n";
}
} catch (Throwable $e) {
    echo "ITER EXCEPTION ", get_class($e), ": ", $e->getMessage(), "\n";
}
