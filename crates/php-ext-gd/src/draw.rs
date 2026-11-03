//! Colors, palettes, pixels, drawing primitives, bitmap text and copies.

use php_gd::{filters, font, Image, Point};
use php_runtime::{Value, ZArray};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::objects::{font_arg, image_arg};
use crate::util::{ci, exceeds_int, flush, try_get_long, E_DEPRECATED, INT_MAX, INT_MIN};

/// `true`, after raising libgd's messages.
fn done(vm: &mut Vm) -> VmResult<Value> {
    flush(vm)?;
    Ok(Value::Bool(true))
}

fn imagesetstyle(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let styles = a.array(1);
    if styles.is_empty() {
        return Err(a.value_error(vm, 2, "must not be empty"));
    }
    let mut out = Vec::with_capacity(styles.len());
    for v in styles.values() {
        let v = v.deref();
        let Some(i) = try_get_long(vm, &v)? else {
            return Err(a.type_error(
                vm,
                2,
                &format!(
                    "must only have elements of type int, {} given",
                    v.type_name()
                ),
            ));
        };
        if exceeds_int(i) {
            return Err(a.value_error(
                vm,
                2,
                &format!("elements must be between {INT_MIN} and {INT_MAX}"),
            ));
        }
        out.push(ci(i));
    }
    im.set_style(&out);
    done(vm)
}

fn imageistruecolor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    Ok(Value::Bool(im.is_truecolor()))
}

fn imagetruecolortopalette(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let dither = a.bool(1);
    let n = a.int(2);
    if n <= 0 || exceeds_int(n) {
        return Err(a.value_error(vm, 3, &format!("must be between 1 and {INT_MAX}")));
    }
    let ok = im.truecolor_to_palette(dither, ci(n));
    flush(vm)?;
    if ok {
        Ok(Value::Bool(true))
    } else {
        vm.php_warning("Couldn't convert to palette")?;
        Ok(Value::Bool(false))
    }
}

fn imagepalettetotruecolor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let ok = im.palette_to_truecolor();
    flush(vm)?;
    Ok(Value::Bool(ok))
}

fn imagecolormatch(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im1 = image_arg(vm, a, 0)?;
    let im2 = image_arg(vm, a, 1)?;
    let r = Image::color_match(&im1, &im2);
    flush(vm)?;
    match r {
        -1 => Err(a.value_error(vm, 1, "must be TrueColor")),
        -2 => Err(a.value_error(vm, 2, "must be Palette")),
        -3 => Err(a.value_error(vm, 2, "must be the same size as argument #1 ($im1)")),
        -4 => Err(a.value_error(vm, 2, "must have at least one color")),
        _ => Ok(Value::Bool(true)),
    }
}

fn imagesetthickness(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    im.set_thickness(ci(a.int(1)));
    done(vm)
}

fn imagefilledellipse(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let w = a.int(3);
    if w < 0 || exceeds_int(w) {
        return Err(a.value_error(vm, 4, &format!("must be between 0 and {INT_MAX}")));
    }
    let im = image_arg(vm, a, 0)?;
    im.filled_ellipse(
        ci(a.int(1)),
        ci(a.int(2)),
        ci(w),
        ci(a.int(4)),
        ci(a.int(5)),
    );
    done(vm)
}

/// The angle normalisation of `imagearc()` / `imagefilledarc()`.
fn angle(v: i64) -> i32 {
    let a = ci(v);
    if a < 0 {
        a % 360
    } else {
        a
    }
}

fn imagefilledarc(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    im.filled_arc(
        ci(a.int(1)),
        ci(a.int(2)),
        ci(a.int(3)),
        ci(a.int(4)),
        angle(a.int(5)),
        angle(a.int(6)),
        ci(a.int(7)),
        ci(a.int(8)),
    );
    done(vm)
}

fn imagealphablending(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    im.alpha_blending(i32::from(a.bool(1)));
    done(vm)
}

fn imagesavealpha(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    im.save_alpha(a.bool(1));
    done(vm)
}

fn imagelayereffect(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    im.alpha_blending(ci(a.int(1)));
    done(vm)
}

/// `CHECK_RGBA_RANGE` for arguments starting at `first` (0-based);
/// `alpha` adds the fourth component.
fn rgba(vm: &mut Vm, a: &Args, first: usize, alpha: bool) -> VmResult<[i32; 4]> {
    let mut out = [0; 4];
    let n = if alpha { 4 } else { 3 };
    for (i, slot) in out.iter_mut().enumerate().take(n) {
        let v = a.int(first + i);
        let max = if i == 3 { 127 } else { 255 };
        if v < 0 || v > max {
            return Err(a.value_error(
                vm,
                first + i + 1,
                &format!("must be between 0 and {max} (inclusive)"),
            ));
        }
        *slot = ci(v);
    }
    Ok(out)
}

fn imagecolorallocatealpha(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let [r, g, b, al] = rgba(vm, a, 1, true)?;
    let c = im.color_allocate_alpha(r, g, b, al);
    flush(vm)?;
    Ok(if c < 0 {
        Value::Bool(false)
    } else {
        Value::Int(i64::from(c))
    })
}

fn imagecolorresolvealpha(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let [r, g, b, al] = rgba(vm, a, 1, true)?;
    Ok(Value::Int(i64::from(im.color_resolve_alpha(r, g, b, al))))
}

fn imagecolorclosestalpha(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let [r, g, b, al] = rgba(vm, a, 1, true)?;
    Ok(Value::Int(i64::from(im.color_closest_alpha(r, g, b, al))))
}

fn imagecolorexactalpha(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let [r, g, b, al] = rgba(vm, a, 1, true)?;
    Ok(Value::Int(i64::from(im.color_exact_alpha(r, g, b, al))))
}

fn imagecolorallocate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let [r, g, b, _] = rgba(vm, a, 1, false)?;
    let c = im.color_allocate(r, g, b);
    flush(vm)?;
    Ok(if c < 0 {
        Value::Bool(false)
    } else {
        Value::Int(i64::from(c))
    })
}

fn imagecolorclosest(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let [r, g, b, _] = rgba(vm, a, 1, false)?;
    Ok(Value::Int(i64::from(im.color_closest(r, g, b))))
}

fn imagecolorclosesthwb(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let [r, g, b, _] = rgba(vm, a, 1, false)?;
    Ok(Value::Int(i64::from(im.color_closest_hwb(r, g, b))))
}

fn imagecolorresolve(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let [r, g, b, _] = rgba(vm, a, 1, false)?;
    Ok(Value::Int(i64::from(im.color_resolve(r, g, b))))
}

fn imagecolorexact(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let [r, g, b, _] = rgba(vm, a, 1, false)?;
    Ok(Value::Int(i64::from(im.color_exact(r, g, b))))
}

fn imagecolordeallocate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    if im.is_truecolor() {
        return Ok(Value::Bool(true));
    }
    let col = ci(a.int(1));
    let total = im.colors_total();
    if col >= 0 && col < total {
        im.color_deallocate(col);
        return done(vm);
    }
    Err(a.value_error(vm, 2, &format!("must be between 0 and {total}")))
}

fn imagecolorset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let [r, g, b, al] = rgba(vm, a, 2, true)?;
    let col = ci(a.int(1));
    if col >= 0 && col < im.colors_total() {
        im.set_palette_entry(col, [r, g, b, al]);
        Ok(Value::Null)
    } else {
        Ok(Value::Bool(false))
    }
}

fn imagecolorsforindex(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let col = ci(a.int(1));
    let ok = col >= 0 && (im.is_truecolor() || col < im.colors_total());
    match im.color_components(col).filter(|_| ok) {
        Some([r, g, b, al]) => Ok(Value::Array(ZArray::from_pairs([
            ("red", Value::Int(i64::from(r))),
            ("green", Value::Int(i64::from(g))),
            ("blue", Value::Int(i64::from(b))),
            ("alpha", Value::Int(i64::from(al))),
        ]))),
        None => Err(a.value_error(vm, 2, "is out of range")),
    }
}

fn imagegammacorrect(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let input = a.float(1);
    let output = a.float(2);
    if input <= 0.0 {
        return Err(a.value_error(vm, 2, "must be greater than 0"));
    }
    if !input.is_finite() {
        return Err(a.value_error(vm, 2, "must be finite"));
    }
    if output <= 0.0 {
        return Err(a.value_error(vm, 3, "must be greater than 0"));
    }
    if !output.is_finite() {
        return Err(a.value_error(vm, 3, "must be finite"));
    }
    let im = image_arg(vm, a, 0)?;
    filters::gamma_correct(&im, input / output);
    done(vm)
}

fn imagepalettecopy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let dst = image_arg(vm, a, 0)?;
    let src = image_arg(vm, a, 1)?;
    Image::palette_copy(&dst, &src);
    flush(vm)?;
    Ok(Value::Null)
}

fn imagecolorat(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let (x, y) = (a.int(1), a.int(2));
    if let Some(c) = im.pixel_at(ci(x), ci(y)) {
        return Ok(Value::Int(i64::from(c)));
    }
    vm.php_notice(&format!("{x},{y} is out of bounds"))?;
    Ok(Value::Bool(false))
}

fn imagesetpixel(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    im.set_pixel(ci(a.int(1)), ci(a.int(2)), ci(a.int(3)));
    done(vm)
}

/// The four coordinates and color of the line/rectangle functions.
fn coords(a: &Args) -> (i32, i32, i32, i32, i32) {
    (
        ci(a.int(1)),
        ci(a.int(2)),
        ci(a.int(3)),
        ci(a.int(4)),
        ci(a.int(5)),
    )
}

/// Antialiasing as `imageline()` / `php_imagepolygon()` do it.
fn aa_color(im: &Image, col: i32) -> i32 {
    if im.antialias() {
        im.set_antialiased_color(col);
        php_gd::ffi::GD_ANTI_ALIASED
    } else {
        col
    }
}

fn imageline(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let (x1, y1, x2, y2, col) = coords(a);
    let col = aa_color(&im, col);
    im.line(x1, y1, x2, y2, col);
    done(vm)
}

fn imagedashedline(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let (x1, y1, x2, y2, col) = coords(a);
    im.dashed_line(x1, y1, x2, y2, col);
    done(vm)
}

fn imagerectangle(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let (x1, y1, x2, y2, col) = coords(a);
    im.rectangle(x1, y1, x2, y2, col);
    done(vm)
}

fn imagefilledrectangle(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let (x1, y1, x2, y2, col) = coords(a);
    im.filled_rectangle(x1, y1, x2, y2, col);
    done(vm)
}

fn imagearc(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    im.arc(
        ci(a.int(1)),
        ci(a.int(2)),
        ci(a.int(3)),
        ci(a.int(4)),
        angle(a.int(5)),
        angle(a.int(6)),
        ci(a.int(7)),
    );
    done(vm)
}

fn imageellipse(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let (cx, cy, w, h, col) = coords(a);
    im.ellipse(cx, cy, w, h, col);
    done(vm)
}

fn imagefilltoborder(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    im.fill_to_border(ci(a.int(1)), ci(a.int(2)), ci(a.int(3)), ci(a.int(4)));
    done(vm)
}

fn imagefill(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    im.fill(ci(a.int(1)), ci(a.int(2)), ci(a.int(3)));
    done(vm)
}

fn imagecolorstotal(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    Ok(Value::Int(i64::from(im.colors_total())))
}

fn imagecolortransparent(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    if a.opt(1).is_some() {
        im.color_transparent(ci(a.int(1)));
        flush(vm)?;
    }
    Ok(Value::Int(i64::from(im.transparent())))
}

fn imageinterlace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    if a.opt(1).is_some() {
        im.set_interlace(a.bool(1));
    }
    Ok(Value::Bool(im.interlaced()))
}

/// `php_imagepolygon()`: `kind` -1 open, 0 normal, 1 filled.
fn polygon(vm: &mut Vm, a: &Args, kind: i32) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let points = a.array(1);
    let (npoints, col) = if a.opt(3).is_none() {
        let n = i64::try_from(points.len()).unwrap_or(i64::MAX);
        if n % 2 != 0 {
            return Err(a.value_error(vm, 2, "must have an even number of elements"));
        }
        (n / 2, a.int(2))
    } else {
        vm.php_error_bytes(
            E_DEPRECATED,
            b"Using the $num_points parameter is deprecated",
        )?;
        (a.int(2), a.int(3))
    };
    let npoints = ci(npoints);
    let col = ci(col);
    let nelem = i32::try_from(points.len()).unwrap_or(i32::MAX);
    if npoints < 3 {
        return Err(a.value_error(vm, 3, "must be greater than or equal to 3"));
    }
    if i64::from(nelem) < i64::from(npoints) * 2 {
        return Err(vm.throw(
            "ValueError",
            &format!(
                "Trying to use {npoints} points in array with only {} points",
                nelem / 2
            ),
        ));
    }
    let mut pts = Vec::with_capacity(usize::try_from(npoints).unwrap_or(0));
    for i in 0..i64::from(npoints) {
        let mut p = Point::default();
        if let Some(v) = points.get_int(i * 2).cloned() {
            p.x = ci(vm.value_to_int(&v)?);
        }
        if let Some(v) = points.get_int(i * 2 + 1).cloned() {
            p.y = ci(vm.value_to_int(&v)?);
        }
        pts.push(p);
    }
    let col = aa_color(&im, col);
    im.polygon(&pts, kind, col);
    done(vm)
}

fn imagepolygon(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    polygon(vm, a, 0)
}

fn imageopenpolygon(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    polygon(vm, a, -1)
}

fn imagefilledpolygon(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    polygon(vm, a, 1)
}

fn imagefontwidth(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let f = font_arg(vm, a, 0)?;
    Ok(Value::Int(i64::from(f.width())))
}

fn imagefontheight(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let f = font_arg(vm, a, 0)?;
    Ok(Value::Int(i64::from(f.height())))
}

/// `php_imagechar()`: 0 char, 1 charup, 2 string, 3 stringup.
fn imagechar_common(vm: &mut Vm, a: &Args, mode: u8) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let f = font_arg(vm, a, 1)?;
    let mut x = ci(a.int(2));
    let mut y = ci(a.int(3));
    let s = a.str(4);
    let col = ci(a.int(5));
    let bytes = s.as_bytes();
    // `strlen()` of the duplicated string.
    let text = bytes.split(|&b| b == 0).next().unwrap_or_default();
    let ch = i32::from(bytes.first().copied().unwrap_or(0));
    match mode {
        0 => im.draw_char(&f, x, y, ch, col),
        1 => font::char_up(&im, &f, x, y, ch, col),
        2 => {
            for &b in text {
                im.draw_char(&f, x, y, i32::from(b), col);
                x = x.wrapping_add(f.width());
            }
        }
        _ => {
            for &b in text {
                im.draw_char_up(&f, x, y, i32::from(b), col);
                y = y.wrapping_sub(f.width());
            }
        }
    }
    done(vm)
}

fn imagechar(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    imagechar_common(vm, a, 0)
}

fn imagecharup(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    imagechar_common(vm, a, 1)
}

fn imagestring(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    imagechar_common(vm, a, 2)
}

fn imagestringup(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    imagechar_common(vm, a, 3)
}

fn imagecopy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let dst = image_arg(vm, a, 0)?;
    let src = image_arg(vm, a, 1)?;
    Image::copy(
        &dst,
        &src,
        ci(a.int(2)),
        ci(a.int(3)),
        ci(a.int(4)),
        ci(a.int(5)),
        ci(a.int(6)),
        ci(a.int(7)),
    );
    done(vm)
}

fn copy_merge(vm: &mut Vm, a: &Args, gray: bool) -> VmResult<Value> {
    let dst = image_arg(vm, a, 0)?;
    let src = image_arg(vm, a, 1)?;
    Image::copy_merge(
        &dst,
        &src,
        gray,
        (ci(a.int(2)), ci(a.int(3))),
        (ci(a.int(4)), ci(a.int(5))),
        ci(a.int(6)),
        ci(a.int(7)),
        ci(a.int(8)),
    );
    done(vm)
}

fn imagecopymerge(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    copy_merge(vm, a, false)
}

fn imagecopymergegray(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    copy_merge(vm, a, true)
}

fn imagecopyresampled(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let dst = image_arg(vm, a, 0)?;
    let src = image_arg(vm, a, 1)?;
    Image::copy_resized(
        &dst,
        &src,
        true,
        (ci(a.int(2)), ci(a.int(3))),
        (ci(a.int(4)), ci(a.int(5))),
        (ci(a.int(6)), ci(a.int(7))),
        (ci(a.int(8)), ci(a.int(9))),
    );
    done(vm)
}

fn imagecopyresized(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let src = image_arg(vm, a, 1)?;
    let dst = image_arg(vm, a, 0)?;
    let (dw, dh, sw, sh) = (ci(a.int(6)), ci(a.int(7)), ci(a.int(8)), ci(a.int(9)));
    for (i, v) in [(7, dw), (8, dh), (9, sw), (10, sh)] {
        if v <= 0 {
            return Err(a.value_error(vm, i, "must be greater than 0"));
        }
    }
    Image::copy_resized(
        &dst,
        &src,
        false,
        (ci(a.int(2)), ci(a.int(3))),
        (ci(a.int(4)), ci(a.int(5))),
        (dw, dh),
        (sw, sh),
    );
    done(vm)
}

fn imagesx(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    Ok(Value::Int(i64::from(im.width())))
}

fn imagesy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    Ok(Value::Int(i64::from(im.height())))
}

fn imagesettile(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let tile = image_arg(vm, a, 1)?;
    im.set_tile(&tile);
    done(vm)
}

fn imagesetbrush(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let brush = image_arg(vm, a, 1)?;
    im.set_brush(&brush);
    done(vm)
}

/// Registers the functions of this module.
#[allow(clippy::too_many_lines)] // one row per function
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let fns: &[(&str, php_vm::ext::NativeFn)] = &[
        ("imagesetstyle(GdImage $image, array $style): bool", imagesetstyle),
        ("imageistruecolor(GdImage $image): bool", imageistruecolor),
        (
            "imagetruecolortopalette(GdImage $image, bool $dither, int $num_colors): bool",
            imagetruecolortopalette,
        ),
        ("imagepalettetotruecolor(GdImage $image): bool", imagepalettetotruecolor),
        ("imagecolormatch(GdImage $image1, GdImage $image2): true", imagecolormatch),
        ("imagesetthickness(GdImage $image, int $thickness): true", imagesetthickness),
        (
            "imagefilledellipse(GdImage $image, int $center_x, int $center_y, int $width, int $height, int $color): true",
            imagefilledellipse,
        ),
        (
            "imagefilledarc(GdImage $image, int $center_x, int $center_y, int $width, int $height, int $start_angle, int $end_angle, int $color, int $style): true",
            imagefilledarc,
        ),
        ("imagealphablending(GdImage $image, bool $enable): true", imagealphablending),
        ("imagesavealpha(GdImage $image, bool $enable): true", imagesavealpha),
        ("imagelayereffect(GdImage $image, int $effect): true", imagelayereffect),
        (
            "imagecolorallocatealpha(GdImage $image, int $red, int $green, int $blue, int $alpha): int|false",
            imagecolorallocatealpha,
        ),
        (
            "imagecolorresolvealpha(GdImage $image, int $red, int $green, int $blue, int $alpha): int",
            imagecolorresolvealpha,
        ),
        (
            "imagecolorclosestalpha(GdImage $image, int $red, int $green, int $blue, int $alpha): int",
            imagecolorclosestalpha,
        ),
        (
            "imagecolorexactalpha(GdImage $image, int $red, int $green, int $blue, int $alpha): int",
            imagecolorexactalpha,
        ),
        (
            "imagecopyresampled(GdImage $dst_image, GdImage $src_image, int $dst_x, int $dst_y, int $src_x, int $src_y, int $dst_width, int $dst_height, int $src_width, int $src_height): true",
            imagecopyresampled,
        ),
        ("imagesettile(GdImage $image, GdImage $tile): true", imagesettile),
        ("imagesetbrush(GdImage $image, GdImage $brush): true", imagesetbrush),
        (
            "imagecolorallocate(GdImage $image, int $red, int $green, int $blue): int|false",
            imagecolorallocate,
        ),
        ("imagepalettecopy(GdImage $dst, GdImage $src): void", imagepalettecopy),
        ("imagecolorat(GdImage $image, int $x, int $y): int|false", imagecolorat),
        (
            "imagecolorclosest(GdImage $image, int $red, int $green, int $blue): int",
            imagecolorclosest,
        ),
        (
            "imagecolorclosesthwb(GdImage $image, int $red, int $green, int $blue): int",
            imagecolorclosesthwb,
        ),
        ("imagecolordeallocate(GdImage $image, int $color): true", imagecolordeallocate),
        (
            "imagecolorresolve(GdImage $image, int $red, int $green, int $blue): int",
            imagecolorresolve,
        ),
        (
            "imagecolorexact(GdImage $image, int $red, int $green, int $blue): int",
            imagecolorexact,
        ),
        (
            "imagecolorset(GdImage $image, int $color, int $red, int $green, int $blue, int $alpha = 0): false|null",
            imagecolorset,
        ),
        ("imagecolorsforindex(GdImage $image, int $color): array", imagecolorsforindex),
        (
            "imagegammacorrect(GdImage $image, float $input_gamma, float $output_gamma): true",
            imagegammacorrect,
        ),
        ("imagesetpixel(GdImage $image, int $x, int $y, int $color): true", imagesetpixel),
        (
            "imageline(GdImage $image, int $x1, int $y1, int $x2, int $y2, int $color): true",
            imageline,
        ),
        (
            "imagedashedline(GdImage $image, int $x1, int $y1, int $x2, int $y2, int $color): true",
            imagedashedline,
        ),
        (
            "imagerectangle(GdImage $image, int $x1, int $y1, int $x2, int $y2, int $color): true",
            imagerectangle,
        ),
        (
            "imagefilledrectangle(GdImage $image, int $x1, int $y1, int $x2, int $y2, int $color): true",
            imagefilledrectangle,
        ),
        (
            "imagearc(GdImage $image, int $center_x, int $center_y, int $width, int $height, int $start_angle, int $end_angle, int $color): true",
            imagearc,
        ),
        (
            "imageellipse(GdImage $image, int $center_x, int $center_y, int $width, int $height, int $color): true",
            imageellipse,
        ),
        (
            "imagefilltoborder(GdImage $image, int $x, int $y, int $border_color, int $color): true",
            imagefilltoborder,
        ),
        ("imagefill(GdImage $image, int $x, int $y, int $color): true", imagefill),
        ("imagecolorstotal(GdImage $image): int", imagecolorstotal),
        (
            "imagecolortransparent(GdImage $image, ?int $color = null): int",
            imagecolortransparent,
        ),
        ("imageinterlace(GdImage $image, ?bool $enable = null): bool", imageinterlace),
        (
            "imagepolygon(GdImage $image, array $points, int $num_points_or_color, ?int $color = null): bool",
            imagepolygon,
        ),
        (
            "imageopenpolygon(GdImage $image, array $points, int $num_points_or_color, ?int $color = null): bool",
            imageopenpolygon,
        ),
        (
            "imagefilledpolygon(GdImage $image, array $points, int $num_points_or_color, ?int $color = null): bool",
            imagefilledpolygon,
        ),
        ("imagefontwidth(GdFont|int $font): int", imagefontwidth),
        ("imagefontheight(GdFont|int $font): int", imagefontheight),
        (
            "imagechar(GdImage $image, GdFont|int $font, int $x, int $y, string $char, int $color): true",
            imagechar,
        ),
        (
            "imagecharup(GdImage $image, GdFont|int $font, int $x, int $y, string $char, int $color): true",
            imagecharup,
        ),
        (
            "imagestring(GdImage $image, GdFont|int $font, int $x, int $y, string $string, int $color): true",
            imagestring,
        ),
        (
            "imagestringup(GdImage $image, GdFont|int $font, int $x, int $y, string $string, int $color): true",
            imagestringup,
        ),
        (
            "imagecopy(GdImage $dst_image, GdImage $src_image, int $dst_x, int $dst_y, int $src_x, int $src_y, int $src_width, int $src_height): true",
            imagecopy,
        ),
        (
            "imagecopymerge(GdImage $dst_image, GdImage $src_image, int $dst_x, int $dst_y, int $src_x, int $src_y, int $src_width, int $src_height, int $pct): true",
            imagecopymerge,
        ),
        (
            "imagecopymergegray(GdImage $dst_image, GdImage $src_image, int $dst_x, int $dst_y, int $src_x, int $src_y, int $src_width, int $src_height, int $pct): true",
            imagecopymergegray,
        ),
        (
            "imagecopyresized(GdImage $dst_image, GdImage $src_image, int $dst_x, int $dst_y, int $src_x, int $src_y, int $dst_width, int $dst_height, int $src_width, int $src_height): true",
            imagecopyresized,
        ),
        ("imagesx(GdImage $image): int", imagesx),
        ("imagesy(GdImage $image): int", imagesy),
    ];
    for &(sig, f) in fns {
        m.function(sig, f);
    }
}
