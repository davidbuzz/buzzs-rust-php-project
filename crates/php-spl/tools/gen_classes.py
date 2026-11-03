#!/usr/bin/env python3
"""Generates src/classes_gen.rs from ext/spl/*.stub.php (php-src master).

Constant values come from the C sources' @cvalue names, resolved by a small
table (the values are ABI-stable and identical in /usr/bin/php).

Usage: python3 -I tools/gen_classes.py ../../modules/php-src/ext/spl > src/classes_gen.rs
"""
import os
import re
import sys

STUBS = ["spl_exceptions", "spl_iterators", "spl_array", "spl_directory",
         "spl_dllist", "spl_heap", "spl_fixedarray", "spl_observer", "php_spl"]

CVALUES = {
    "MultipleIterator::MIT_NEED_ANY": 0, "MultipleIterator::MIT_NEED_ALL": 1,
    "MultipleIterator::MIT_KEYS_NUMERIC": 0, "MultipleIterator::MIT_KEYS_ASSOC": 2,
    "SplDoublyLinkedList::IT_MODE_LIFO": 2, "SplDoublyLinkedList::IT_MODE_FIFO": 0,
    "SplDoublyLinkedList::IT_MODE_DELETE": 1, "SplDoublyLinkedList::IT_MODE_KEEP": 0,
    "SplPriorityQueue::EXTR_BOTH": 3, "SplPriorityQueue::EXTR_PRIORITY": 2,
    "SplPriorityQueue::EXTR_DATA": 1,
    "FilesystemIterator::CURRENT_MODE_MASK": 0xF0, "FilesystemIterator::CURRENT_AS_PATHNAME": 0x20,
    "FilesystemIterator::CURRENT_AS_FILEINFO": 0, "FilesystemIterator::CURRENT_AS_SELF": 0x10,
    "FilesystemIterator::KEY_MODE_MASK": 0xF00, "FilesystemIterator::KEY_AS_PATHNAME": 0,
    "FilesystemIterator::FOLLOW_SYMLINKS": 0x4000, "FilesystemIterator::KEY_AS_FILENAME": 0x100,
    "FilesystemIterator::NEW_CURRENT_AND_KEY": 0x100, "FilesystemIterator::OTHER_MODE_MASK": 0x7000,
    "FilesystemIterator::SKIP_DOTS": 0x1000, "FilesystemIterator::UNIX_PATHS": 0x2000,
    "SplFileObject::DROP_NEW_LINE": 1, "SplFileObject::READ_AHEAD": 2,
    "SplFileObject::SKIP_EMPTY": 4, "SplFileObject::READ_CSV": 8,
    "RecursiveIteratorIterator::LEAVES_ONLY": 0, "RecursiveIteratorIterator::SELF_FIRST": 1,
    "RecursiveIteratorIterator::CHILD_FIRST": 2, "RecursiveIteratorIterator::CATCH_GET_CHILD": 16,
    "CachingIterator::CALL_TOSTRING": 1, "CachingIterator::CATCH_GET_CHILD": 16,
    "CachingIterator::TOSTRING_USE_KEY": 2, "CachingIterator::TOSTRING_USE_CURRENT": 4,
    "CachingIterator::TOSTRING_USE_INNER": 8, "CachingIterator::FULL_CACHE": 256,
    "RegexIterator::USE_KEY": 1, "RegexIterator::INVERT_MATCH": 2, "RegexIterator::MATCH": 0,
    "RegexIterator::GET_MATCH": 1, "RegexIterator::ALL_MATCHES": 2, "RegexIterator::SPLIT": 3,
    "RegexIterator::REPLACE": 4,
    "RecursiveTreeIterator::BYPASS_CURRENT": 4, "RecursiveTreeIterator::BYPASS_KEY": 8,
    "RecursiveTreeIterator::PREFIX_LEFT": 0, "RecursiveTreeIterator::PREFIX_MID_HAS_NEXT": 1,
    "RecursiveTreeIterator::PREFIX_MID_LAST": 2, "RecursiveTreeIterator::PREFIX_END_HAS_NEXT": 3,
    "RecursiveTreeIterator::PREFIX_END_LAST": 4, "RecursiveTreeIterator::PREFIX_RIGHT": 5,
    "ArrayObject::STD_PROP_LIST": 1, "ArrayObject::ARRAY_AS_PROPS": 2,
    "ArrayIterator::STD_PROP_LIST": 1, "ArrayIterator::ARRAY_AS_PROPS": 2,
    "RecursiveArrayIterator::CHILD_ARRAYS_ONLY": 4,
}


def rs(s):
    if s is None:
        return "None"
    return 'Some("%s")' % s.replace("\\", "\\\\").replace('"', '\\"')


def split_params(s):
    out, depth, cur, quote, esc = [], 0, "", None, False
    for ch in s:
        if quote:
            cur += ch
            if esc:
                esc = False
            elif ch == "\\":
                esc = True
            elif ch == quote:
                quote = None
            continue
        if ch in "\"'":
            quote = ch
            cur += ch
            continue
        if ch in "([":
            depth += 1
        elif ch in ")]":
            depth -= 1
        if ch == "," and depth == 0:
            out.append(cur)
            cur = ""
        else:
            cur += ch
    if cur.strip():
        out.append(cur)
    return [p.strip() for p in out]


def parse_param(p):
    default = None
    if "=" in p:
        p, default = p.split("=", 1)
        p, default = p.strip(), default.strip()
    m = re.match(r"^(.*?)\s*(&)?\s*(\.\.\.)?\$(\w+)$", p)
    if m is None:
        raise SystemExit("bad param: %r" % p)
    ty = m.group(1).strip() or None
    return {"name": m.group(4), "type": ty, "default": default,
            "by_ref": bool(m.group(2)), "variadic": bool(m.group(3))}


def parse(path):
    src = open(path).read()
    classes = []
    funcs = []
    cur = None
    doc = ""
    attrs = []
    lines = src.split("\n")
    i = 0
    while i < len(lines):
        line = lines[i].strip()
        i += 1
        if line.startswith("/**"):
            doc = line
            while "*/" not in doc and i < len(lines):
                doc += "\n" + lines[i].strip()
                i += 1
            continue
        if line.startswith("#["):
            attrs.append(line)
            continue
        m = re.match(r"^(final\s+|abstract\s+)*(class|interface)\s+(\w+)(?:\s+extends\s+([\w\\, ]+?))?(?:\s+implements\s+([\w\\, ]+))?\s*$", line)
        if m:
            kind = m.group(2)
            mods = (m.group(1) or "")
            extends = [x.strip() for x in (m.group(4) or "").split(",") if x.strip()]
            impls = [x.strip() for x in (m.group(5) or "").split(",") if x.strip()]
            cur = {"name": m.group(3), "kind": kind, "abstract": "abstract" in line.split("class")[0],
                   "final": "final" in mods, "parent": None, "interfaces": [], "consts": [], "methods": [], "props": []}
            if kind == "interface":
                cur["interfaces"] = extends
            else:
                cur["parent"] = extends[0] if extends else None
                cur["interfaces"] = impls
            classes.append(cur)
            doc, attrs = "", []
            continue
        if line == "}" and cur is not None and not lines[i - 2].strip().endswith("{}"):
            pass
        pm = re.match(r"^(public|protected|private) (\??[\w|]+ )?\$(\w+)(?: = (.*))?;", line)
        if pm and cur is not None:
            cur["props"].append((pm.group(3), (pm.group(2) or "").strip() or None, pm.group(4)))
            doc, attrs = "", []
            continue
        m = re.match(r"^public const (?:\w+ )?(\w+) = (.*);", line)
        if m and cur is not None:
            key = cur["name"] + "::" + m.group(1)
            val = CVALUES[key]
            cur["consts"].append((m.group(1), val))
            doc, attrs = "", []
            continue
        if "function " in line:
            sig = line
            while not re.search(r"\)\s*(:\s*[^{;]+)?\s*(\{\}|;)\s*$", sig):
                sig += " " + lines[i].strip()
                i += 1
            m = re.match(r"^((?:\w+\s+)*)function\s+(&?)(\w+)\((.*)\)\s*(?::\s*([^{;]+?))?\s*(\{\}|;)$", sig)
            mods = m.group(1).split()
            params = [parse_param(p) for p in split_params(m.group(4))]
            dep = None
            for a in attrs:
                if a.startswith("#[\\Deprecated"):
                    dm = re.search(r"since:\s*'([^']*)'", a) or re.search(r'since:\s*"([^"]*)"', a)
                    mm = re.search(r"message:\s*'([^']*)'", a) or re.search(r'message:\s*"([^"]*)"', a)
                    dep = (dm.group(1) if dm else "") + "|" + (mm.group(1) if mm else "")
            alias = None
            am = re.search(r"@implementation-alias\s+(\S+)", doc)
            if am:
                alias = am.group(1)
            entry = {"name": m.group(3), "mods": mods, "params": params,
                     "ret": (m.group(5) or "").strip() or None, "by_ref_ret": m.group(2) == "&",
                     "tentative": "@tentative-return-type" in doc, "deprecated": dep, "alias": alias}
            if cur is not None and lines[i - 1:i] and (line.startswith("public") or line.startswith("abstract") or line.startswith("final") or line.startswith("protected") or line.startswith("private")):
                cur["methods"].append(entry)
            else:
                funcs.append(entry)
            doc, attrs = "", []
            continue
        if line == "}":
            cur = None
    return classes, funcs


def emit_params(params):
    out = []
    for p in params:
        out.append("ParamDef { name: \"%s\", ty: %s, default: %s, by_ref: %s, variadic: %s }" % (
            p["name"], rs(p["type"]), rs(p["default"]), str(p["by_ref"]).lower(), str(p["variadic"]).lower()))
    return "&[" + ", ".join(out) + "]"


def emit_method(m):
    flags = []
    for f in ("static", "abstract", "final", "protected", "private"):
        if f in m["mods"]:
            flags.append("MethodFlags::" + f.upper())
    fl = " | ".join(flags) if flags else "0"
    dep = "None"
    if m["deprecated"]:
        since, msg = m["deprecated"].split("|", 1)
        dep = "Some(Deprecation { since: %s, message: %s })" % (rs(since), rs(msg or None))
    return ("MethodDef { name: \"%s\", flags: %s, params: %s, return_type: %s, tentative: %s, "
            "deprecated: %s, alias_of: %s }") % (
        m["name"], fl, emit_params(m["params"]), rs(m["ret"]), str(m["tentative"]).lower(), dep, rs(m["alias"]))


def main():
    base = sys.argv[1]
    allc, allf = [], []
    for s in STUBS:
        c, f = parse(os.path.join(base, s + ".stub.php"))
        allc += c
        allf += f
    print("// @generated by tools/gen_classes.py from ext/spl/*.stub.php (php-src master). Do not edit.")
    print("use super::*;")
    print()
    print("/// Every class and interface declared by SPL, in registration order.")
    print("pub static CLASSES: &[ClassDef] = &[")
    for c in allc:
        kind = "ClassKind::Interface" if c["kind"] == "interface" else ("ClassKind::Abstract" if c["abstract"] else "ClassKind::Class")
        consts = ", ".join('("%s", %d)' % (n, v) for n, v in c["consts"])
        print("    ClassDef {")
        print('        name: "%s",' % c["name"])
        print("        kind: %s," % kind)
        print("        is_final: %s," % str(c["final"]).lower())
        print("        parent: %s," % rs(c["parent"]))
        print("        interfaces: &[%s]," % ", ".join('"%s"' % x for x in c["interfaces"]))
        print("        constants: &[%s]," % consts)
        print("        properties: &[%s]," % ", ".join('PropDef { name: "%s", ty: %s, default: %s }' % (n, rs(t), rs(d)) for n, t, d in c["props"]))
        print("        methods: &[")
        for m in c["methods"]:
            print("            " + emit_method(m) + ",")
        print("        ],")
        print("    },")
    print("];")
    print()
    print("/// The SPL functions.")
    print("pub static FUNCTIONS: &[MethodDef] = &[")
    for f in allf:
        print("    " + emit_method(f) + ",")
    print("];")


main()
