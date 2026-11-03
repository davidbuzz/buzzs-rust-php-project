#!/usr/bin/env python3
"""Generates src/stubs.rs from php-src's ext/intl *.stub.php files.

Usage: python3 -I tools/gen_stubs.py <php-src>/ext/intl <php-intl constants.rs> > src/stubs.rs

Every class (methods with their exact stub signatures, flags, the
procedural alias each one implements), enum and procedural function is
emitted as static tables; `Class::CONST` default values are resolved to
literals from the php-intl constants table so the VM fills them in.
"""
import os
import re
import sys

root, consts_path = sys.argv[1], sys.argv[2]

# ----------------------------------------------------------- constants
consts = {}
cur = "__global__"
in_classes = False
for line in open(consts_path, encoding="utf-8"):
    if line.startswith("pub const CLASSES"):
        in_classes = True
    m = re.match(r'^        "(\w+)",$', line)
    if m and in_classes:
        cur = m.group(1)
        continue
    m = re.match(r'^\s+\("(\w+)", ConstValue::(Int|Str|Float)\((.*)\)\),$', line)
    if m and cur:
        consts[(cur, m.group(1))] = m.group(3)

files = []
for d in sorted(os.listdir(root)):
    p = os.path.join(root, d)
    if os.path.isdir(p):
        for f in sorted(os.listdir(p)):
            if f.endswith(".stub.php"):
                files.append(os.path.join(p, f))
files.append(os.path.join(root, "php_intl.stub.php"))


def resolve_defaults(sig):
    def rep(m):
        cls, name = m.group(1), m.group(2)
        v = consts.get((cls, name))
        if v is None:
            return m.group(0)
        return v
    # `= Foo::BAR` and `= Foo::BAR | Foo::BAZ` (only simple forms occur)
    def rep_default(m):
        expr = m.group(1)
        parts = [p.strip() for p in expr.split("|")]
        vals = []
        for p in parts:
            mm = re.fullmatch(r"(\w+)::(\w+)", p)
            if not mm:
                return m.group(0)
            v = consts.get((mm.group(1), mm.group(2)))
            if v is None:
                return m.group(0)
            vals.append(int(v))
        r = 0
        for v in vals:
            r |= v
        return "= " + str(r)
    sig = re.sub(r"= ((?:\w+::\w+)(?:\s*\|\s*\w+::\w+)*)", rep_default, sig)

    def rep_global(m):
        v = consts.get(("__global__", m.group(1)))
        return m.group(0) if v is None else "= " + v
    return re.sub(r"= ([A-Z][A-Z0-9_]+)(?=[,)])", rep_global, sig)


def norm(s):
    s = re.sub(r"\s+", " ", s).strip()
    s = s.replace("( ", "(").replace(", )", ")").replace(" )", ")")
    s = s.replace(",)", ")")
    return s


classes = []   # dicts
deprecated_consts = []  # (class or None, name, since, message)
functions = []  # dicts

for path in files:
    src = open(path, encoding="utf-8").read()
    # strip `#if`/`#endif` lines
    src = "\n".join(l for l in src.splitlines() if not re.match(r"#(if|endif|else|ifdef|ifndef)", l))
    i = 0
    doc = ""
    attr = ""
    cls = None
    depth = 0
    tokens = re.finditer(
        r"(/\*\*.*?\*/)|(/\*.*?\*/)|(#\[\\?Deprecated\((.*?)\)\])|"
        r"((?:final\s+|abstract\s+)*(class|interface|enum)\s+(\w+)([^{]*)\{)|"
        r"((?:(?:public|protected|private|static|final|abstract)\s+)*function\s+[^{;]*?\)\s*(?::\s*[^{;]+)?)\s*(\{\s*\}|;)|"
        r"(case\s+(\w+)\s*;)|"
        r"((?:public|protected|private)\s+(?:readonly\s+)?[\w?|\\]+\s+\$(\w+)\s*;)|"
        r"(const\s+[^;]*;)|"
        r"(\})",
        src, re.S)
    for m in tokens:
        if m.group(1):
            doc = m.group(1)
            continue
        if m.group(2):
            continue
        if m.group(3):
            attr = m.group(4)
            continue
        if m.group(5):
            head = m.group(5)
            kind = m.group(6)
            name = m.group(7)
            rest = m.group(8)
            ext = re.search(r"extends\s+([\w\\]+)", rest)
            impl = re.search(r"implements\s+([\w\\,\s]+)", rest)
            cls = {
                "name": name,
                "kind": kind,
                "final": "final" in head.split(kind)[0],
                "abstract": "abstract" in head.split(kind)[0],
                "parent": ext.group(1) if ext else None,
                "interfaces": [x.strip() for x in impl.group(1).split(",")] if impl else [],
                "not_serializable": "@not-serializable" in doc,
                "methods": [],
                "props": [],
                "cases": [],
            }
            classes.append(cls)
            doc = ""
            attr = ""
            continue
        if m.group(9):
            decl = norm(m.group(9))
            mods, _, sig = decl.partition("function ")
            sig = resolve_defaults(norm(sig))
            # ZPP parses these with Z_PARAM_OBJ_OR_STR_OR_NULL: its TypeError
            # names "object|string|null".
            sig = sig.replace("IntlTimeZone|DateTimeZone|string|null $timezone", "object|string|null $timezone")
            alias = re.search(r"@alias\s+(\w+)", doc)
            impl_alias = re.search(r"@implementation-alias\s+(\w+)", doc)
            dep = None
            if attr:
                since = re.search(r"since:\s*'([^']*)'", attr)
                msg = re.search(r"message:\s*'([^']*)'", attr)
                dep = (since.group(1) if since else "", msg.group(1) if msg else "")
            if cls is not None:
                cls["methods"].append({
                    "sig": sig,
                    "mods": mods.split(),
                    "alias": alias.group(1) if alias else None,
                    "impl_alias": impl_alias.group(1) if impl_alias else None,
                    "deprecated": dep,
                })
            else:
                functions.append({"sig": sig, "deprecated": dep})
            doc = ""
            attr = ""
            continue
        if m.group(11):
            if cls is not None:
                cls["cases"].append(m.group(12))
            continue
        if m.group(13):
            if cls is not None:
                decl = norm(m.group(13))
                parts = decl.rstrip(";").split()
                vis = parts[0]
                readonly = "readonly" in parts
                ty = parts[-2]
                cls["props"].append((m.group(14), ty, vis, readonly))
            doc = ""
            continue
        if m.group(15):
            if attr:
                cname = re.search(r"const\s+(?:\w+\s+)?(\w+)\s*=", m.group(15))
                since = re.search(r"since:\s*'([^']*)'", attr)
                msg = re.search(r"message:\s*'([^']*)'", attr)
                if cname:
                    deprecated_consts.append((cls["name"] if cls else None, cname.group(1),
                                              since.group(1) if since else "", msg.group(1) if msg else ""))
            doc = ""
            attr = ""
            continue
        if m.group(16):
            cls = None
            continue

# Handler keys.
alias_to_method = {}
for c in classes:
    for mt in c["methods"]:
        name = mt["sig"].split("(")[0]
        if mt["alias"]:
            alias_to_method[mt["alias"]] = f'{c["name"]}::{name}'


def fn_key(fname):
    return alias_to_method.get(fname, fname)


for c in classes:
    for mt in c["methods"]:
        name = mt["sig"].split("(")[0]
        if mt["impl_alias"]:
            mt["key"] = fn_key(mt["impl_alias"])
        else:
            mt["key"] = f'{c["name"]}::{name}'
for f in functions:
    f["key"] = fn_key(f["sig"].split("(")[0])

# Topological order (parents/interfaces of this extension first).
names = {c["name"] for c in classes}
ordered = []
seen = set()


def visit(c):
    if c["name"] in seen:
        return
    for dep in [c["parent"]] + c["interfaces"]:
        if dep in names:
            visit(next(x for x in classes if x["name"] == dep))
    seen.add(c["name"])
    ordered.append(c)


for c in classes:
    visit(c)


def rs(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def opt(s):
    return "None" if s is None else f"Some({rs(s)})"


out = []
out.append("//! The ext/intl stub declarations (php-src master `*.stub.php`).")
out.append("//! Generated by `tools/gen_stubs.py`; do not edit by hand.")
out.append("")
out.append("use crate::registry::{ClassKind, ClassStub, FuncStub, MethodStub, PropStub};")
out.append("")
out.append("/// Every class, interface and enum, parents first.")
out.append("pub(crate) const CLASSES: &[ClassStub] = &[")
for c in ordered:
    out.append("    ClassStub {")
    out.append(f'        name: {rs(c["name"])},')
    kind = {"class": "Class", "interface": "Interface", "enum": "Enum"}[c["kind"]]
    out.append(f"        kind: ClassKind::{kind},")
    out.append(f'        parent: {opt(c["parent"])},')
    out.append("        interfaces: &[" + ", ".join(rs(i) for i in c["interfaces"]) + "],")
    out.append(f'        is_final: {"true" if c["final"] else "false"},')
    out.append(f'        is_abstract: {"true" if c["abstract"] else "false"},')
    out.append(f'        not_serializable: {"true" if c["not_serializable"] else "false"},')
    out.append("        cases: &[" + ", ".join(rs(x) for x in c["cases"]) + "],")
    out.append("        props: &[")
    for (n, ty, vis, ro) in c["props"]:
        out.append(f"            PropStub {{ name: {rs(n)}, ty: {rs(ty)}, visibility: {rs(vis)}, readonly: {'true' if ro else 'false'} }},")
    out.append("        ],")
    out.append("        methods: &[")
    for mt in c["methods"]:
        mods = mt["mods"]
        dep = mt["deprecated"]
        dep_s = "None" if dep is None else f"Some(({rs(dep[0])}, {rs(dep[1])}))"
        out.append("            MethodStub {")
        out.append(f'                sig: {rs(mt["sig"])},')
        out.append(f'                key: {rs(mt["key"])},')
        out.append(f'                visibility: {rs(next((x for x in mods if x in ("public", "protected", "private")), "public"))},')
        out.append(f'                is_static: {"true" if "static" in mods else "false"},')
        out.append(f'                is_final: {"true" if "final" in mods else "false"},')
        out.append(f'                is_abstract: {"true" if "abstract" in mods else "false"},')
        out.append(f"                deprecated: {dep_s},")
        out.append("            },")
    out.append("        ],")
    out.append("    },")
out.append("];")
out.append("")
out.append("/// `#[\\Deprecated]` constants: `(class, name, since, message)`.")
out.append("pub(crate) const DEPRECATED_CONSTANTS: &[(Option<&str>, &str, &str, &str)] = &[")
for (c, n, si, me) in deprecated_consts:
    out.append(f"    ({opt(c)}, {rs(n)}, {rs(si)}, {rs(me)}),")
out.append("];")
out.append("")
out.append("/// Every procedural function.")
out.append("pub(crate) const FUNCTIONS: &[FuncStub] = &[")
for f in functions:
    dep = f["deprecated"]
    dep_s = "None" if dep is None else f"Some(({rs(dep[0])}, {rs(dep[1])}))"
    out.append(f'    FuncStub {{ sig: {rs(f["sig"])}, key: {rs(f["key"])}, deprecated: {dep_s} }},')
out.append("];")
print("\n".join(out))
