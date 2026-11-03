//! The `GdImage` and `GdFont` classes: final, not serializable, not
//! cloneable, not constructible from PHP; each instance carries its
//! library value behind an `Rc`.

use std::rc::Rc;

use php_gd::{Font, Image};
use php_runtime::{Obj, Value};
use php_vm::ext::{class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::{object, Vm};

/// `GdImage`.
pub(crate) const IMAGE_CLASS: &str = "GdImage";
/// `GdFont`.
pub(crate) const FONT_CLASS: &str = "GdFont";

/// Internal state of a `GdImage`.
struct ImageState(Rc<Image>);

/// Internal state of a `GdFont`.
struct FontState(Rc<Font>);

fn image_ctor(vm: &mut Vm, _o: &Obj) -> VmResult<()> {
    Err(vm.throw(
        "Error",
        "You cannot initialize a GdImage object except through helper functions",
    ))
}

fn font_ctor(vm: &mut Vm, _o: &Obj) -> VmResult<()> {
    Err(vm.throw(
        "Error",
        "You cannot initialize a GdFont object except through helper functions",
    ))
}

/// `zend_objects_not_comparable` (identity still compares equal).
#[allow(clippy::unnecessary_wraps)]
fn not_comparable(_vm: &mut Vm, a: &Value, b: &Value) -> VmResult<Option<i32>> {
    if let (Value::Object(x), Value::Object(y)) = (a, b) {
        if Obj::ptr_eq(x, y) {
            return Ok(Some(0));
        }
    }
    Ok(Some(1))
}

/// Registers both classes.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let flags = class_flags::FINAL
        | class_flags::NOT_SERIALIZABLE
        | class_flags::NO_CLONE
        | class_flags::NO_DYNAMIC_PROPERTIES;
    ClassBuilder::new(IMAGE_CLASS)
        .flags(flags)
        .hooks(ClassHooks {
            get_constructor: Some(image_ctor),
            compare: Some(not_comparable),
            ..ClassHooks::default()
        })
        .register(m.vm());
    ClassBuilder::new(FONT_CLASS)
        .flags(flags)
        .hooks(ClassHooks {
            get_constructor: Some(font_ctor),
            ..ClassHooks::default()
        })
        .register(m.vm());
}

fn instantiate(vm: &mut Vm, class: &str) -> VmResult<Obj> {
    let Some(cls) = vm.find_class(class.as_bytes()) else {
        return Err(vm.throw("Error", &format!("Class \"{class}\" not found")));
    };
    vm.new_object(&cls)
}

/// `php_gd_assign_libgdimageptr_as_extgdimage()`.
pub(crate) fn new_image(vm: &mut Vm, im: Image) -> VmResult<Value> {
    let o = instantiate(vm, IMAGE_CLASS)?;
    object::set_internal(&o, ImageState(Rc::new(im)));
    Ok(Value::Object(o))
}

/// `Some(image)` → a new `GdImage`, `None` → `false`.
pub(crate) fn image_or_false(vm: &mut Vm, im: Option<Image>) -> VmResult<Value> {
    match im {
        Some(im) => new_image(vm, im),
        None => Ok(Value::Bool(false)),
    }
}

/// A new `GdFont`.
pub(crate) fn new_font(vm: &mut Vm, f: Font) -> VmResult<Value> {
    let o = instantiate(vm, FONT_CLASS)?;
    object::set_internal(&o, FontState(Rc::new(f)));
    Ok(Value::Object(o))
}

/// The image of a `GdImage` value.
pub(crate) fn image_of(v: &Value) -> Option<Rc<Image>> {
    match v.deref() {
        Value::Object(o) => object::internal::<ImageState, _>(&o, |s| Rc::clone(&s.0)),
        _ => None,
    }
}

/// Argument `n` (0-based), declared `GdImage`.
pub(crate) fn image_arg(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Rc<Image>> {
    let v = a.at(n);
    match image_of(&v) {
        Some(im) => Ok(im),
        None => Err(a.type_error(
            vm,
            n + 1,
            &format!("must be of type GdImage, {} given", v.value_name()),
        )),
    }
}

/// `php_find_gd_font()` for argument `n` (0-based), declared `GdFont|int`.
pub(crate) fn font_arg(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Rc<Font>> {
    let v = a.at(n);
    match &v {
        Value::Int(i) => Ok(Rc::new(Font::builtin(*i))),
        Value::Object(o) => match object::internal::<FontState, _>(o, |s| Rc::clone(&s.0)) {
            Some(f) => Ok(f),
            None => Err(a.type_error(
                vm,
                n + 1,
                &format!("must be of type GdFont|int, {} given", v.value_name()),
            )),
        },
        other => {
            let i = vm.value_to_int(other)?;
            Ok(Rc::new(Font::builtin(i)))
        }
    }
}
