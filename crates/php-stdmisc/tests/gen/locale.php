<?php
// Fixture generator: setlocale return values, localeconv() and nl_langinfo()
// for the C and C.UTF-8 locales. Output: tag hex(var_export(result))
function out($tag, $v) { echo $tag, " ", bin2hex(var_export($v, true)), "\n"; }
foreach ([LC_ALL, LC_CTYPE, LC_NUMERIC, LC_TIME, LC_COLLATE, LC_MONETARY, LC_MESSAGES] as $cat) {
  foreach (["C", "POSIX", "C.UTF-8", "C.utf8", "0", "zz_ZZ"] as $loc) {
    setlocale(LC_ALL, "C");
    out("set $cat " . bin2hex(var_export($loc, true)), setlocale($cat, $loc));
  }
}
setlocale(LC_ALL, "C");
out("multi", setlocale(LC_ALL, "zz", "yy", "C.UTF-8", "C"));
out("arr", setlocale(LC_ALL, ["zz", "POSIX", "C"]));
out("arrnone", setlocale(LC_ALL, ["zz", "yy"]));
out("emptyarr", setlocale(LC_ALL, []));
out("bad", setlocale(99, "C"));
foreach (["C", "C.UTF-8"] as $loc) {
  setlocale(LC_ALL, $loc);
  out("conv $loc", localeconv());
  foreach (get_defined_constants(true)["standard"] as $name => $v) {
    if (preg_match('/^(AB)?(DAY|MON)_|_STR$|_FMT|^ERA|ALT_DIGITS|CURR|CRNCYSTR|^MON_|SIGN|DIGITS|PRECEDES|SEP_BY|POSN|DECIMAL_POINT|RADIXCHAR|THOUSANDS_SEP|THOUSEP|GROUPING|EXPR$|^YESSTR|^NOSTR|CODESET/', $name)) {
      out("langinfo $loc $name $v", nl_langinfo($v));
    }
  }
}
