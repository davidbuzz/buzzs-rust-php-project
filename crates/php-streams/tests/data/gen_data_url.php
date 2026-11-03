<?php
// Generates tests/data/data_url_oracle.txt: hex(url) TAB result, where the
// result is "E:"hex(error) or "M:"meta-json TAB hex(contents).
$urls = ['data://,', 'data://', 'data://;base64,', 'data://;base64', 'data://foo,', 'data://foo=bar,',
  'data://text/plain,', 'data://text/plain;foo,', 'data://text/plain;foo=bar,', 'data://text/plain;foo=bar;bla,',
  'data://text/plain;foo=bar;base64,', 'data://text/plain;foo=bar;bar=baz', 'data://text/plain;foo=bar;bar=baz,',
  'data:,012345', 'data:,A%20brief%20note', 'data:application/vnd-xxx-query,select_vcount,fcol_from_fieldtable/local',
  'data:;base64,Zm9vYmFyIGZvb2Jhcg==', 'data:,;test', 'data:text/plain,test', 'data:text/plain;charset=US-ASCII,test',
  'data:;charset=UTF-8,Hello', 'data:text/plain;charset=UTF-8,Hello', 'data:,a,b',
'data:;base64,#Zm9vYmFyIGZvb2Jhcg==', 'data:;base64,#Zm9vYmFyIGZvb2Jhc=',
  'data:;base64,Zm9v YmFy', "data:;base64,Zm9v\nYmFy", 'data:;base64,Zm9vYg', 'data:;base64,Zm9vYmE=', 'data:;base64,Zm9vY===',
  'data:;base64,Zm9vY=x', 'data:;base64,Z', 'data:,a+b%2', 'data:,%zz%41', 'data:text/plain;mediatype=x;a=1,z',
  'data:text/plain;a=1;a=2,z', 'data:a/b;base64;x=y,abc', 'data:;x,a', 'data:a/b;=c,d', 'data:;=,e', 'data:a/b;base64x,',
  'data:text;plain,x', 'data:/,x', 'data:a/b;c=d;base64,YQ=='];
foreach ($urls as $u) {
  $err = null;
  set_error_handler(function ($no, $str) use (&$err) { if ($err === null) $err = $str; return true; });
  $f = fopen($u, 'r');
  restore_error_handler();
  if (!$f) {
    $msg = preg_replace('/^fopen\([^)]*\): Failed to open stream: /', '', $err ?? '');
    echo bin2hex($u), "\tE:", bin2hex($msg), "\n";
    continue;
  }
  $meta = stream_get_meta_data($f);
  $m = [];
  foreach ($meta as $k => $v) {
    if (in_array($k, ['wrapper_type', 'stream_type', 'mode', 'unread_bytes', 'seekable', 'uri'])) continue;
    $m[] = $k . '=' . (is_bool($v) ? ($v ? 'true' : 'false') : bin2hex($v));
  }
  echo bin2hex($u), "\tM:", implode(',', $m), "\t", bin2hex(stream_get_contents($f)), "\n";
}
