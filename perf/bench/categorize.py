#!/usr/bin/env python3
"""Buckets `perf report --no-children -F overhead,sym` lines (stdin) into
interpreter cost categories.  First matching rule wins; order matters."""
import re
import sys

RULES = [
    ('alloc (malloc/free/counting allocator)', r'malloc|free\b|_int_free|_int_malloc|realloc|cfree|CountingAlloc|__rust_alloc|__rust_dealloc|__rdl_|alloc::alloc|memlimit'),
    ('hash lookups (SipHash/hashbrown/name maps)', r'hashbrown|sip::|SipHasher|core::hash|BuildHasher|HashMap|HashSet|lookup_class|find_or_add|VarStore::find|ZStr as core::hash'),
    ('frames & calls', r'push_user_frame|Frame::|drop_in_place<php_vm::vm::Frame|call_builtin|op_send|op_do_call|op_init_fcall|op_init_method|op_init_static|op_return|PendingCall|op_recv|recv_check|method_call|static_method_call|frame_args|call_pending|call_value|bind_closure|op_new\b|VarStore::with_names'),
    ('type checks (params/returns/props)', r'verify_return|TypeDecl|typed_refs|type_check|coerce|accepts|check_type'),
    ('places / CV access', r'resolve_place|write_place|write_var_root|place_|descend|read_cv|cv_assign|cv_get|cv_bind|apply_final|fetch_dim|key_of|flush_deferred'),
    ('properties / objects', r'props::|object::|obj_walk|read_property|write_property|PhpObject|ObjectData|Obj::|class_of|find_prop|prop_'),
    ('Value clone/drop / refcounts', r'drop_in_place<php_runtime|<php_runtime::value::Value as core::clone|Value::deref|PhpRef|Rc<|rc::Rc|drop_in_place<core::option::Option<php_runtime|drop_in_place<alloc::vec::Vec<php_runtime|ZStr::drop|gc::possible_root|copy_on_assign'),
    ('arrays (HashTable)', r'php_runtime::array|HashTable|ZArray|php_stdarr'),
    ('strings', r'php_runtime::string|ZStr|memcpy|memmove|memcmp|php_stdstr|__memc|__memm|strlen'),
    ('operators / conversions', r'php_runtime::ops|php_runtime::convert|binary_op|to_bool|to_long|to_double|to_zstr|is_identical|is_equal|is_smaller|compare|php_runtime::number|fmt::float|dtoa'),
    ('dispatch loop (Vm::run + tail checks)', r'exec::<impl php_vm::vm::Vm>::run|execute_inner|has_pending_destructors|wants_run|check_interrupt|is_pending|php_vm::exec'),
    ('builtin function bodies', r'php_vm::builtins|php_std|php_hash|php_pcre|libm|__sin|__cos|__sqrt|sin\b|cos\b'),
    ('output', r'output|echo|write\b|php_support'),
    ('kernel / ld.so / startup', r'\[k\]|_dl_|do_lookup|ld-linux|parse_sig|ClassBuilder|with_extensions|register'),
]

def main():
    tot = {}
    other = {}
    for line in sys.stdin:
        m = re.match(r'\s*([\d.]+)%\s+(.*)$', line)
        if not m:
            continue
        pct, sym = float(m.group(1)), m.group(2)
        for name, rx in RULES:
            if re.search(rx, sym):
                tot[name] = tot.get(name, 0.0) + pct
                break
        else:
            other[sym] = other.get(sym, 0.0) + pct
    total_other = sum(other.values())
    for name, v in sorted(tot.items(), key=lambda kv: -kv[1]):
        print(f'{v:6.1f}%  {name}')
    print(f'{total_other:6.1f}%  other')
    for sym, v in sorted(other.items(), key=lambda kv: -kv[1])[:8]:
        print(f'         {v:5.1f}%  {sym[:110]}')

if __name__ == '__main__':
    main()
