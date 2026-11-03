//! `Spoofchecker`.

use php_intl::spoofchecker::{self as lib, Check, Report, Spoofchecker};
use php_runtime::{Obj, Value, ZStr};
use php_vm::ext::{Args, NativeFn, VmResult};
use php_vm::Vm;

use crate::core;

fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    core::reset_global();
    if core::is_constructed::<Spoofchecker>(&o) {
        return Err(vm.throw("Error", lib::ALREADY_CONSTRUCTED));
    }
    let r = Spoofchecker::new();
    let _g = core::CtorGuard::new();
    match r {
        Ok(c) => core::set_data(&o, c),
        Err(e) => core::fail(vm, a, Some(&o), e, 0)?,
    }
    Ok(Value::Null)
}

fn fetch(vm: &mut Vm, a: &Args) -> VmResult<Obj> {
    let o = a.this_obj();
    core::reset_both(&o);
    if !core::is_constructed::<Spoofchecker>(&o) {
        return Err(vm.throw("Error", lib::UNCONSTRUCTED));
    }
    Ok(o)
}

fn sync_status(o: &Obj) {
    if let Some(st) = core::with::<Spoofchecker, _>(o, |c| c.status()) {
        core::with_obj_error(o, |e| e.code = st);
    }
}

fn report(vm: &mut Vm, r: Report) -> VmResult<()> {
    if let Some(c) = r.error {
        core::with_global(|g| g.code = c);
    }
    for w in r.warnings {
        vm.php_warning(&w)?;
    }
    Ok(())
}

fn finish_check(vm: &mut Vm, a: &Args, o: &Obj, c: Check, err_arg: usize) -> VmResult<Value> {
    sync_status(o);
    if let Some(code) = c.error_code {
        if a.has(err_arg) {
            a.set_ref(err_arg, Value::Int(code));
        }
    }
    report(vm, c.report)?;
    Ok(Value::Bool(c.value))
}

fn is_suspicious(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    let t = a.str(0);
    let Some(c) = core::with::<Spoofchecker, _>(&o, |c| c.is_suspicious(t.as_bytes())) else {
        return Ok(Value::Bool(false));
    };
    finish_check(vm, a, &o, c, 1)
}

fn are_confusable(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    let (x, y) = (a.str(0), a.str(1));
    let Some(c) =
        core::with::<Spoofchecker, _>(&o, |c| c.are_confusable(x.as_bytes(), y.as_bytes()))
    else {
        return Ok(Value::Bool(false));
    };
    finish_check(vm, a, &o, c, 2)
}

fn are_bidi_confusable(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    let (x, y) = (a.str(1), a.str(2));
    let dir = a.int(0);
    match core::with::<Spoofchecker, _>(&o, |c| {
        c.are_bidi_confusable(dir, x.as_bytes(), y.as_bytes())
    }) {
        Some(Ok(c)) => finish_check(vm, a, &o, c, 3),
        Some(Err(e)) => {
            sync_status(&o);
            core::fail_with(vm, a, Some(&o), e, 0, Value::Bool(false))
        }
        None => Ok(Value::Bool(false)),
    }
}

fn set_allowed_locales(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    let l = a.str(0);
    if let Some(r) = core::with::<Spoofchecker, _>(&o, |c| c.set_allowed_locales(l.as_bytes())) {
        sync_status(&o);
        report(vm, r)?;
    }
    Ok(Value::Null)
}

fn set_checks(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    let n = a.int(0);
    if let Some(r) = core::with::<Spoofchecker, _>(&o, |c| c.set_checks(n)) {
        sync_status(&o);
        report(vm, r)?;
    }
    Ok(Value::Null)
}

fn set_restriction_level(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    let n = a.int(0);
    if let Some(Err(e)) = core::with::<Spoofchecker, _>(&o, |c| c.set_restriction_level(n)) {
        core::fail(vm, a, Some(&o), e, 0)?;
    }
    sync_status(&o);
    Ok(Value::Null)
}

fn set_allowed_chars(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    let p = a.str(0);
    let opts = a.int(1);
    match core::with::<Spoofchecker, _>(&o, |c| c.set_allowed_chars(p.as_bytes(), opts)) {
        Some(Ok(r)) => {
            sync_status(&o);
            report(vm, r)?;
        }
        Some(Err(e)) => {
            sync_status(&o);
            core::fail(vm, a, Some(&o), e, 0)?;
        }
        None => {}
    }
    Ok(Value::Null)
}

fn skeleton_result(
    vm: &mut Vm,
    a: &Args,
    o: &Obj,
    r: Option<php_intl::Result<Vec<u8>>>,
) -> VmResult<Value> {
    match r {
        Some(Ok(v)) => {
            sync_status(o);
            Ok(Value::Str(ZStr::from(v)))
        }
        Some(Err(e)) => core::fail_with(vm, a, Some(o), e, 0, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

fn get_skeleton(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    let t = a.str(0);
    let r = core::with::<Spoofchecker, _>(&o, |c| c.get_skeleton(t.as_bytes()));
    skeleton_result(vm, a, &o, r)
}

fn get_bidi_skeleton(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    let t = a.str(1);
    let d = a.int(0);
    let r = core::with::<Spoofchecker, _>(&o, |c| c.get_bidi_skeleton(d, t.as_bytes()));
    skeleton_result(vm, a, &o, r)
}

/// `clone_obj`.
pub(crate) fn clone(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    core::create(vm, new)?;
    match core::with::<Spoofchecker, _>(old, |c| c.try_clone()) {
        Some(Ok(c)) => core::set_data(new, c),
        Some(Err(_)) => return Err(vm.throw("Error", lib::CLONE_FAILED)),
        None => return Err(vm.throw("Error", lib::CLONE_UNINITIALIZED)),
    }
    Ok(())
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        ("Spoofchecker::__construct", construct),
        ("Spoofchecker::isSuspicious", is_suspicious),
        ("Spoofchecker::areConfusable", are_confusable),
        ("Spoofchecker::setAllowedLocales", set_allowed_locales),
        ("Spoofchecker::setChecks", set_checks),
        ("Spoofchecker::setRestrictionLevel", set_restriction_level),
        ("Spoofchecker::setAllowedChars", set_allowed_chars),
        ("Spoofchecker::getSkeleton", get_skeleton),
        ("Spoofchecker::getBidiSkeleton", get_bidi_skeleton),
        ("Spoofchecker::areBidiConfusable", are_bidi_confusable),
    ]
}
