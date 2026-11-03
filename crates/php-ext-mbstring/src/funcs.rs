//! The string, encoding and configuration `mb_*` functions
//! (`ext/mbstring/mbstring.c` `PHP_FUNCTION`s), excluding the array walkers
//! (see [`crate::arrays`]) and the regex functions (see [`crate::regex`]).

use php_mbstring::info::InfoValue;
use php_mbstring::strings::StrstrVariant;
use php_mbstring::{EncodingList, HttpInput, SubstituteArg, SubstituteChar};
use php_runtime::{Value, ZArray, ZStr};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::state::{self, bytes, call, opt_str, str_list};

/// An encoding-list argument (`array|string`) with its elements already
/// converted to strings.
pub(crate) enum OwnedList {
    /// A comma-separated list.
    Str(Vec<u8>),
    /// An array of names; `all` is set for the `mb_list_encodings()` array.
    Array { items: Vec<Vec<u8>>, all: bool },
}

impl OwnedList {
    /// Converts an `array|string` argument (`zval_try_get_string` on array
    /// elements).
    pub(crate) fn from_value(vm: &mut Vm, v: &Value) -> VmResult<Self> {
        match v {
            Value::Array(arr) => {
                let all = state::is_all_encodings(arr);
                let mut items = Vec::with_capacity(arr.len());
                for (_, item) in arr.iter() {
                    items.push(vm.value_to_string(&item.deref())?.as_bytes().to_vec());
                }
                Ok(Self::Array { items, all })
            }
            other => Ok(Self::Str(vm.value_to_string(other)?.as_bytes().to_vec())),
        }
    }

    /// The borrowed form the library takes.
    pub(crate) fn as_list(&self) -> EncodingList<'_> {
        match self {
            Self::Str(s) => EncodingList::Str(s),
            Self::Array { items, .. } => EncodingList::Array(items),
        }
    }

    /// Whether this is the `mb_list_encodings()` array.
    pub(crate) fn is_all(&self) -> bool {
        matches!(self, Self::Array { all: true, .. })
    }
}

fn opt_bytes(s: Option<&ZStr>) -> Option<&[u8]> {
    s.map(ZStr::as_bytes)
}

fn int_or_false(v: Option<i64>) -> Value {
    v.map_or(Value::Bool(false), Value::Int)
}

fn str_or_false(v: Option<Vec<u8>>) -> Value {
    v.map_or(Value::Bool(false), bytes)
}

// ---- configuration ------------------------------------------------------

fn mb_language(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(name) = opt_str(a, 0) else {
        return Ok(Value::from(state::with(|s| s.mb_language_get())));
    };
    call(vm, a, |s| s.mb_language_set(name.as_bytes()))?;
    // mb_language() goes through zend_alter_ini_entry().
    crate::ini::store_raw(vm, "mbstring.language", name.as_bytes());
    Ok(Value::Bool(true))
}

fn mb_internal_encoding(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(name) = opt_str(a, 0) else {
        return Ok(Value::from(state::with(|s| s.mb_internal_encoding_get())));
    };
    call(vm, a, |s| s.mb_internal_encoding_set(name.as_bytes())).map(Value::Bool)
}

fn mb_http_input(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ty = opt_str(a, 0);
    let r = call(vm, a, |s| s.mb_http_input(opt_bytes(ty.as_ref())))?;
    Ok(match r {
        HttpInput::False => Value::Bool(false),
        HttpInput::Name(n) => Value::from(n),
        HttpInput::List(l) => str_list(l),
    })
}

fn mb_http_output(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(name) = opt_str(a, 0) else {
        return Ok(Value::from(state::with(|s| s.mb_http_output_get())));
    };
    if name.as_bytes().contains(&0) {
        return Err(a.value_error(vm, 1, "must not contain any null bytes"));
    }
    call(vm, a, |s| s.mb_http_output_set(name.as_bytes())).map(Value::Bool)
}

fn mb_detect_order(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(v) = a.opt(0) else {
        return Ok(str_list(state::with(|s| s.mb_detect_order_get())));
    };
    let list = OwnedList::from_value(vm, &v)?;
    call(vm, a, |s| s.mb_detect_order_set(list.as_list())).map(Value::Bool)
}

fn mb_substitute_character(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(v) = a.opt(0) else {
        return Ok(match state::with(|s| s.mb_substitute_character_get()) {
            SubstituteChar::None => Value::from("none"),
            SubstituteChar::Long => Value::from("long"),
            SubstituteChar::Entity => Value::from("entity"),
            SubstituteChar::Char(c) => Value::Int(i64::from(c)),
        });
    };
    match v {
        Value::Str(s) => call(vm, a, |st| {
            st.mb_substitute_character_set(SubstituteArg::Str(s.as_bytes()))
        }),
        other => {
            let n = vm.value_to_int(&other)?;
            call(vm, a, |st| st.mb_substitute_character_set(SubstituteArg::Int(n)))
        }
    }
    .map(Value::Bool)
}

fn mb_preferred_mime_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0);
    let r = call(vm, a, |s| s.mb_preferred_mime_name(name.as_bytes()))?;
    Ok(r.map_or(Value::Bool(false), Value::from))
}

fn mb_list_encodings(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(state::all_encodings()))
}

fn mb_encoding_aliases(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0);
    call(vm, a, |s| s.mb_encoding_aliases(name.as_bytes())).map(str_list)
}

fn info_value(v: InfoValue) -> Value {
    match v {
        InfoValue::Str(s) => Value::from(s),
        InfoValue::Int(i) => Value::Int(i),
        InfoValue::List(l) => str_list(l),
        InfoValue::Assoc(pairs) => Value::Array(ZArray::from_pairs(
            pairs.into_iter().map(|(k, v)| (k, info_value(v))),
        )),
        InfoValue::Null => Value::Null,
        InfoValue::False => Value::Bool(false),
    }
}

fn mb_get_info(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ty = a.str(0);
    let r = state::with(|s| s.mb_get_info(Some(ty.as_bytes())));
    state::flush_diagnostics(vm)?;
    Ok(info_value(r))
}

// ---- string functions ---------------------------------------------------

fn mb_str_split(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, len, enc) = (a.str(0), a.int(1), opt_str(a, 2));
    call(vm, a, |st| {
        st.mb_str_split(s.as_bytes(), len, opt_bytes(enc.as_ref()))
    })
    .map(str_list)
}

fn mb_strlen(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, enc) = (a.str(0), opt_str(a, 1));
    call(vm, a, |st| st.mb_strlen(s.as_bytes(), opt_bytes(enc.as_ref()))).map(Value::Int)
}

macro_rules! strpos_fn {
    ($($name:ident => $method:ident),* $(,)?) => {$(
        fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let (h, n, off, enc) = (a.str(0), a.str(1), a.int(2), opt_str(a, 3));
            call(vm, a, |st| {
                st.$method(h.as_bytes(), n.as_bytes(), off, opt_bytes(enc.as_ref()))
            })
            .map(int_or_false)
        }
    )*};
}

strpos_fn! {
    mb_strpos => mb_strpos,
    mb_strrpos => mb_strrpos,
    mb_stripos => mb_stripos,
    mb_strripos => mb_strripos,
}

fn strstr_common(vm: &mut Vm, a: &Args, variant: StrstrVariant) -> VmResult<Value> {
    let (h, n, before, enc) = (a.str(0), a.str(1), a.bool(2), opt_str(a, 3));
    call(vm, a, |st| {
        st.mb_strstr_variant(
            variant,
            h.as_bytes(),
            n.as_bytes(),
            before,
            opt_bytes(enc.as_ref()),
        )
    })
    .map(str_or_false)
}

fn mb_strstr(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    strstr_common(vm, a, StrstrVariant::Strstr)
}

fn mb_strrchr(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    strstr_common(vm, a, StrstrVariant::Strrchr)
}

fn mb_stristr(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    strstr_common(vm, a, StrstrVariant::Stristr)
}

fn mb_strrichr(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    strstr_common(vm, a, StrstrVariant::Strrichr)
}

fn mb_substr_count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (h, n, enc) = (a.str(0), a.str(1), opt_str(a, 2));
    call(vm, a, |st| {
        st.mb_substr_count(h.as_bytes(), n.as_bytes(), opt_bytes(enc.as_ref()))
    })
    .map(Value::Int)
}

fn opt_int(a: &Args, n: usize) -> Option<i64> {
    match a.opt(n) {
        Some(Value::Int(i)) => Some(i),
        _ => None,
    }
}

fn mb_substr(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, start, len, enc) = (a.str(0), a.int(1), opt_int(a, 2), opt_str(a, 3));
    call(vm, a, |st| {
        st.mb_substr(s.as_bytes(), start, len, opt_bytes(enc.as_ref()))
    })
    .map(bytes)
}

fn mb_strcut(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, start, len, enc) = (a.str(0), a.int(1), opt_int(a, 2), opt_str(a, 3));
    call(vm, a, |st| {
        st.mb_strcut(s.as_bytes(), start, len, opt_bytes(enc.as_ref()))
    })
    .map(bytes)
}

fn mb_strwidth(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, enc) = (a.str(0), opt_str(a, 1));
    call(vm, a, |st| st.mb_strwidth(s.as_bytes(), opt_bytes(enc.as_ref()))).map(Value::Int)
}

fn mb_strimwidth(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, start, width, marker, enc) = (a.str(0), a.int(1), a.int(2), a.str(3), opt_str(a, 4));
    call(vm, a, |st| {
        st.mb_strimwidth(
            s.as_bytes(),
            start,
            width,
            marker.as_bytes(),
            opt_bytes(enc.as_ref()),
        )
    })
    .map(bytes)
}

fn mb_convert_case(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, mode, enc) = (a.str(0), a.int(1), opt_str(a, 2));
    call(vm, a, |st| {
        st.mb_convert_case(s.as_bytes(), mode, opt_bytes(enc.as_ref()))
    })
    .map(bytes)
}

macro_rules! str_enc_fn {
    ($($name:ident),* $(,)?) => {$(
        fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let (s, enc) = (a.str(0), opt_str(a, 1));
            call(vm, a, |st| st.$name(s.as_bytes(), opt_bytes(enc.as_ref()))).map(bytes)
        }
    )*};
}

str_enc_fn!(mb_strtoupper, mb_strtolower, mb_ucfirst, mb_lcfirst, mb_scrub);

macro_rules! trim_fn {
    ($($name:ident),* $(,)?) => {$(
        fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let (s, chars, enc) = (a.str(0), opt_str(a, 1), opt_str(a, 2));
            call(vm, a, |st| {
                st.$name(s.as_bytes(), opt_bytes(chars.as_ref()), opt_bytes(enc.as_ref()))
            })
            .map(bytes)
        }
    )*};
}

trim_fn!(mb_trim, mb_ltrim, mb_rtrim);

fn mb_detect_encoding(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    let list = match a.opt(1) {
        Some(v) => Some(OwnedList::from_value(vm, &v)?),
        None => None,
    };
    let strict = a.has(2).then(|| a.bool(2));
    let all = list.as_ref().is_some_and(OwnedList::is_all);
    let r = call(vm, a, |st| {
        st.mb_detect_encoding(s.as_bytes(), list.as_ref().map(OwnedList::as_list), strict, all)
    })?;
    Ok(r.map_or(Value::Bool(false), Value::from))
}

fn mb_encode_mimeheader(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, charset, te, nl, indent) = (
        a.str(0),
        opt_str(a, 1),
        opt_str(a, 2),
        a.str(3),
        a.int(4),
    );
    call(vm, a, |st| {
        st.mb_encode_mimeheader(
            s.as_bytes(),
            opt_bytes(charset.as_ref()),
            opt_bytes(te.as_ref()),
            nl.as_bytes(),
            indent,
        )
    })
    .map(bytes)
}

fn mb_decode_mimeheader(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    call(vm, a, |st| Ok(st.mb_decode_mimeheader(s.as_bytes()))).map(bytes)
}

fn mb_convert_kana(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, mode, enc) = (a.str(0), a.str(1), opt_str(a, 2));
    call(vm, a, |st| {
        st.mb_convert_kana(s.as_bytes(), Some(mode.as_bytes()), opt_bytes(enc.as_ref()))
    })
    .map(bytes)
}

/// The `$map` argument of the numeric-entity functions
/// (`zval_try_get_long` on each element).
fn conversion_map(vm: &mut Vm, arr: &ZArray) -> Vec<Option<i64>> {
    arr.iter()
        .map(|(_, v)| {
            php_runtime::convert::try_get_long(&v.deref(), vm)
                .ok()
                .flatten()
        })
        .collect()
}

fn mb_encode_numericentity(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, map, enc, hex) = (a.str(0), a.array(1), opt_str(a, 2), a.bool(3));
    let map = conversion_map(vm, &map);
    call(vm, a, |st| {
        st.mb_encode_numericentity(s.as_bytes(), &map, opt_bytes(enc.as_ref()), hex)
    })
    .map(bytes)
}

fn mb_decode_numericentity(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, map, enc) = (a.str(0), a.array(1), opt_str(a, 2));
    let map = conversion_map(vm, &map);
    call(vm, a, |st| {
        st.mb_decode_numericentity(s.as_bytes(), &map, opt_bytes(enc.as_ref()))
    })
    .map(bytes)
}

fn mb_ord(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, enc) = (a.str(0), opt_str(a, 1));
    call(vm, a, |st| st.mb_ord(s.as_bytes(), opt_bytes(enc.as_ref()))).map(int_or_false)
}

fn mb_chr(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (cp, enc) = (a.int(0), opt_str(a, 1));
    call(vm, a, |st| st.mb_chr(cp, opt_bytes(enc.as_ref()))).map(str_or_false)
}

fn mb_str_pad(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    // `STR_PAD_RIGHT` is not a literal the stub parser knows.
    let ty = if a.has(3) { a.int(3) } else { 1 };
    let (s, len, pad, enc) = (a.str(0), a.int(1), a.str(2), opt_str(a, 4));
    call(vm, a, |st| {
        st.mb_str_pad(
            s.as_bytes(),
            len,
            pad.as_bytes(),
            ty,
            opt_bytes(enc.as_ref()),
        )
    })
    .map(bytes)
}

/// Registers the functions of this module.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function("mb_language(?string $language = null): string|bool", mb_language)
        .function("mb_internal_encoding(?string $encoding = null): string|bool", mb_internal_encoding)
        .function("mb_http_input(?string $type = null): array|string|false", mb_http_input)
        .function("mb_http_output(?string $encoding = null): string|bool", mb_http_output)
        .function("mb_detect_order(array|string|null $encoding = null): array|bool", mb_detect_order)
        .function(
            "mb_substitute_character(string|int|null $substitute_character = null): string|int|bool",
            mb_substitute_character,
        )
        .function("mb_preferred_mime_name(string $encoding): string|false", mb_preferred_mime_name)
        .function("mb_str_split(string $string, int $length = 1, ?string $encoding = null): array", mb_str_split)
        .function("mb_strlen(string $string, ?string $encoding = null): int", mb_strlen)
        .function("mb_strpos(string $haystack, string $needle, int $offset = 0, ?string $encoding = null): int|false", mb_strpos)
        .function("mb_strrpos(string $haystack, string $needle, int $offset = 0, ?string $encoding = null): int|false", mb_strrpos)
        .function("mb_stripos(string $haystack, string $needle, int $offset = 0, ?string $encoding = null): int|false", mb_stripos)
        .function("mb_strripos(string $haystack, string $needle, int $offset = 0, ?string $encoding = null): int|false", mb_strripos)
        .function("mb_strstr(string $haystack, string $needle, bool $before_needle = false, ?string $encoding = null): string|false", mb_strstr)
        .function("mb_strrchr(string $haystack, string $needle, bool $before_needle = false, ?string $encoding = null): string|false", mb_strrchr)
        .function("mb_stristr(string $haystack, string $needle, bool $before_needle = false, ?string $encoding = null): string|false", mb_stristr)
        .function("mb_strrichr(string $haystack, string $needle, bool $before_needle = false, ?string $encoding = null): string|false", mb_strrichr)
        .function("mb_substr_count(string $haystack, string $needle, ?string $encoding = null): int", mb_substr_count)
        .function("mb_substr(string $string, int $start, ?int $length = null, ?string $encoding = null): string", mb_substr)
        .function("mb_strcut(string $string, int $start, ?int $length = null, ?string $encoding = null): string", mb_strcut)
        .function("mb_strwidth(string $string, ?string $encoding = null): int", mb_strwidth)
        .function("mb_strimwidth(string $string, int $start, int $width, string $trim_marker = \"\", ?string $encoding = null): string", mb_strimwidth)
        .function("mb_convert_case(string $string, int $mode, ?string $encoding = null): string", mb_convert_case)
        .function("mb_strtoupper(string $string, ?string $encoding = null): string", mb_strtoupper)
        .function("mb_strtolower(string $string, ?string $encoding = null): string", mb_strtolower)
        .function("mb_ucfirst(string $string, ?string $encoding = null): string", mb_ucfirst)
        .function("mb_lcfirst(string $string, ?string $encoding = null): string", mb_lcfirst)
        .function("mb_trim(string $string, ?string $characters = null, ?string $encoding = null): string", mb_trim)
        .function("mb_ltrim(string $string, ?string $characters = null, ?string $encoding = null): string", mb_ltrim)
        .function("mb_rtrim(string $string, ?string $characters = null, ?string $encoding = null): string", mb_rtrim)
        .function("mb_detect_encoding(string $string, array|string|null $encodings = null, bool $strict = false): string|false", mb_detect_encoding)
        .function("mb_list_encodings(): array", mb_list_encodings)
        .function("mb_encoding_aliases(string $encoding): array", mb_encoding_aliases)
        .function(
            "mb_encode_mimeheader(string $string, ?string $charset = null, ?string $transfer_encoding = null, string $newline = \"\\r\\n\", int $indent = 0): string",
            mb_encode_mimeheader,
        )
        .function("mb_decode_mimeheader(string $string): string", mb_decode_mimeheader)
        .function("mb_convert_kana(string $string, string $mode = \"KV\", ?string $encoding = null): string", mb_convert_kana)
        .function("mb_encode_numericentity(string $string, array $map, ?string $encoding = null, bool $hex = false): string", mb_encode_numericentity)
        .function("mb_decode_numericentity(string $string, array $map, ?string $encoding = null): string", mb_decode_numericentity)
        .function("mb_get_info(string $type = \"all\"): array|string|int|false|null", mb_get_info)
        .function("mb_scrub(string $string, ?string $encoding = null): string", mb_scrub)
        .function("mb_ord(string $string, ?string $encoding = null): int|false", mb_ord)
        .function("mb_chr(int $codepoint, ?string $encoding = null): string|false", mb_chr)
        .function(
            "mb_str_pad(string $string, int $length, string $pad_string = \" \", int $pad_type = STR_PAD_RIGHT, ?string $encoding = null): string",
            mb_str_pad,
        );
}
