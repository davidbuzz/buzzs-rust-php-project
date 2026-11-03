//! Filters, transformations, interpolation, clipping, resolution and
//! FreeType text.

use php_gd::info::{GD_METHOD_COUNT, IMAGE_FILTER_MAX};
use php_gd::{affine, Filter, Image, Rect};
use php_runtime::{Value, ZArray};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::objects::{image_arg, image_or_false};
use crate::util::{
    arity, ci, exceeds_int, flush, try_get_long, unnamed_arg_error, zpp_bool, zpp_double, zpp_long,
    INT_MAX, INT_MIN,
};

fn imagerotate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let degrees = a.float(1);
    let (lo, hi) = (INT_MIN / 100, INT_MAX / 100);
    #[allow(clippy::cast_precision_loss)]
    if degrees < lo as f64 || degrees > hi as f64 {
        return Err(a.value_error(vm, 2, &format!("must be between {lo} and {hi}")));
    }
    let im = image_arg(vm, a, 0)?;
    #[allow(clippy::cast_possible_truncation)]
    let r = im.rotate(degrees as f32, ci(a.int(2)));
    flush(vm)?;
    image_or_false(vm, r)
}

/// Runs a filter and returns its result as a PHP bool.
fn run_filter(vm: &mut Vm, im: &Image, f: &Filter) -> VmResult<Value> {
    let ok = im.filter(f);
    flush(vm)?;
    Ok(Value::Bool(ok))
}

fn imagefilter(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let filter = a.int(1);
    if !(0..=IMAGE_FILTER_MAX).contains(&filter) {
        return Err(a.value_error(vm, 2, "must be one of the IMG_FILTER_* filter constants"));
    }
    let im = image_arg(vm, a, 0)?;
    let f = match filter {
        0 => Filter::Negate,
        1 => Filter::Grayscale,
        2 | 3 => {
            arity(vm, a, 3, 3)?;
            let v = zpp_long(vm, a, 2)?;
            if filter == 2 {
                Filter::Brightness(ci(v))
            } else {
                #[allow(clippy::cast_precision_loss)]
                Filter::Contrast(f64::from(ci(v)))
            }
        }
        4 => {
            arity(vm, a, 5, 6)?;
            let r = zpp_long(vm, a, 2)?;
            let g = zpp_long(vm, a, 3)?;
            let b = zpp_long(vm, a, 4)?;
            let al = if a.len() > 5 { zpp_long(vm, a, 5)? } else { 0 };
            Filter::Colorize(ci(r), ci(g), ci(b), ci(al))
        }
        5 => Filter::EdgeDetect,
        6 => Filter::Emboss,
        7 => Filter::GaussianBlur,
        8 => Filter::SelectiveBlur,
        9 => Filter::MeanRemoval,
        10 => {
            arity(vm, a, 3, 3)?;
            let w = zpp_double(vm, a, 2)?;
            #[allow(clippy::cast_possible_truncation)]
            Filter::Smooth(w as f32)
        }
        11 => {
            arity(vm, a, 3, 4)?;
            let bs = zpp_long(vm, a, 2)?;
            let mode = if a.len() > 3 {
                zpp_bool(vm, a, 3)?
            } else {
                false
            };
            Filter::Pixelate(ci(bs), mode)
        }
        _ => return scatter(vm, a, &im),
    };
    run_filter(vm, &im, &f)
}

/// `php_image_filter_scatter()`.
fn scatter(vm: &mut Vm, a: &Args, im: &Image) -> VmResult<Value> {
    arity(vm, a, 4, 5)?;
    let sub = zpp_long(vm, a, 2)?;
    let plus = zpp_long(vm, a, 3)?;
    let colors = if a.len() > 4 {
        match a.at(4) {
            Value::Array(arr) => Some(arr),
            other => {
                return Err(unnamed_arg_error(
                    vm,
                    a,
                    "TypeError",
                    5,
                    &format!("must be of type array, {} given", other.value_name()),
                ))
            }
        }
    } else {
        None
    };
    if sub < 0 || exceeds_int(sub) {
        return Err(unnamed_arg_error(
            vm,
            a,
            "ValueError",
            3,
            &format!("must be between 0 and {INT_MAX}"),
        ));
    }
    if plus < 0 || exceeds_int(plus) {
        return Err(unnamed_arg_error(
            vm,
            a,
            "ValueError",
            4,
            &format!("must be between 0 and {INT_MAX}"),
        ));
    }
    let mut list = Vec::new();
    if let Some(arr) = colors {
        for v in arr.values() {
            let v = v.deref();
            let Some(c) = try_get_long(vm, &v)? else {
                return Err(unnamed_arg_error(
                    vm,
                    a,
                    "TypeError",
                    5,
                    &format!("must be of type int, {} given", v.type_name()),
                ));
            };
            if c < 0 || exceeds_int(c) {
                return Err(unnamed_arg_error(
                    vm,
                    a,
                    "ValueError",
                    5,
                    &format!("value must be between 0 and {INT_MAX}"),
                ));
            }
            list.push(ci(c));
        }
    }
    run_filter(vm, im, &Filter::Scatter(ci(sub), ci(plus), list))
}

fn imageconvolution(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let m = a.array(1);
    let div = a.float(2);
    let offset = a.float(3);
    if m.len() != 3 {
        return Err(a.value_error(vm, 2, "must be a 3x3 array"));
    }
    let mut matrix = [[0f32; 3]; 3];
    for (i, row) in matrix.iter_mut().enumerate() {
        let Some(Value::Array(r)) = m.get_int(i64::try_from(i).unwrap_or(0)).map(Value::deref)
        else {
            continue;
        };
        if r.len() != 3 {
            return Err(a.value_error(
                vm,
                2,
                &format!(
                    "must be a 3x3 array, matrix[{i}] only has {} elements",
                    r.len()
                ),
            ));
        }
        for (j, cell) in row.iter_mut().enumerate() {
            let Some(v) = r.get_int(i64::try_from(j).unwrap_or(0)).cloned() else {
                return Err(a.value_error(
                    vm,
                    2,
                    &format!(
                        "must be a 3x3 array, matrix[{i}][{j}] cannot be found (missing integer key)"
                    ),
                ));
            };
            #[allow(clippy::cast_possible_truncation)]
            {
                *cell = vm.value_to_float(&v)? as f32;
            }
        }
    }
    if !div.is_finite() {
        return Err(a.value_error(vm, 3, "must be finite"));
    }
    #[allow(clippy::cast_possible_truncation)]
    let div_f = div as f32;
    if div_f == 0.0 {
        return Err(a.value_error(vm, 3, "must not be 0"));
    }
    if !offset.is_finite() {
        return Err(a.value_error(vm, 4, "must be finite"));
    }
    #[allow(clippy::cast_possible_truncation)]
    let f = Filter::Convolution(matrix, div_f, offset as f32);
    run_filter(vm, &im, &f)
}

fn imageflip(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let mode = a.int(1);
    if !(1..=3).contains(&mode) {
        return Err(a.value_error(
            vm,
            2,
            "must be one of IMG_FLIP_VERTICAL, IMG_FLIP_HORIZONTAL, or IMG_FLIP_BOTH",
        ));
    }
    im.flip(ci(mode));
    flush(vm)?;
    Ok(Value::Bool(true))
}

fn imageantialias(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    if im.is_truecolor() {
        im.set_antialias_flag(a.bool(1));
    }
    Ok(Value::Bool(true))
}

/// `php_gd_zval_try_get_c_int()` for a key of the rectangle argument.
fn rect_int(vm: &mut Vm, a: &Args, v: &Value, field: &str) -> VmResult<i32> {
    let v = v.deref();
    let Some(r) = try_get_long(vm, &v)? else {
        return Err(a.type_error(
            vm,
            2,
            &format!(
                "\"{field}\" key must be of type int, {} given",
                v.type_name()
            ),
        ));
    };
    if exceeds_int(r) {
        return Err(a.value_error(
            vm,
            2,
            &format!("\"{field}\" key must be between {INT_MIN} and {INT_MAX}"),
        ));
    }
    Ok(ci(r))
}

fn imagecrop(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let arr = a.array(1);
    let mut vals = [0i32; 4];
    for (i, (key, article)) in [("x", "an"), ("y", "a"), ("width", "a"), ("height", "a")]
        .into_iter()
        .enumerate()
    {
        let Some(v) = arr.get_str(key.as_bytes()).cloned() else {
            return Err(a.value_error(vm, 2, &format!("must have {article} \"{key}\" key")));
        };
        vals[i] = rect_int(vm, a, &v, key)?;
    }
    let [x, y, w, h] = vals;
    if w > 0 && x > i32::MAX - w {
        return Err(a.value_error(vm, 2, "overflow with \"x\" and \"width\" keys"));
    }
    if w < 0 && x < i32::MIN - w {
        return Err(a.value_error(vm, 2, "underflow with \"x\" and \"width\" keys"));
    }
    if h > 0 && y > i32::MAX - h {
        return Err(a.value_error(vm, 2, "overflow with \"y\" and \"height\" keys"));
    }
    if h < 0 && y < i32::MIN - h {
        return Err(a.value_error(vm, 2, "underflow with \"y\" and \"height\" keys"));
    }
    let r = im.crop(Rect {
        x,
        y,
        width: w,
        height: h,
    });
    flush(vm)?;
    image_or_false(vm, r)
}

fn imagecropauto(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let mode = a.int(1);
    let threshold = a.float(2);
    let color = a.int(3);
    let r = match mode {
        0..=4 => im.crop_auto(u32::try_from(mode).unwrap_or(0)),
        5 => {
            if color < 0 || (!im.is_truecolor() && color >= i64::from(im.colors_total())) {
                return Err(a.value_error(
                    vm,
                    4,
                    "must be greater than or equal to 0 when using the threshold mode",
                ));
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            im.crop_threshold(color as u32, threshold as f32)
        }
        _ => return Err(a.value_error(vm, 2, "must be a valid mode")),
    };
    flush(vm)?;
    image_or_false(vm, r)
}

fn imagescale(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let mut w = a.int(1);
    let mut h = a.int(2);
    let method = a.int(3);
    if !(0..GD_METHOD_COUNT).contains(&method) {
        return Err(a.value_error(vm, 4, "must be one of the GD_* constants"));
    }
    if h < 0 && w < 0 {
        return Err(vm.throw(
            "ValueError",
            "Argument #2 ($width) and argument #3 ($height) cannot be both negative",
        ));
    }
    if h < 0 || w < 0 {
        let sx = i64::from(im.width());
        let sy = i64::from(im.height());
        if sx != 0 && h < 0 {
            if w > i64::MAX / sy {
                return Err(a.value_error(
                    vm,
                    2,
                    &format!("must be less than or equal to {}", i64::MAX / sy),
                ));
            }
            h = w * sy / sx;
        }
        if sy != 0 && w < 0 {
            if h > i64::MAX / sx {
                return Err(a.value_error(
                    vm,
                    3,
                    &format!("must be less than or equal to {}", i64::MAX / sx),
                ));
            }
            w = h * sx / sy;
        }
    }
    if w <= 0 || exceeds_int(w) {
        return Err(a.value_error(vm, 2, &format!("must be between 1 and {INT_MAX}")));
    }
    if h <= 0 || exceeds_int(h) {
        return Err(a.value_error(vm, 3, &format!("must be between 1 and {INT_MAX}")));
    }
    let old = im.interpolation_id();
    let mut r = None;
    if im.set_interpolation(ci(method)) {
        r = im.scale(u32::try_from(w).unwrap_or(0), u32::try_from(h).unwrap_or(0));
    }
    im.set_interpolation(old);
    flush(vm)?;
    image_or_false(vm, r)
}

fn imageaffine(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let m = a.array(1);
    if m.len() != 6 {
        return Err(a.value_error(vm, 2, "must have 6 elements"));
    }
    let mut affine = [0f64; 6];
    for (i, slot) in affine.iter_mut().enumerate() {
        let Some(v) = m.get_int(i64::try_from(i).unwrap_or(0)).map(Value::deref) else {
            continue;
        };
        match v {
            Value::Int(_) | Value::Float(_) | Value::Str(_) => {
                let d = vm.value_to_float(&v)?;
                #[allow(clippy::cast_precision_loss)]
                if d < INT_MIN as f64 || d > INT_MAX as f64 {
                    return Err(a.value_error(
                        vm,
                        2,
                        &format!("element {i} must be between {INT_MIN} and {INT_MAX}"),
                    ));
                }
                *slot = d;
            }
            _ => {
                return Err(a.type_error(vm, 3, &format!("contains invalid type for element {i}")));
            }
        }
    }
    let clip = match a.opt(2) {
        Some(Value::Array(r)) => {
            let mut vals = [0i32; 4];
            for (i, (key, article)) in [("x", "an"), ("y", "a"), ("width", "a"), ("height", "a")]
                .into_iter()
                .enumerate()
            {
                let Some(v) = r.get_str(key.as_bytes()).cloned() else {
                    return Err(a.value_error(
                        vm,
                        3,
                        &format!("must have {article} \"{key}\" key"),
                    ));
                };
                vals[i] = ci(vm.value_to_int(&v)?);
            }
            Some(Rect {
                x: vals[0],
                y: vals[1],
                width: vals[2],
                height: vals[3],
            })
        }
        _ => None,
    };
    let r = im.affine(&affine, clip);
    flush(vm)?;
    image_or_false(vm, r)
}

/// A packed array of six floats.
fn matrix_value(m: [f64; 6]) -> Value {
    Value::Array(ZArray::from_values(m.into_iter().map(Value::Float)))
}

fn imageaffinematrixget(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let kind = a.int(0);
    let opt = a.at(1);
    let res = match kind {
        0 | 1 => {
            let Value::Array(o) = &opt else {
                return Err(a.type_error(
                    vm,
                    2,
                    "must be of type array when using translate or scale",
                ));
            };
            let Some(x) = o.get_str(b"x").cloned() else {
                return Err(a.value_error(vm, 2, "must have an \"x\" key"));
            };
            let x = vm.value_to_float(&x)?;
            let Some(y) = o.get_str(b"y").cloned() else {
                return Err(a.value_error(vm, 2, "must have a \"y\" key"));
            };
            let y = vm.value_to_float(&y)?;
            if kind == 0 {
                affine::translate(x, y)
            } else {
                affine::scale(x, y)
            }
        }
        2..=4 => {
            if matches!(opt, Value::Array(_)) {
                return Err(a.type_error(
                    vm,
                    2,
                    "must be of type float when using rotate or shear",
                ));
            }
            let angle = vm.value_to_float(&opt)?;
            match kind {
                3 => affine::shear_horizontal(angle),
                4 => affine::shear_vertical(angle),
                _ => affine::rotate(angle),
            }
        }
        _ => return Err(a.value_error(vm, 1, "must be a valid element type")),
    };
    Ok(res.map_or(Value::Bool(false), matrix_value))
}

/// The elements of a matrix argument of `imageaffinematrixconcat()`.
fn matrix_arg(
    vm: &mut Vm,
    a: &Args,
    n: usize,
    m: &ZArray,
    out: &mut [f64; 6],
    i: usize,
) -> VmResult<()> {
    if let Some(v) = m.get_int(i64::try_from(i).unwrap_or(0)).map(Value::deref) {
        match v {
            Value::Int(_) | Value::Float(_) | Value::Str(_) => out[i] = vm.value_to_float(&v)?,
            _ => {
                return Err(a.type_error(
                    vm,
                    n + 1,
                    &format!("contains invalid type for element {i}"),
                ));
            }
        }
    }
    Ok(())
}

fn imageaffinematrixconcat(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m1 = a.array(0);
    let m2 = a.array(1);
    if m1.len() != 6 {
        return Err(a.value_error(vm, 1, "must have 6 elements"));
    }
    if m2.len() != 6 {
        return Err(a.value_error(vm, 2, "must have 6 elements"));
    }
    let (mut x1, mut x2) = ([0f64; 6], [0f64; 6]);
    for i in 0..6 {
        matrix_arg(vm, a, 0, &m1, &mut x1, i)?;
        matrix_arg(vm, a, 1, &m2, &mut x2, i)?;
    }
    Ok(affine::concat(&x1, &x2).map_or(Value::Bool(false), matrix_value))
}

fn imagegetinterpolation(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    Ok(Value::Int(i64::from(im.interpolation_id())))
}

fn imagesetinterpolation(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let mut method = a.int(1);
    if method == -1 {
        method = 3;
    }
    Ok(Value::Bool(im.set_interpolation(ci(method))))
}

fn imageresolution(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let rx = a.opt(1).map(|_| a.int(1));
    let ry = a.opt(2).map(|_| a.int(2));
    let check = |vm: &mut Vm, n: usize, v: i64| -> VmResult<u32> {
        u32::try_from(v)
            .map_err(|_| a.value_error(vm, n, &format!("must be between 0 and {}", u32::MAX)))
    };
    match (rx, ry) {
        (Some(x), Some(y)) => {
            let x = check(vm, 2, x)?;
            let y = check(vm, 3, y)?;
            im.set_resolution(x, y);
        }
        (Some(x), None) => {
            let x = check(vm, 2, x)?;
            im.set_resolution(x, x);
        }
        (None, Some(y)) => {
            let y = check(vm, 3, y)?;
            im.set_resolution(y, y);
        }
        (None, None) => {
            let (x, y) = im.resolution();
            return Ok(Value::Array(ZArray::from_values([
                Value::Int(i64::from(x)),
                Value::Int(i64::from(y)),
            ])));
        }
    }
    Ok(Value::Bool(true))
}

fn imagesetclip(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    im.set_clip(ci(a.int(1)), ci(a.int(2)), ci(a.int(3)), ci(a.int(4)));
    flush(vm)?;
    Ok(Value::Bool(true))
}

fn imagegetclip(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    Ok(Value::Array(ZArray::from_values(
        im.clip().into_iter().map(|v| Value::Int(i64::from(v))),
    )))
}

/// `php_imagettftext_common()`.
fn ttftext(vm: &mut Vm, a: &Args, draw: bool) -> VmResult<Value> {
    let (im, base) = if draw {
        (Some(image_arg(vm, a, 0)?), 1)
    } else {
        (None, 0)
    };
    let size = a.float(base);
    let angle = a.float(base + 1);
    let (x, y, col) = if draw {
        (ci(a.int(3)), ci(a.int(4)), ci(a.int(5)))
    } else {
        (0, 0, -1)
    };
    let font = a.str(if draw { 6 } else { 2 });
    let text = a.str(if draw { 7 } else { 3 });
    let ext_idx = if draw { 8 } else { 4 };
    // FT_F26Dot6 is a signed long.
    let (lo, hi) = (i64::MIN / 64, i64::MAX / 64);
    #[allow(clippy::cast_precision_loss)]
    if size < i64::MIN as f64 / 64.0 || size >= i64::MAX as f64 / 64.0 {
        return Err(a.value_error(vm, base + 1, &format!("must be between {lo} and {hi}")));
    }
    if !size.is_finite() {
        return Err(a.value_error(vm, base + 1, "must be finite"));
    }
    let angle = angle * (std::f64::consts::PI / 180.0);
    let mut linespacing = None;
    if let Some(Value::Array(ext)) = a.get(ext_idx) {
        if !ext.is_packed() {
            if let Some(v) = ext.get_str(b"linespacing").cloned() {
                linespacing = Some(vm.value_to_float(&v)?);
            }
        }
    }
    let r = Image::string_ft(
        im.as_deref(),
        col,
        font.as_bytes(),
        size,
        angle,
        x,
        y,
        text.as_bytes(),
        linespacing,
    );
    flush(vm)?;
    match r {
        Ok(brect) => Ok(Value::Array(ZArray::from_values(
            brect.into_iter().map(|v| Value::Int(i64::from(v))),
        ))),
        Err(msg) => {
            vm.php_warning_bytes(&msg)?;
            Ok(Value::Bool(false))
        }
    }
}

fn imageftbbox(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    ttftext(vm, a, false)
}

fn imagefttext(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    ttftext(vm, a, true)
}

/// Registers the functions of this module.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let fns: &[(&str, php_vm::ext::NativeFn)] = &[
        (
            "imagerotate(GdImage $image, float $angle, int $background_color): GdImage|false",
            imagerotate,
        ),
        ("imagefilter(GdImage $image, int $filter, ...$args): bool", imagefilter),
        (
            "imageconvolution(GdImage $image, array $matrix, float $divisor, float $offset): bool",
            imageconvolution,
        ),
        ("imageflip(GdImage $image, int $mode): true", imageflip),
        ("imageantialias(GdImage $image, bool $enable): true", imageantialias),
        ("imagecrop(GdImage $image, array $rectangle): GdImage|false", imagecrop),
        (
            "imagecropauto(GdImage $image, int $mode = IMG_CROP_DEFAULT, float $threshold = 0.5, int $color = -1): GdImage|false",
            imagecropauto,
        ),
        (
            "imagescale(GdImage $image, int $width, int $height = -1, int $mode = IMG_BILINEAR_FIXED): GdImage|false",
            imagescale,
        ),
        (
            "imageaffine(GdImage $image, array $affine, ?array $clip = null): GdImage|false",
            imageaffine,
        ),
        (
            "imageaffinematrixget(int $type, array|float $options): array|false",
            imageaffinematrixget,
        ),
        (
            "imageaffinematrixconcat(array $matrix1, array $matrix2): array|false",
            imageaffinematrixconcat,
        ),
        ("imagegetinterpolation(GdImage $image): int", imagegetinterpolation),
        (
            "imagesetinterpolation(GdImage $image, int $method = IMG_BILINEAR_FIXED): bool",
            imagesetinterpolation,
        ),
        (
            "imageresolution(GdImage $image, ?int $resolution_x = null, ?int $resolution_y = null): array|true",
            imageresolution,
        ),
        (
            "imagesetclip(GdImage $image, int $x1, int $y1, int $x2, int $y2): true",
            imagesetclip,
        ),
        ("imagegetclip(GdImage $image): array", imagegetclip),
        (
            "imageftbbox(float $size, float $angle, string $font_filename, string $string, array $options = []): array|false",
            imageftbbox,
        ),
        (
            "imagefttext(GdImage $image, float $size, float $angle, int $x, int $y, int $color, string $font_filename, string $text, array $options = []): array|false",
            imagefttext,
        ),
        (
            "imagettfbbox(float $size, float $angle, string $font_filename, string $string, array $options = []): array|false",
            imageftbbox,
        ),
        (
            "imagettftext(GdImage $image, float $size, float $angle, int $x, int $y, int $color, string $font_filename, string $text, array $options = []): array|false",
            imagefttext,
        ),
    ];
    for &(sig, f) in fns {
        m.function(sig, f);
    }
}
