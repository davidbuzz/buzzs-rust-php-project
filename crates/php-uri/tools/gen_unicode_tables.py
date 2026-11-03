#!/usr/bin/env python3
"""Generate src/whatwg/unicode_tables.rs from lexbor's unicode/res.h.

lexbor embeds the Unicode data it needs for IDNA (UTS #46 mapping table)
and normalization (NFC) as C arrays in ext/lexbor/lexbor/unicode/res.h.
This script parses those arrays and emits compact Rust tables:

* IDNA_RANGES / IDNA_ENTRIES / IDNA_MAP: per-code-point UTS #46 status and
  mapping, as runs of code points sharing the same lexbor IDNA entry.
* CCC_RANGES: canonical combining classes (non-zero runs).
* DECOMP_INDEX / DECOMP_CPS: full canonical decompositions.
* COMPOSITIONS: (first, second) -> composite pairs that are not excluded.

Usage:
    python3 -I tools/gen_unicode_tables.py \
        ../../modules/php-src/ext/lexbor/lexbor/unicode/res.h \
        src/whatwg/unicode_tables.rs
"""

import re
import sys


def parse_array(text, name):
    m = re.search(r"\b" + re.escape(name) + r"\[\d+\]\s*=\s*\{(.*?)\n\};", text, re.S)
    if m is None:
        raise SystemExit("array not found: " + name)
    body = re.sub(r"/\*.*?\*/", "", m.group(1), flags=re.S)
    return body


def parse_numbers(body):
    return [int(x, 0) for x in re.findall(r"0x[0-9A-Fa-f]+|\d+", body)]


def parse_structs(body):
    return [s.strip() for s in re.findall(r"\{([^{}]*)\}", body)]


IDNA_TYPES = {
    "0": 0,
    "LXB_UNICODE_IDNA__UNDEF": 0,
    "LXB_UNICODE_IDNA_DEVIATION": 1,
    "LXB_UNICODE_IDNA_DISALLOWED": 2,
    "LXB_UNICODE_IDNA_IGNORED": 3,
    "LXB_UNICODE_IDNA_MAPPED": 4,
    "LXB_UNICODE_IDNA_VALID": 5,
}


def decomp_type(tok):
    tok = tok.strip()
    separately = "LXB_UNICODE_CANONICAL_SEPARATELY" in tok
    rest = [t.strip() for t in tok.split("|") if "SEPARATELY" not in t]
    base = rest[0] if rest else "0"
    canonical = base in ("0", "LXB_UNICODE_DECOMPOSITION_TYPE__UNDEF")
    return canonical, separately


def main():
    src, dst = sys.argv[1], sys.argv[2]
    text = open(src, encoding="utf-8").read()

    # Code point -> entry index maps.
    maps = []
    for m in re.finditer(r"lxb_unicode_table_map_(\d+)_(\d+)\[(\d+)\]", text):
        start, end = int(m.group(1)), int(m.group(2))
        name = "lxb_unicode_table_map_%d_%d" % (start, end)
        nums = parse_numbers(parse_array(text, name))
        maps.append((start, nums))

    entries = [parse_numbers(s) for s in parse_structs(parse_array(text, "lxb_unicode_entries"))]
    idna_entries = []
    for s in parse_structs(parse_array(text, "lxb_unicode_idna_entries")):
        parts = [p.strip() for p in s.split(",")]
        idna_entries.append((IDNA_TYPES[parts[0]], int(parts[1], 0), int(parts[2], 0)))
    idna_cps = parse_numbers(parse_array(text, "lxb_unicode_idna_cps"))
    norm_entries = []
    for s in parse_structs(parse_array(text, "lxb_unicode_normalization_entries")):
        parts = [p.strip() for p in s.split(",")]
        canonical, separately = decomp_type(parts[0])
        norm_entries.append(
            (canonical, separately, int(parts[2], 0), int(parts[3], 0), int(parts[4], 0), int(parts[5], 0))
        )
    decomp_cps = parse_numbers(parse_array(text, "lxb_unicode_decomposition_cps"))
    comp_entries = [parse_numbers(s) for s in parse_structs(parse_array(text, "lxb_unicode_composition_entries"))]
    comp_cps = []
    for s in parse_structs(parse_array(text, "lxb_unicode_composition_cps")):
        a, b = [p.strip() for p in s.split(",")]
        comp_cps.append((int(a, 0), b == "true"))

    def entry_of(cp):
        # Mirrors lxb_unicode_entry(): cp > 0x10FFFE maps to entry 0.
        if cp > 0x10FFFE:
            return 0
        for start, nums in maps:
            if start <= cp < start + len(nums):
                return nums[cp - start]
        return 0

    # Resolve the per-code-point entries once.
    cp_entry = [entry_of(cp) for cp in range(0x110000)]

    # ---- IDNA ----
    idna_runs = []
    prev = None
    for cp in range(0x110000):
        idx = entries[cp_entry[cp]][1]
        if idx != prev:
            idna_runs.append((cp, idx))
            prev = idx

    # ---- CCC ----
    ccc_runs = []
    cur = None
    for cp in range(0x110000):
        ccc = norm_entries[entries[cp_entry[cp]][0]][2]
        if ccc != 0:
            if cur is not None and cur[1] == cp - 1 and cur[2] == ccc:
                cur[1] = cp
            else:
                if cur is not None:
                    ccc_runs.append(tuple(cur))
                cur = [cp, cp, ccc]
    if cur is not None:
        ccc_runs.append(tuple(cur))

    # ---- canonical decomposition ----
    decomp_index = []
    decomp_out = []
    comps = []
    for cp in range(0x110000):
        ne_idx = entries[cp_entry[cp]][0]
        if ne_idx == 0:
            continue
        canonical, separately, ccc, length, dec, comp = norm_entries[ne_idx]
        if canonical and length > 0:
            if separately:
                n = decomp_cps[dec + length]
                seq = decomp_cps[dec + length + 1: dec + length + 1 + n]
            else:
                seq = decomp_cps[dec: dec + length]
            decomp_index.append((cp, len(decomp_out), len(seq)))
            decomp_out.extend(seq)
        if comp != 0:
            clen, cindex, cfirst = comp_entries[comp]
            for k in range(clen):
                composite, excluded = comp_cps[cindex + k]
                if composite != 0 and not excluded:
                    comps.append((cp, cfirst + k, composite))
    comps.sort()

    out = []
    w = out.append
    w("// @generated by tools/gen_unicode_tables.py from lexbor's unicode/res.h.")
    w("// Do not edit by hand.")
    w("")
    w("//! Unicode data for IDNA (UTS #46) and NFC normalization.")
    w("")
    w("/// IDNA status: deviation.")
    w("pub const IDNA_DEVIATION: u8 = 1;")
    w("/// IDNA status: disallowed.")
    w("pub const IDNA_DISALLOWED: u8 = 2;")
    w("/// IDNA status: ignored.")
    w("pub const IDNA_IGNORED: u8 = 3;")
    w("/// IDNA status: mapped.")
    w("pub const IDNA_MAPPED: u8 = 4;")
    w("/// IDNA status: valid.")
    w("pub const IDNA_VALID: u8 = 5;")
    w("")
    w("/// First code point and index into [`IDNA_ENTRIES`]; a run lasts until the next start.")
    w("pub static IDNA_RANGES: [(u32, u16); %d] = [" % len(idna_runs))
    for cp, idx in idna_runs:
        w("    (0x%X, %d)," % (cp, idx))
    w("];")
    w("")
    w("/// Status, mapping length and mapping offset into [`IDNA_MAP`].")
    w("pub static IDNA_ENTRIES: [(u8, u8, u16); %d] = [" % len(idna_entries))
    for t, l, i in idna_entries:
        w("    (%d, %d, %d)," % (t, l, i))
    w("];")
    w("")
    w("/// Mapping targets referenced by `IDNA_ENTRIES`.")
    w("pub static IDNA_MAP: [u32; %d] = [" % len(idna_cps))
    for i in range(0, len(idna_cps), 8):
        w("    " + " ".join("0x%X," % c for c in idna_cps[i:i + 8]))
    w("];")
    w("")
    w("/// `(first, last, combining class)` for code points with a non-zero class.")
    w("pub static CCC_RANGES: [(u32, u32, u8); %d] = [" % len(ccc_runs))
    for a, b, c in ccc_runs:
        w("    (0x%X, 0x%X, %d)," % (a, b, c))
    w("];")
    w("")
    w("/// Code point, offset into [`DECOMP_CPS`] and length of full canonical decompositions.")
    w("pub static DECOMP_INDEX: [(u32, u16, u8); %d] = [" % len(decomp_index))
    for cp, off, l in decomp_index:
        w("    (0x%X, %d, %d)," % (cp, off, l))
    w("];")
    w("")
    w("/// Decomposition targets referenced by `DECOMP_INDEX`.")
    w("pub static DECOMP_CPS: [u32; %d] = [" % len(decomp_out))
    for i in range(0, len(decomp_out), 8):
        w("    " + " ".join("0x%X," % c for c in decomp_out[i:i + 8]))
    w("];")
    w("")
    w("/// `(first, second, composite)` primary compositions, sorted.")
    w("pub static COMPOSITIONS: [(u32, u32, u32); %d] = [" % len(comps))
    for a, b, c in comps:
        w("    (0x%X, 0x%X, 0x%X)," % (a, b, c))
    w("];")
    with open(dst, "w", encoding="utf-8") as f:
        f.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
