<?php
class T{public int $i=-1; public ?int $ni=-1; public float $f=-1.5; public string $s='q'; public bool $b=true; public ?string $ns='q'; public int|string $u=-1; public array $a=[1]; public ?array $na=[1]; public mixed $m=-1; public int|float $if = -1; public static int $si=-1;}
function f() {
$t=new T;
foreach (['i','ni','f','s','b','ns','u','a','na','m','if'] as $p) {
  try { $t->$p -= []; } catch (Throwable $e) { echo $p, ': ', get_class($e), ' ', $e->getMessage(), ' prev=', $e->getPrevious() ? $e->getPrevious()->getMessage() : '-', "\n"; } var_dump($t->$p);
}
try { T::$si *= []; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump(T::$si);
$r = &$t->i; $t->i = -1; try { $r += []; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump($t->i);
$t->s = 'q'; try { $t->s .= []; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump($t->s);
$t->i = 5; try { $t->i %= 0; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump($t->i);
$t->i = 5; try { $t->i <<= -1; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump($t->i);
$t->i = 5; try { $t->i **= []; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump($t->i);
$t->i = 5; try { $t->i |= "a"; } catch (Throwable $e) { echo $e->getMessage(), "\n"; } var_dump($t->i);
}
f(); f();
