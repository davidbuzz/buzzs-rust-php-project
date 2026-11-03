#!/usr/bin/env python3
"""Generates the `icu! { ... }` declaration blocks of src/ffi.rs from the
system ICU C headers (/usr/include/unicode).  Run manually; the output is
committed (pasted between the DECLS markers of src/ffi.rs)."""
import re, sys

INC = '/usr/include/unicode/'
HEADERS = ['utypes', 'uversion', 'uenum', 'uloc', 'unum', 'udat', 'ucal', 'ucol', 'unorm2', 'uidna',
           'ubrk', 'utrans', 'ures', 'uspoof', 'ucnv', 'ucnv_cb', 'udatpg', 'ulistformatter', 'uchar',
           'ustring', 'upluralrules', 'unumberformatter', 'unumberrangeformatter', 'ureldatefmt',
           'usearch', 'uset', 'uscript', 'ucurr', 'ufieldpositer', 'uformattedvalue', 'uldnames',
           'unumsys', 'umsg', 'ucasemap', 'uiter', 'ucoleitr', 'putil', 'uclean', 'udisplaycontext',
           'icudataver', 'ulocdata', 'uformattednumber', 'uformattable', 'ustring', 'utext']
OPAQUE = set('''UNumberFormat UDateFormat UCollator UEnumeration UCalendar UBreakIterator UTransliterator
UResourceBundle USpoofChecker USpoofCheckResult UConverter UDateTimePatternGenerator UListFormatter
UFormattedList UIDNA UNormalizer2 UPluralRules UNumberFormatter UFormattedNumber UNumberRangeFormatter
UFormattedNumberRange URelativeDateTimeFormatter UStringSearch USet UFieldPositionIterator
ULocaleDisplayNames UFormattedValue UConstrainedFieldPosition UCharsetDetector UNumberingSystem UText'''.split())
STRUCTS = {'UParseError', 'UFieldPosition', 'UIDNAInfo', 'UTransPosition', 'UConverterFromUnicodeArgs',
           'UConverterToUnicodeArgs'}
SKIP_TYPES = {'UCharIterator', 'UCaseMap', 'UReplaceable', 'UReplaceableCallbacks',
              'va_list', 'UDataInfo', 'UCollationElements', 'UBreakIterator*const',
              'UDateFormatOpener', 'UCPMap', 'UCPTrie', 'UMutableCPTrie', 'UTraceEntry',
              'UCharNameChoice_', 'UCollationResult_', 'ULocaleData', 'USerializedSet', 'UStringPrepProfile',
              'UConverterSelector', 'ULocale', 'ULocaleBuilder', 'UDisplayOptions', 'UConstrainedFieldPosition',
              'UGenderInfo', 'USimpleNumberFormatter', 'USimpleNumber'}
CALLBACKS = {'UConverterToUCallback': 'UConverterToUCallback', 'UConverterFromUCallback': 'UConverterFromUCallback',
             'UCharEnumTypeRange': 'UCharEnumTypeRange', 'UEnumCharNamesFn': 'UEnumCharNamesFn'}
SCALARS = {'int32_t': 'i32', 'int64_t': 'i64', 'uint32_t': 'u32', 'int16_t': 'i16', 'uint16_t': 'u16',
           'int8_t': 'i8', 'uint8_t': 'u8', 'double': 'f64', 'UBool': 'UBool', 'bool': 'bool', 'UChar': 'UChar',
           'UChar32': 'UChar32', 'UDate': 'UDate', 'char': 'c_char', 'size_t': 'usize', 'void': 'c_void',
           'UErrorCode': 'UErrorCode', 'UVersionInfo': '[u8; 4]', 'int': 'i32', 'float': 'f32',
           'UProperty': 'i32', 'UScriptCode': 'i32', 'UAcceptResult': 'i32',
           'UMessageFormat': '*mut c_void', 'UFormattable': '*mut c_void'}

FNTYPES = {'UEnumCharNamesFn', 'UCharEnumTypeRange'}

def rtype(ctype):
    t = ctype.replace('U_CALLCONV', '').strip()
    t = t.replace('*', ' * ')
    toks = [w for w in t.split() if w not in ('struct', 'enum')]
    # const-ness per pointer level: level 0 = base, level k = k-th '*'
    levels = [False]
    base = None
    for w in toks:
        if w == 'const':
            levels[-1] = True
        elif w == '*':
            levels.append(False)
        else:
            base = w
    stars = len(levels) - 1
    if base is None or base in SKIP_TYPES:
        return None
    if base == 'UVersionInfo':
        return '*mut u8'
    if base in FNTYPES:
        if stars == 1:
            return CALLBACKS[base]
        return None
    if base in CALLBACKS:
        r = CALLBACKS[base]
    elif base in SCALARS:
        r = SCALARS[base]
    elif base in OPAQUE or base in STRUCTS:
        r = base
    elif base.startswith('U'):
        r = 'i32'  # enum
    else:
        return None
    # pointee const-ness: the const before the k-th star applies to what it points to
    out = r
    for k in range(1, stars + 1):
        out = ('*const ' if levels[k - 1] else '*mut ') + out
    return out

def split_args(s):
    depth = 0; cur = ''; out = []
    for ch in s:
        if ch == '(':
            depth += 1
        if ch == ')':
            depth -= 1
        if ch == ',' and depth == 0:
            out.append(cur); cur = ''
        else:
            cur += ch
    if cur.strip():
        out.append(cur)
    return out

seen = set()
lines = []
for h in HEADERS:
    try:
        src = open(INC + h + '.h').read()
    except FileNotFoundError:
        continue
    src = re.sub(r'/\*.*?\*/', '', src, flags=re.S)
    src = re.sub(r'//[^\n]*', '', src)
    for m in re.finditer(r'U_CAPI\s+(.*?)\s*U_EXPORT2\s*(\w+)\s*\((.*?)\)\s*(?:U_\w+\s*)*;', src, re.S):
        ret, name, args = m.group(1), m.group(2), m.group(3)
        if name in seen or 'U_DEPRECATED' in ret or name.startswith('ucnv_cb') and False:
            continue
        ret = ret.replace('U_NOEXCEPT', '').strip()
        rr = None
        if ret.strip() != 'void':
            rr = rtype(ret)
            if rr is None:
                continue
        argl = split_args(args.strip())
        rargs = []
        ok = True
        if argl == ['void'] or argl == []:
            argl = []
        for i, a in enumerate(argl):
            a = ' '.join(a.split())
            if a == '...':
                ok = False; break
            a = re.sub(r'\[\s*\w*\s*\]$', '*', a)
            mm = re.match(r'^(.*?)(\w+)\s*(\[[^\]]*\])?$', a)
            if not mm:
                ok = False; break
            ty, nm = mm.group(1), mm.group(2)
            if mm.group(3):
                ty += '*'
            if ty.strip() == '' or ty.strip() in ('const',):
                ty, nm = a, 'arg%d' % i
            r = rtype(ty)
            if r is None:
                ok = False; break
            nm = re.sub(r'^(type|fn|ref|match|in|loop|mod|use|self|impl|where|move|box|str|unsafe|override|final|abstract|virtual|yield|async|await|dyn|try|macro|priv|typeof|unsized|become|do|crate|super|extern|static|struct|trait|enum|const|let|mut|pub|return|as|break|continue|else|false|true|for|if|while)$', r'\1_', nm)
            rargs.append('%s: %s' % (nm, r))
        if not ok:
            continue
        seen.add(name)
        lines.append('    fn %s(%s)%s;' % (name, ', '.join(rargs), (' -> ' + rr) if rr else ''))
print('icu! {')
print('\n'.join(lines))
print('}')
