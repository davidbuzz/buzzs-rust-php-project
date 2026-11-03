//! The `mb_*` functions that walk arrays (and objects):
//! `mb_convert_encoding` on arrays (`php_mb_convert_encoding_recursive`),
//! `mb_check_encoding` on arrays (`php_mb_check_encoding_recursive`) and
//! `mb_convert_variables` (`mb_recursive_*`).

use php_mbstring::strings::check_str_encoding;
use php_mbstring::{Encoding, MbState};
use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::funcs::OwnedList;
use crate::state::{self, bytes, call, opt_str};

/// Nesting limit standing in for `mb_check_stack_limit()`.
const MAX_DEPTH: usize = 4096;

/// Recursion protection (`GC_PROTECT_RECURSION`) by array identity.
#[derive(Default)]
struct Guard(Vec<usize>);

impl Guard {
    fn enter(&mut self, addr: usize) -> bool {
        if self.0.contains(&addr) || self.0.len() >= MAX_DEPTH {
            return false;
        }
        self.0.push(addr);
        true
    }

    fn leave(&mut self) {
        self.0.pop();
    }
}

// ---- mb_convert_encoding -------------------------------------------------

struct Converter<'a> {
    to: &'static Encoding,
    from: &'a [&'static Encoding],
    guard: Guard,
}

enum Item {
    Keep(Value),
    Skip,
}

impl Converter<'_> {
    fn string(&self, s: &[u8]) -> Option<Vec<u8>> {
        state::with(|st| st.convert_encoding_value(s, self.to, self.from))
    }

    /// `php_mb_convert_encoding_recursive`; `None` is PHP's `NULL` result
    /// (recursion detected).
    fn array(&mut self, vm: &mut Vm, input: &ZArray) -> VmResult<Option<HashTable>> {
        if !self.guard.enter(input.addr()) {
            vm.php_warning("Cannot convert recursively referenced values")?;
            return Ok(None);
        }
        let mut out = HashTable::with_capacity(input.len());
        for (key, entry) in input.iter() {
            let key = match key {
                ArrayKey::Str(k) => {
                    let conv = self.string(k.as_bytes());
                    state::flush_diagnostics(vm)?;
                    match conv {
                        Some(k) => ArrayKey::Str(ZStr::new(k)),
                        None => continue,
                    }
                }
                ArrayKey::Int(i) => ArrayKey::Int(*i),
            };
            let item = match entry.deref() {
                Value::Str(s) => {
                    let conv = self.string(s.as_bytes());
                    state::flush_diagnostics(vm)?;
                    conv.map_or(Item::Skip, |c| Item::Keep(bytes(c)))
                }
                v @ (Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)) => {
                    Item::Keep(v)
                }
                Value::Array(inner) => {
                    let t = self.array(vm, &inner)?;
                    Item::Keep(Value::Array(t.map_or_else(ZArray::new, ZArray::from_table)))
                }
                _ => {
                    vm.php_warning("Object is not supported")?;
                    Item::Skip
                }
            };
            if let Item::Keep(v) = item {
                // zend_hash_add: an existing key is kept.
                if !out.contains_key(&key) {
                    out.insert(key, v);
                }
            }
        }
        self.guard.leave();
        Ok(Some(out))
    }
}

fn mb_convert_encoding(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let input = a.at(0);
    let to = a.str(1);
    let from = match a.opt(2) {
        Some(v) => Some(OwnedList::from_value(vm, &v)?),
        None => None,
    };
    let (to_enc, from_list) = call(vm, a, |st| {
        st.convert_encoding_args(to.as_bytes(), from.as_ref().map(OwnedList::as_list))
    })?;
    match input {
        Value::Array(arr) => {
            let mut c = Converter {
                to: to_enc,
                from: &from_list,
                guard: Guard::default(),
            };
            let t = c.array(vm, &arr)?;
            Ok(t.map_or(Value::Null, |t| Value::Array(ZArray::from_table(t))))
        }
        other => {
            let s = vm.value_to_string(&other)?;
            let r = state::with(|st| st.convert_encoding_value(s.as_bytes(), to_enc, &from_list));
            state::flush_diagnostics(vm)?;
            Ok(r.map_or(Value::Bool(false), bytes))
        }
    }
}

// ---- mb_check_encoding ---------------------------------------------------

fn check_recursive(
    vm: &mut Vm,
    arr: &ZArray,
    enc: &'static Encoding,
    guard: &mut Guard,
) -> VmResult<bool> {
    if !guard.enter(arr.addr()) {
        vm.php_warning("Cannot not handle circular references")?;
        return Ok(false);
    }
    let mut valid = true;
    for (key, entry) in arr.iter() {
        if let ArrayKey::Str(k) = key {
            if !check_str_encoding(k.as_bytes(), enc) {
                valid = false;
                break;
            }
        }
        match entry.deref() {
            Value::Str(s) => {
                if !check_str_encoding(s.as_bytes(), enc) {
                    valid = false;
                }
            }
            Value::Array(inner) => {
                if !check_recursive(vm, &inner, enc, guard)? {
                    valid = false;
                }
            }
            Value::Int(_) | Value::Float(_) | Value::Null | Value::Bool(_) => {}
            _ => valid = false,
        }
    }
    guard.leave();
    Ok(valid)
}

fn mb_check_encoding(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let enc_name = opt_str(a, 1);
    let enc_arg = enc_name.as_ref().map(ZStr::as_bytes);
    match a.opt(0) {
        None => call(vm, a, |st| st.mb_check_encoding_noarg(enc_arg)).map(Value::Bool),
        Some(Value::Array(arr)) => {
            let enc = call(vm, a, |st| st.get_encoding(enc_arg, 2, "encoding"))?;
            check_recursive(vm, &arr, enc, &mut Guard::default()).map(Value::Bool)
        }
        Some(v) => {
            let s = vm.value_to_string(&v)?;
            call(vm, a, |st| st.mb_check_encoding(s.as_bytes(), enc_arg)).map(Value::Bool)
        }
    }
}

// ---- mb_convert_variables ------------------------------------------------

/// `mb_recursive_find_strings`; `false` means recursion was detected.
fn find_strings(v: &Value, out: &mut Vec<ZStr>, guard: &mut Guard) -> bool {
    match v.deref() {
        Value::Str(s) => {
            out.push(s);
            true
        }
        Value::Array(arr) => {
            if !guard.enter(arr.addr()) {
                return false;
            }
            for (_, e) in arr.iter() {
                if !find_strings(e, out, guard) {
                    return false;
                }
            }
            guard.leave();
            true
        }
        Value::Object(o) => {
            if !guard.enter(o.handle() as usize | (1 << 62)) {
                return false;
            }
            for e in object_values(&o) {
                if !find_strings(&e, out, guard) {
                    return false;
                }
            }
            guard.leave();
            true
        }
        _ => true,
    }
}

/// The property values of an object (declared slots, then dynamic ones).
fn object_values(o: &php_runtime::Obj) -> Vec<Value> {
    let d = php_vm::object::data(o);
    let mut vals: Vec<Value> = d.slots.borrow().iter().flatten().cloned().collect();
    if let Some(dynamic) = d.dynamic.borrow().as_ref() {
        vals.extend(dynamic.values().cloned());
    }
    vals
}

struct VarConverter {
    from: &'static Encoding,
    to: &'static Encoding,
    guard: Guard,
}

impl VarConverter {
    fn string(&self, s: &[u8]) -> Value {
        bytes(state::with(|st: &mut MbState| st.convert_ex(s, self.to, self.from)))
    }

    /// `mb_recursive_convert_variable` on a value: returns the converted
    /// value (references inside are updated in place); `None` means
    /// recursion was detected.
    fn value(&mut self, vm: &mut Vm, arg_num: usize, v: &Value) -> VmResult<Option<Value>> {
        if let Value::Reference(r) = v {
            let inner = r.get();
            let Some(nv) = self.value(vm, arg_num, &inner)? else {
                return Ok(None);
            };
            r.set(nv);
            return Ok(Some(v.clone()));
        }
        match v {
            Value::Str(s) => Ok(Some(self.string(s.as_bytes()))),
            Value::Array(arr) => {
                if !self.guard.enter(arr.addr()) {
                    return Ok(None);
                }
                let mut out = arr.clone();
                let t = out.make_mut();
                let keys: Vec<ArrayKey> = t.keys().cloned().collect();
                for k in keys {
                    let Some(e) = t.get(&k).cloned() else {
                        continue;
                    };
                    let Some(nv) = self.value(vm, arg_num, &e)? else {
                        return Ok(None);
                    };
                    if let Some(slot) = t.get_mut(&k) {
                        *slot = nv;
                    }
                }
                self.guard.leave();
                Ok(Some(Value::Array(out)))
            }
            Value::Object(o) => {
                vm.php_deprecated(&format!(
                    "Passing an object for argument #{arg_num} $vars to mb_convert_variables() is deprecated, call get_object_vars() first instead"
                ))?;
                if !self.guard.enter(o.handle() as usize | (1 << 62)) {
                    return Ok(None);
                }
                let d = php_vm::object::data(o);
                let n = d.slots.borrow().len();
                for i in 0..n {
                    let Some(e) = d.slots.borrow().get(i).cloned().flatten() else {
                        continue;
                    };
                    let Some(nv) = self.value(vm, arg_num, &e)? else {
                        return Ok(None);
                    };
                    if let Some(slot) = d.slots.borrow_mut().get_mut(i) {
                        *slot = Some(nv);
                    }
                }
                let keys: Vec<ArrayKey> = d
                    .dynamic
                    .borrow()
                    .as_ref()
                    .map(|t| t.keys().cloned().collect())
                    .unwrap_or_default();
                for k in keys {
                    let Some(e) = d.dynamic.borrow().as_ref().and_then(|t| t.get(&k).cloned())
                    else {
                        continue;
                    };
                    let Some(nv) = self.value(vm, arg_num, &e)? else {
                        return Ok(None);
                    };
                    if let Some(t) = d.dynamic.borrow_mut().as_mut() {
                        if let Some(slot) = t.get_mut(&k) {
                            *slot = nv;
                        }
                    }
                }
                self.guard.leave();
                Ok(Some(v.clone()))
            }
            other => Ok(Some(other.clone())),
        }
    }
}

fn mb_convert_variables(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let to = a.str(0);
    let from = OwnedList::from_value(vm, &a.at(1))?;
    let vars: Vec<Value> = a.values.iter().skip(2).cloned().collect();
    let mut strings = Vec::new();
    let mut recursive = false;
    let multi = match &from {
        OwnedList::Array { items, .. } => items.len() > 1,
        OwnedList::Str(s) => s.contains(&b',') || s.eq_ignore_ascii_case(b"auto"),
    };
    if multi {
        let mut guard = Guard::default();
        for v in &vars {
            if !find_strings(v, &mut strings, &mut guard) {
                recursive = true;
                break;
            }
        }
    }
    let refs: Vec<&[u8]> = strings.iter().map(ZStr::as_bytes).collect();
    let all = from.is_all();
    let r = state::with(|st| {
        if recursive {
            // Resolve the arguments (for their errors) before reporting.
            st.convert_variables_encodings(to.as_bytes(), from.as_list(), all, &[])
                .map(|_| None)
        } else {
            st.convert_variables_encodings(to.as_bytes(), from.as_list(), all, &refs)
        }
    });
    let r = state::finish(vm, a, r)?;
    if recursive {
        vm.php_warning("Cannot handle recursive references")?;
        return Ok(Value::Bool(false));
    }
    let Some((from_enc, to_enc)) = r else {
        return Ok(Value::Bool(false));
    };
    let mut conv = VarConverter {
        from: from_enc,
        to: to_enc,
        guard: Guard::default(),
    };
    for (i, v) in vars.iter().enumerate() {
        let target = match v {
            Value::Reference(r) => r.get(),
            other => other.clone(),
        };
        let Some(nv) = conv.value(vm, i + 3, &target)? else {
            vm.php_warning("Cannot handle recursive references")?;
            return Ok(Value::Bool(false));
        };
        if let Value::Reference(r) = v {
            r.set(nv);
        }
    }
    Ok(Value::from(from_enc.name))
}

/// Registers the array-walking functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function(
        "mb_convert_encoding(array|string $string, string $to_encoding, array|string|null $from_encoding = null): array|string|false",
        mb_convert_encoding,
    )
    .function(
        "mb_check_encoding(array|string|null $value = null, ?string $encoding = null): bool",
        mb_check_encoding,
    )
    .function(
        "mb_convert_variables(string $to_encoding, array|string $from_encoding, mixed &$var, mixed &...$vars): string|false",
        mb_convert_variables,
    );
}
