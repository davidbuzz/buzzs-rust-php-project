# Usage: python3 crates/php-hash/tools/gen_tables.py crates/php-hash/src/algo
import re, sys
SRC = "/home/buzz/phprust/modules/php-src/ext/hash/"

def strip_comments(s):
    s = re.sub(r"/\*.*?\*/", "", s, flags=re.S)
    s = re.sub(r"//[^\n]*", "", s)
    return s

def extract(fname, name):
    txt = strip_comments(open(SRC + fname).read())
    m = re.search(r"\b" + re.escape(name) + r"\s*(\[[^=]*\])?\s*=\s*\{", txt)
    assert m, name
    i = m.end()
    depth = 1
    j = i
    while depth:
        if txt[j] == '{': depth += 1
        elif txt[j] == '}': depth -= 1
        j += 1
    body = txt[i:j-1]
    nums = re.findall(r"L64\(\s*(0x[0-9A-Fa-f]+)\s*\)|(0x[0-9A-Fa-f]+|\d+)[UuLl]*", body)
    out = []
    for a, b in nums:
        v = a or b
        out.append(int(v, 0))
    return out

def emit(name, ty, vals, per=4, width=None):
    w = width or (16 if ty == "u64" else 8)
    lines = [f"pub(crate) static {name}: [{ty}; {len(vals)}] = ["]
    for k in range(0, len(vals), per):
        chunk = ", ".join(f"0x{v:0{w}x}" for v in vals[k:k+per])
        lines.append(f"    {chunk},")
    lines.append("];")
    return "\n".join(lines)

hdr = "// @generated from php-src/ext/hash/{} by a table extraction script; do not edit.\n#![allow(clippy::unreadable_literal)]\n\n"
out_dir = sys.argv[1]

t = extract("php_hash_tiger_tables.h", "table"); assert len(t) == 1024
open(out_dir + "/tiger_tables.rs", "w").write(hdr.format("php_hash_tiger_tables.h") + "/// Tiger S-boxes t1..t4, concatenated.\n" + emit("TIGER_TABLE", "u64", t) + "\n")

s = extract("php_hash_snefru_tables.h", "tables"); assert len(s) == 16*256, len(s)
open(out_dir + "/snefru_tables.rs", "w").write(hdr.format("php_hash_snefru_tables.h") + "/// Snefru S-boxes, 16 tables of 256 words, concatenated.\n" + emit("SNEFRU_TABLES", "u32", s, 8) + "\n")

g1 = extract("php_hash_gost_tables.h", "tables_test"); assert len(g1) == 1024
g2 = extract("php_hash_gost_tables.h", "tables_crypto"); assert len(g2) == 1024
open(out_dir + "/gost_tables.rs", "w").write(hdr.format("php_hash_gost_tables.h") + "/// GOST R 34.11-94 test-parameter S-box tables (4 x 256), concatenated.\n" + emit("GOST_TABLES_TEST", "u32", g1, 8) + "\n\n/// GOST R 34.11-94 CryptoPro-parameter S-box tables (4 x 256), concatenated.\n" + emit("GOST_TABLES_CRYPTO", "u32", g2, 8) + "\n")

rc = extract("php_hash_whirlpool_tables.h", "rc"); assert len(rc) == 11
cs = []
for k in range(8):
    c = extract("php_hash_whirlpool_tables.h", f"C{k}"); assert len(c) == 256
    cs += c
open(out_dir + "/whirlpool_tables.rs", "w").write(hdr.format("php_hash_whirlpool_tables.h") + "/// Whirlpool round constants.\n" + emit("WHIRLPOOL_RC", "u64", rc) + "\n\n/// Whirlpool circulant tables C0..C7, concatenated.\n" + emit("WHIRLPOOL_C", "u64", cs) + "\n")

for nm in ["crc32_table", "crc32b_table", "crc32c_table"]:
    v = extract("php_hash_crc32_tables.h", nm)
    print(nm, len(v))
