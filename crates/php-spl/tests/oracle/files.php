<?php
$dir = sys_get_temp_dir() . '/spl_oracle_' . getmypid();
@mkdir($dir);
file_put_contents("$dir/a.txt", "line1\nline2\n\nline4");
file_put_contents("$dir/b.csv", "a,b,c\n1,\"2,x\",3\n");
mkdir("$dir/sub");
file_put_contents("$dir/sub/c.txt", "c");
$f = new SplFileObject("$dir/a.txt");
foreach ($f as $n => $line) { var_dump($n, $line); }
$f->setFlags(SplFileObject::DROP_NEW_LINE | SplFileObject::SKIP_EMPTY | SplFileObject::READ_AHEAD);
foreach ($f as $n => $line) { var_dump($n, $line); }
$f->seek(1);
var_dump($f->current(), $f->key());
$f = new SplFileObject("$dir/b.csv");
$f->setFlags(SplFileObject::READ_CSV);
foreach ($f as $row) { var_dump($row); }
$t = new SplTempFileObject();
var_dump($t->fwrite("hello\nworld\n"), $t->ftell());
$t->rewind();
var_dump($t->fgets(), $t->fgetc(), $t->eof(), $t->fread(3));
var_dump($t->getFilename(), $t->getPathname());
$i = new SplFileInfo("$dir/a.txt");
var_dump($i->getFilename(), $i->getExtension(), $i->getBasename('.txt'), $i->isFile(), $i->isDir(), $i->getSize());
$i = new SplFileInfo('/a/b/c.tar.gz/');
var_dump($i->getPath(), $i->getFilename(), $i->getExtension(), $i->getPathname(), (string) $i->getPathInfo());
try { new SplFileObject("$dir/nope.txt"); } catch (RuntimeException $e) { echo str_replace($dir, 'DIR', $e->getMessage()), "\n"; }
try { new SplFileObject("$dir"); } catch (LogicException $e) { echo $e->getMessage(), "\n"; }
try { new DirectoryIterator("$dir/nope"); } catch (UnexpectedValueException $e) { echo str_replace($dir, 'DIR', $e->getMessage()), "\n"; }
$names = [];
foreach (new DirectoryIterator($dir) as $e) { if (!$e->isDot()) $names[] = $e->getFilename() . ($e->isDir() ? '/' : ''); }
sort($names);
var_dump($names);
$names = [];
foreach (new RecursiveIteratorIterator(new RecursiveDirectoryIterator($dir, FilesystemIterator::SKIP_DOTS)) as $p => $e) { $names[] = str_replace($dir, 'DIR', $p); }
sort($names);
var_dump($names);
$g = new GlobIterator("$dir/*.txt");
var_dump(count($g));
foreach ($g as $k => $v) echo str_replace($dir, 'DIR', $k), ' ', $v->getFilename(), "\n";
unlink("$dir/sub/c.txt"); rmdir("$dir/sub"); unlink("$dir/a.txt"); unlink("$dir/b.csv"); rmdir($dir);
