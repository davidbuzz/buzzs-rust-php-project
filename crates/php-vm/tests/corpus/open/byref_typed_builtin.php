<?php
// Known (being fixed elsewhere): a builtin's by-reference output written
// into a typed property is lost (from the first call for similar_text, from
// the second for str_replace's count). C writes the value, coerced to the
// property's type, or throws the TypeError.
class T { public ?int $ni = null; public string $s = 'abc'; public int|array $ia = 5; public static ?string $ss = null; }
function scalars($i) {
  $t = new T; $out = [];
  $out[] = similar_text('Hello', 'World', $t->ni); $out[] = $t->ni;
  try { $out[] = similar_text('ab', 'ab', $t->s); } catch (TypeError $e) { $out[] = $e->getMessage(); } $out[] = $t->s;
  $out[] = str_replace('a', 'b', 'banana', $t->ia); $out[] = $t->ia;
  try { $out[] = str_replace('a', 'b', 'banana', T::$ss); } catch (TypeError $e) { $out[] = $e->getMessage(); } $out[] = T::$ss;
  return $out;
}
for ($i = 0; $i < 3; $i++) { var_export(scalars($i)); echo "\n"; }
