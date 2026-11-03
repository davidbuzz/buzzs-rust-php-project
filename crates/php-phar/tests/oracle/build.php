<?php
// Builds an archive from a recipe (see tests/oracle.rs for the DSL).
// usage: php -d phar.readonly=0 build.php <recipe> <workdir>
// Prints the path of the resulting archive.
$recipe = file($argv[1], FILE_IGNORE_NEW_LINES | FILE_SKIP_EMPTY_LINES);
$dir = rtrim($argv[2], '/');
$p = null;
$path = null;
function arg($s) { return str_starts_with($s, 'hex:') ? hex2bin(substr($s, 4)) : $s; }
$algos = ['md5' => Phar::MD5, 'sha1' => Phar::SHA1, 'sha256' => Phar::SHA256, 'sha512' => Phar::SHA512];
$comps = ['gz' => Phar::GZ, 'bz2' => Phar::BZ2, 'none' => Phar::NONE];
$fmts = ['phar' => Phar::PHAR, 'tar' => Phar::TAR, 'zip' => Phar::ZIP];
foreach ($recipe as $line) {
    if ($line[0] === '#') continue;
    $w = explode(' ', $line);
    switch ($w[0]) {
        case 'new':
            $path = "$dir/" . $w[2];
            $alias = null; $format = 0;
            foreach (array_slice($w, 3) as $opt) {
                [$k, $v] = explode('=', $opt, 2);
                if ($k === 'alias') $alias = arg($v);
                if ($k === 'format') $format = $fmts[$v];
            }
            $p = $w[1] === 'data' ? new PharData($path, 0, $alias, $format) : new Phar($path, 0, $alias);
            break;
        case 'add': $p[arg($w[1])] = arg($w[2] ?? ''); break;
        case 'dir': $p->addEmptyDir(arg($w[1])); break;
        case 'stub': $p->setStub(arg($w[1])); break;
        case 'defaultstub': isset($w[1]) ? $p->setDefaultStub(arg($w[1]), arg($w[2])) : $p->setDefaultStub(); break;
        case 'meta': $p->setMetadata(unserialize(arg($w[1]))); break;
        case 'filemeta': $p[arg($w[1])]->setMetadata(unserialize(arg($w[2]))); break;
        case 'delmeta': $p->delMetadata(); break;
        case 'sig': $p->setSignatureAlgorithm($algos[$w[1]]); break;
        case 'compressfiles': $p->compressFiles($comps[$w[1]]); break;
        case 'decompressfiles': $p->decompressFiles(); break;
        case 'fcompress': $p[arg($w[1])]->compress($comps[$w[2]]); break;
        case 'fdecompress': $p[arg($w[1])]->decompress(); break;
        case 'compress': $p = $p->compress($comps[$w[1]]); $path = $p->getPath(); break;
        case 'decompress': $p = $p->decompress(); $path = $p->getPath(); break;
        case 'delete': $p->delete(arg($w[1])); break;
        case 'unset': unset($p[arg($w[1])]); break;
        case 'chmod': $p[arg($w[1])]->chmod(octdec($w[2])); break;
        case 'setalias': $p->setAlias(arg($w[1])); break;
        case 'buffer': $w[1] === 'start' ? $p->startBuffering() : $p->stopBuffering(); break;
        case 'copy': $p->copy(arg($w[1]), arg($w[2])); break;
        case 'convert':
            $m = isset($w[3]) ? $comps[$w[3]] : null;
            $p = $w[1] === 'data' ? $p->convertToData($fmts[$w[2]], $m) : $p->convertToExecutable($fmts[$w[2]], $m);
            $path = $p->getPath();
            break;
        default: fwrite(STDERR, "unknown op $w[0]\n"); exit(1);
    }
}
echo $path, "\n";
