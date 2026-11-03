#!/usr/bin/env python3
"""Generate crates/php-stdstr/src/html_tables.rs from php-src's
ext/standard/html_tables.h.

Usage: python3 -I gen_html_tables.py <php-src>/ext/standard/html_tables.h <out.rs>
"""
import re
import sys

src = open(sys.argv[1], encoding="latin-1").read()
out = []
w = out.append

w("//! Data tables for `html.rs`, generated from php-src")
w("//! `ext/standard/html_tables.h` by `tools/gen_html_tables.py`.")
w("//! DO NOT EDIT BY HAND.")
w("")
w("#![allow(clippy::unreadable_literal)]")
w("")
w("/// A code point -> entity mapping row.")
w("#[derive(Debug, Clone, Copy)]")
w("pub(crate) enum EntityRow {")
w("    /// Unambiguous entity name (without `&` and `;`).")
w("    Single(&'static str),")
w("    /// Entity that depends on the next code point.")
w("    Multi {")
w("        /// Entity when no listed second code point follows.")
w("        default: Option<&'static str>,")
w("        /// `(second code point, entity)` pairs.")
w("        seconds: &'static [(u32, &'static str)],")
w("    },")
w("}")
w("")

# ---- enc_to_uni stage 2 tables
s2 = {}
for m in re.finditer(r"static const enc_to_uni_stage2 (enc_to_uni_s2_\w+) = \{ \{(.*?)\} \};", src, re.S):
    vals = [int(x, 16) for x in re.findall(r"0x([0-9A-Fa-f]+)", m.group(2))]
    assert len(vals) == 64, m.group(1)
    s2[m.group(1)] = vals

encs = {}
for m in re.finditer(r"static const enc_to_uni (enc_to_uni_\w+) = \{ \{(.*?)\}\s*\};", src, re.S):
    names = re.findall(r"&(enc_to_uni_s2_\w+)", m.group(2))
    assert len(names) == 4
    table = []
    for n in names:
        table += s2[n]
    encs[m.group(1)] = table


def emit_u16_array(name, vals):
    w("#[rustfmt::skip]")
    w(f"pub(crate) static {name}: [u16; {len(vals)}] = [")
    for i in range(0, len(vals), 8):
        w("    " + " ".join(f"0x{v:04X}," for v in vals[i:i + 8]))
    w("];")
    w("")


for cname, rname in [
    ("enc_to_uni_iso88591", "TO_UNI_ISO88591"),
    ("enc_to_uni_win1252", "TO_UNI_WIN1252"),
    ("enc_to_uni_iso885915", "TO_UNI_ISO885915"),
    ("enc_to_uni_win1251", "TO_UNI_WIN1251"),
    ("enc_to_uni_iso88595", "TO_UNI_ISO88595"),
    ("enc_to_uni_cp866", "TO_UNI_CP866"),
    ("enc_to_uni_macroman", "TO_UNI_MACROMAN"),
    ("enc_to_uni_koi8r", "TO_UNI_KOI8R"),
]:
    emit_u16_array(rname, encs[cname])

# ---- uni_to_enc tables
for m in re.finditer(r"static const uni_to_enc (unimap_\w+)\[\] = \{(.*?)\n\};", src, re.S):
    pairs = re.findall(r"\{ 0x([0-9A-Fa-f]+), 0x([0-9A-Fa-f]+) \}", m.group(2))
    rname = m.group(1).upper()
    keys = [int(a, 16) for a, _ in pairs]
    assert keys == sorted(keys), rname
    w("#[rustfmt::skip]")
    w(f"pub(crate) static {rname}: [(u16, u8); {len(pairs)}] = [")
    for i in range(0, len(pairs), 6):
        w("    " + " ".join(f"(0x{int(a,16):04X}, 0x{int(b,16):02X})," for a, b in pairs[i:i + 6]))
    w("];")
    w("")

# ---- multi code point tables
multi = {}
for m in re.finditer(r"static const entity_multicodepoint_row (multi_cp_\w+)\[\] = \{(.*?)\n\};", src, re.S):
    rows = re.findall(r'\{ \{(NULL|"[^"]*"),\s*(0x[0-9A-Fa-f]+|\d+),\s*(\d+)\} \}', m.group(2))
    lead = rows[0]
    default = None if lead[0] == "NULL" else lead[0].strip('"')
    size = int(lead[1], 16) if lead[1].startswith("0x") else int(lead[1], 8 if lead[1].startswith("0") else 10)
    seconds = [(int(r[1], 16), r[0].strip('"')) for r in rows[1:]]
    assert size == len(seconds), m.group(1)
    multi[m.group(1)] = (default, seconds)


def stage3_rows(prefix):
    rows = {}
    for m in re.finditer(r"static const entity_stage3_row stage3_table_" + prefix + r"_([0-9A-F]{5})\[\] = \{(.*?)\n\};", src, re.S):
        base = int(m.group(1), 16)
        cells = re.findall(r'\{([01]), \{ \{(NULL|"[^"]*"|\(void \*\)(\w+)), (\d+)\} \} \}', m.group(2))
        assert len(cells) == 64, (prefix, m.group(1), len(cells))
        for k, c in enumerate(cells):
            if c[0] == "1":
                rows[base + k] = ("multi", multi[c[2]])
            elif c[1] != "NULL":
                rows[base + k] = ("single", c[1].strip('"'))
    return rows


def check_stage_refs(prefix):
    # Ensure every referenced stage3 table sits at the position its name says.
    m = re.search(r"static const entity_stage1_row entity_ms_table_" + prefix + r"\[\] = \{(.*?)\n\};", src, re.S)
    s1 = re.findall(r"(\w+),", m.group(1))
    for i, name in enumerate(s1):
        if name == "empty_stage2_table":
            continue
        m2 = re.search(r"static const entity_stage2_row " + name + r"\[\] = \{(.*?)\n\};", src, re.S)
        s2n = re.findall(r"(\w+),", m2.group(1))
        assert len(s2n) == 64, name
        for j, n3 in enumerate(s2n):
            if n3 == "empty_stage3_table":
                continue
            base = int(n3.rsplit("_", 1)[1], 16)
            assert base == (i << 12) | (j << 6), (n3, i, j)


def emit_entity_table(rname, rows):
    w("#[rustfmt::skip]")
    w(f"pub(crate) static {rname}: &[(u32, EntityRow)] = &[")
    for cp in sorted(rows):
        kind, data = rows[cp]
        if kind == "single":
            w(f"    (0x{cp:05X}, EntityRow::Single(\"{data}\")),")
        else:
            default, seconds = data
            d = "None" if default is None else f"Some(\"{default}\")"
            sec = ", ".join(f"(0x{a:05X}, \"{b}\")" for a, b in seconds)
            w(f"    (0x{cp:05X}, EntityRow::Multi {{ default: {d}, seconds: &[{sec}] }}),")
    w("];")
    w("")


def inverse(prefix):
    ents = []
    for m in re.finditer(r"static const entity_cp_map ht_bucket_" + prefix + r"_[0-9A-F]+\[\] = \{(.*?)\};\n", src):
        for e in re.findall(r'\{"([^"]*)", (\d+), 0x([0-9A-Fa-f]+), (0x[0-9A-Fa-f]+|0)\}', m.group(1)):
            assert len(e[0]) == int(e[1])
            ents.append((e[0], int(e[2], 16), int(e[3], 16)))
    ents.sort(key=lambda e: e[0].encode())
    names = [e[0] for e in ents]
    assert len(names) == len(set(names)), prefix
    return ents


def emit_inverse(rname, ents):
    w("#[rustfmt::skip]")
    w(f"pub(crate) static {rname}: &[(&str, u32, u32)] = &[")
    for n, a, b in ents:
        w(f"    (\"{n}\", 0x{a:05X}, 0x{b:05X}),")
    w("];")
    w("")


for prefix, rname, iname in [("html5", "TABLE_HTML5", "INV_HTML5"), ("html4", "TABLE_HTML4", "INV_HTML4")]:
    rows = stage3_rows(prefix)
    check_stage_refs(prefix)
    emit_entity_table(rname, rows)
    emit_inverse(iname, inverse(prefix))

open(sys.argv[2], "w", encoding="utf-8").write("\n".join(out).rstrip("\n") + "\n")
