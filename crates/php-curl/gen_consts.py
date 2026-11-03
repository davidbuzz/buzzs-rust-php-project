#!/usr/bin/env python3
"""Regenerates src/consts.rs from ext/curl/curl.stub.php and the system
libcurl headers (needs gcc). Run: python3 crates/php-curl/gen_consts.py"""
import os, re, subprocess, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
STUB = os.path.join(HERE, '../../modules/php-src/ext/curl/curl.stub.php')
OUT = os.path.join(HERE, 'src/consts.rs')
extra = """CURLOPT_ERRORBUFFER CURLOPT_WRITEDATA CURLOPT_HEADERDATA CURLOPT_READDATA CURLOPT_SEEKDATA
CURLOPT_PROGRESSDATA CURLOPT_XFERINFODATA CURLOPT_FNMATCH_DATA CURLOPT_DEBUGDATA CURLOPT_PREREQDATA
CURLOPT_SSH_HOSTKEYDATA CURLOPT_MIMEPOST CURLOPT_COPYPOSTFIELDS CURLOPT_POSTFIELDSIZE_LARGE
CURLINFO_TYPEMASK CURLINFO_STRING CURLINFO_LONG CURLINFO_DOUBLE CURLINFO_SLIST CURLINFO_OFF_T CURLINFO_PTR CURLINFO_SOCKET
CURL_ERROR_SIZE CURLMOPT_PUSHDATA CURLINFO_APPCONNECT_TIME_T CURLINFO_CONNECT_TIME_T CURLINFO_NAMELOOKUP_TIME_T
CURLINFO_PRETRANSFER_TIME_T CURLINFO_REDIRECT_TIME_T CURLINFO_STARTTRANSFER_TIME_T CURLINFO_TOTAL_TIME_T
CURLINFO_EFFECTIVE_METHOD CURLINFO_CAPATH CURLINFO_CAINFO CURLINFO_PROXYAUTH_AVAIL CURLE_UNKNOWN_OPTION
CURL_FNMATCHFUNC_FAIL CURLKHMATCH_MISMATCH CURLKHMATCH_OK CURL_PUSH_DENY CURL_PUSH_OK CURL_WRITEFUNC_ERROR CURLOPT_SHARE
CURLINFO_CERTINFO CURLINFO_PRIVATE CURLINFO_HEADER_OUT CURLOPT_PROTOCOLS CURLPROTO_ALL CURLPROTO_FILE CURLOPT_STDERR
CURLE_OK CURLM_OK CURLSHE_OK CURLOPT_HTTPPOST CURLOPT_WRITEFUNCTION CURLOPT_HEADERFUNCTION CURLOPT_READFUNCTION
CURLOPT_SEEKFUNCTION CURLOPT_PROGRESSFUNCTION CURLOPT_XFERINFOFUNCTION CURLOPT_FNMATCH_FUNCTION CURLOPT_DEBUGFUNCTION
CURLOPT_PREREQFUNCTION CURLOPT_SSH_HOSTKEYFUNCTION CURLMOPT_PUSHFUNCTION""".split()
src=open(STUB).read().split('\n')
out=['#include <stdio.h>','#include <curl/curl.h>','#define CURLOPT_RETURNTRANSFER 19913','#define CURLOPT_BINARYTRANSFER 19914','#define CURLOPT_SAFE_UPLOAD -1','int main(void){']
cval=None;dep=None
for line in src:
    s=line.strip()
    if s.startswith('#if') or s.startswith('#endif') or s.startswith('#else') or s.startswith('#elif'):
        out.append(s); continue
    m=re.match(r'\* @cvalue (\w+)',s)
    if m: cval=m.group(1); continue
    m=re.match(r"#\[\\Deprecated\(since: '([^']*)', message: '([^']*)'\)\]",s)
    if m: dep=(m.group(1),m.group(2)); continue
    m=re.match(r'const (\w+) = UNKNOWN;',s)
    if m:
        d = 'D|%s|%s'%dep if dep else 'N'
        out.append('printf("P %%s %%lld %%s\\n", "%s", (long long)(%s), "%s");'%(m.group(1),cval,d))
        cval=None;dep=None
for e in extra:
    out.append('printf("I %%s %%lld N\\n", "%s", (long long)(%s));'%(e,e))
out.append('return 0;}')

tmp = tempfile.mkdtemp()
with open(os.path.join(tmp, 'c.c'), 'w') as f:
    f.write('\n'.join(out))
subprocess.check_call(['gcc', '-w', '-o', os.path.join(tmp, 'c'), os.path.join(tmp, 'c.c'), '-lcurl'])
lines = subprocess.check_output([os.path.join(tmp, 'c')]).decode().splitlines()
seen=set(); consts=[]; table=[]
for line in lines:
    kind,name,val,dep=line.split(' ',3)
    val=int(val)
    if name not in seen:
        seen.add(name); consts.append((name,val))
    if kind=='P':
        table.append((name,dep))
o=['//! libcurl constant values (generated from `ext/curl/curl.stub.php` against the',
   '//! system libcurl 8.5.0 headers by `gen_consts.py`; `#if LIBCURL_VERSION_NUM`',
   '//! gates of the stub are evaluated by the C preprocessor). Do not edit by hand.','',
   '#![allow(missing_docs, non_upper_case_globals, clippy::unreadable_literal)]','']
for n,v in consts:
    o.append('pub const %s: i64 = %d;'%(n,v))
o.append('')
o.append('/// A PHP-visible constant: name, value and `#[\\Deprecated]` (since, message).')
o.append('pub type PhpConstant = (&\'static str, i64, Option<(&\'static str, &\'static str)>);')
o.append('')
o.append('/// Every `CURL*` constant ext/curl registers, in stub order.')
o.append('pub static PHP_CONSTANTS: &[PhpConstant] = &[')
for n,d in table:
    if d.startswith('D|'):
        _,s,m=d.split('|',2)
        o.append('    ("%s", %s, Some(("%s", "%s"))),'%(n,n,s,m))
    else:
        o.append('    ("%s", %s, None),'%(n,n))
o.append('];')
open(OUT,'w').write('\n'.join(o)+'\n')
