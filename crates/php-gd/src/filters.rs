// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Image operations gd.c implements itself on top of libgd.

use crate::ffi::{Image, Rect};

/// `gdTrueColorAlpha(r, g, b, a)`.
fn truecolor_alpha(r: i32, g: i32, b: i32, a: i32) -> i32 {
    a.wrapping_shl(24)
        .wrapping_add(r.wrapping_shl(16))
        .wrapping_add(g.wrapping_shl(8))
        .wrapping_add(b)
}

/// `(int) ((pow(c / 255.0, gamma) * 255) + .5)`.
#[allow(clippy::cast_possible_truncation)]
fn gamma_component(c: i32, gamma: f64) -> i32 {
    ((f64::from(c) / 255.0).powf(gamma) * 255.0 + 0.5) as i32
}

/// `imagegammacorrect()` with `gamma = input / output`.
pub fn gamma_correct(im: &Image, gamma: f64) {
    if im.is_truecolor() {
        for y in 0..im.height() {
            for x in 0..im.width() {
                let c = im.get_pixel(x, y);
                im.set_pixel(
                    x,
                    y,
                    truecolor_alpha(
                        gamma_component((c >> 16) & 0xff, gamma),
                        gamma_component((c >> 8) & 0xff, gamma),
                        gamma_component(c & 0xff, gamma),
                        (c >> 24) & 0x7f,
                    ),
                );
            }
        }
        return;
    }
    for i in 0..im.colors_total() {
        if let Some([r, g, b, a]) = im.color_components(i) {
            im.set_palette_entry(
                i,
                [
                    gamma_component(r, gamma),
                    gamma_component(g, gamma),
                    gamma_component(b, gamma),
                    a,
                ],
            );
        }
    }
}

/// The default foreground of `imagexbm()`/`imagewbmp()`: the first palette
/// entry that is black (`gdImageColorsTotal()` when none is).
pub fn first_black(im: &Image) -> i32 {
    let total = im.colors_total();
    (0..total)
        .find(|&i| {
            im.color_components(i)
                .is_some_and(|[r, g, b, _]| r == 0 && g == 0 && b == 0)
        })
        .unwrap_or(total)
}

/// `gdColorMatch()` of php-src's bundled libgd (`gd_color.c`): whether
/// the RGBA distance between two colors of `im` is within `threshold`
/// percent of the maximum distance.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn color_match(im: &Image, col1: i32, col2: i32, threshold: f32) -> bool {
    let (Some(a), Some(b)) = (im.color_components(col1), im.color_components(col2)) else {
        return false;
    };
    let dr = a[0] - b[0];
    let dg = a[1] - b[1];
    let db = a[2] - b[2];
    let da = (a[3] - b[3]) * 2;
    let dist = dr * dr + dg * dg + db * db + da * da;
    let t = threshold / 100.0;
    dist <= (t * t * 260_100.0) as i32
}

/// `gdImageCropThreshold()` of php-src's bundled libgd (`gd_crop.c`),
/// which `imagecropauto(..., IMG_CROP_THRESHOLD, ...)` relies on (the
/// system libgd 2.3.3 still has the pre-#73291 threshold semantics).
#[allow(clippy::cast_possible_wrap)]
pub fn crop_threshold(im: &Image, color: u32, threshold: f32) -> Option<Image> {
    let (width, height) = (im.width(), im.height());
    if threshold > 100.0 {
        return None;
    }
    if !im.is_truecolor() && i64::from(color) >= i64::from(im.colors_total()) {
        return None;
    }
    let color = color as i32;
    let differs = |x: i32, y: i32| !color_match(im, color, im.get_pixel(x, y), threshold);

    let (mut x, mut y) = (0, 0);
    'top: while y < height {
        x = 0;
        while x < width {
            if differs(x, y) {
                break 'top;
            }
            x += 1;
        }
        y += 1;
    }
    if y == height && x == width {
        return None;
    }
    let top = y;

    y = height - 1;
    'bottom: while y >= 0 {
        for x in 0..width {
            if differs(x, y) {
                break 'bottom;
            }
        }
        y -= 1;
    }
    let h = y - top + 1;

    x = 0;
    'left: while x < width {
        for y in top..top + h {
            if differs(x, y) {
                break 'left;
            }
        }
        x += 1;
    }
    let left = x;

    x = width - 1;
    'right: while x >= 0 {
        for y in top..top + h {
            if differs(x, y) {
                break 'right;
            }
        }
        x -= 1;
    }
    let w = x - left + 1;

    im.crop(Rect {
        x: left,
        y: top,
        width: w,
        height: h,
    })
}

#[allow(clippy::many_single_char_names)] // gd's names
/// `gdTransformAffineBoundingBox()` of php-src's bundled libgd (the
/// system libgd 2.3.3 truncates the width by one pixel).
#[allow(clippy::cast_possible_truncation)]
pub fn affine_bounding_box(src: Rect, m: &[f64; 6]) -> Option<Rect> {
    let (w, h) = (f64::from(src.width), f64::from(src.height));
    let extent = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)]
        .map(|(x, y)| (x * m[0] + y * m[2] + m[4], x * m[1] + y * m[3] + m[5]));
    let (mut min, mut max) = (extent[0], extent[0]);
    for &(x, y) in &extent[1..] {
        if min.0 > x {
            min.0 = x;
        }
        if min.1 > y {
            min.1 = y;
        }
        if max.0 < x {
            max.0 = x;
        }
        if max.1 < y {
            max.1 = y;
        }
    }
    let bx = min.0.floor();
    let by = min.1.floor();
    let bw = max.0.ceil() - bx;
    let bh = max.1.ceil() - by;
    let int_max = f64::from(i32::MAX);
    let int_min = f64::from(i32::MIN);
    if !bx.is_finite()
        || !by.is_finite()
        || !bw.is_finite()
        || !bh.is_finite()
        || bx < int_min
        || bx > int_max
        || by < int_min
        || by > int_max
        || bw < 0.0
        || bw > int_max
        || bh < 0.0
        || bh > int_max
    {
        return None;
    }
    let (x, y, width, height) = (bx as i32, by as i32, bw as i32, bh as i32);
    if (x < 0 && width > i32::MAX + x)
        || (x > 0 && width > i32::MAX - x)
        || (y < 0 && height > i32::MAX + y)
        || (y > 0 && height > i32::MAX - y)
    {
        return None;
    }
    Some(Rect {
        x,
        y,
        width,
        height,
    })
}

/// `GD_NEAREST_NEIGHBOUR`.
const NEAREST_NEIGHBOUR: i32 = 16;

/// `imagerotate()`: `gdImageRotateInterpolated()`, with php-src's bundled
/// nearest-neighbour rotation (its bounding box and rounding differ from
/// the system libgd 2.3.3, whose version also crashes on palette images).
#[allow(clippy::cast_possible_truncation)]
pub fn rotate(im: &Image, degrees: f32, bg: i32) -> Option<Image> {
    if bg < 0 {
        return None;
    }
    let rounded = ((degrees * 100.0).floor() as i32) % 36000;
    let square = matches!(rounded, 0 | 9000 | -27000 | 18000 | -18000 | -9000 | 27000);
    if square || im.interpolation_id() != NEAREST_NEIGHBOUR {
        return im.rotate(degrees, bg);
    }
    if im.is_truecolor() {
        return rotate_nearest(im, degrees, bg);
    }
    let mut bg = bg;
    if let Some([r, g, b, a]) = im.color_components(bg) {
        bg = truecolor_alpha(r, g, b, a);
    }
    let tc = im.try_clone()?;
    tc.palette_to_truecolor();
    rotate_nearest(&tc, degrees, bg)
}

/// `gdImageRotateNearestNeighbour()` of php-src's bundled libgd.
#[allow(clippy::cast_possible_truncation)]
fn rotate_nearest(src: &Image, degrees: f32, bg: i32) -> Option<Image> {
    let angle = (-f64::from(degrees) / 180.0) * std::f64::consts::PI;
    let (src_w, src_h) = (src.width(), src.height());
    let hh = f64::from(src_h / 2);
    let ww = f64::from(src_w / 2);
    let (cos_a, sin_a) = ((-angle).cos(), (-angle).sin());
    let m = crate::ffi::affine::rotate(f64::from(degrees))?;
    let bbox = affine_bounding_box(
        Rect {
            x: 0,
            y: 0,
            width: src_w,
            height: src_h,
        },
        &m,
    )?;
    let (nw, nh) = (bbox.width, bbox.height);
    let dst = Image::new_truecolor(nw, nh)?;
    dst.save_alpha(true);
    // Raw `tpixels` stores.
    dst.alpha_blending(0);
    for i in 0..nh {
        let dst_y = f64::from(i - nh / 2);
        for j in 0..nw {
            let dst_x = f64::from(j - nw / 2);
            let src_y = dst_x * sin_a + dst_y * cos_a + hh;
            let src_x = dst_x * cos_a - dst_y * sin_a + ww;
            let my = (src_y + 0.5).floor() as i64;
            let nx = (src_x + 0.5).floor() as i64;
            let c = if my > 0 && my < i64::from(src_h) - 1 && nx > 0 && nx < i64::from(src_w) - 1 {
                src.get_pixel(nx as i32, my as i32)
            } else {
                bg
            };
            dst.set_pixel(j, i, c);
        }
    }
    dst.alpha_blending(1);
    Some(dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamma_palette() {
        let im = Image::new_palette(2, 2).expect("image");
        im.color_allocate(128, 0, 255);
        gamma_correct(&im, 2.0);
        assert_eq!(im.color_components(0), Some([64, 0, 255, 0]));
    }

    #[test]
    fn gamma_truecolor() {
        let im = Image::new_truecolor(1, 1).expect("image");
        im.alpha_blending(0);
        im.set_pixel(0, 0, 0x0080_80ff);
        gamma_correct(&im, 0.5);
        assert_eq!(im.get_pixel(0, 0), (181 << 16) | (181 << 8) | 255);
    }

    #[test]
    fn crop_threshold_keeps_the_differing_area() {
        let im = Image::new_truecolor(10, 8).expect("image");
        im.alpha_blending(0);
        im.filled_rectangle(0, 0, 9, 7, 0x00ff_ffff);
        im.filled_rectangle(3, 2, 5, 4, 0x0000_0000);
        im.set_pixel(7, 6, 0x00f0_f0f0);
        let c = crop_threshold(&im, 0x00ff_ffff, 1.0).expect("crop");
        assert_eq!((c.width(), c.height()), (5, 5));
        let c = crop_threshold(&im, 0x00ff_ffff, 10.0).expect("crop");
        assert_eq!((c.width(), c.height()), (3, 3));
        assert!(crop_threshold(&im, 0x00ff_ffff, 101.0).is_none());
        let blank = Image::new_truecolor(4, 4).expect("image");
        assert!(crop_threshold(&blank, 0, 0.5).is_none());
    }

    #[test]
    fn rotated_bounding_box() {
        let m = crate::ffi::affine::rotate(60.0).expect("matrix");
        let r = affine_bounding_box(
            Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
            &m,
        )
        .expect("bbox");
        assert_eq!((r.width, r.height), (138, 137));
    }

    #[test]
    fn nearest_neighbour_rotation() {
        let im = Image::new_palette(40, 20).expect("image");
        let bg = im.color_allocate_alpha(0, 0, 0, 127);
        assert!(im.set_interpolation(NEAREST_NEIGHBOUR));
        let r = rotate(&im, -5.0, bg).expect("rotate");
        assert_eq!((r.width(), r.height()), (42, 24));
        assert!(r.is_truecolor());
        assert_eq!(r.get_pixel(0, 0), 127 << 24);
        assert!(rotate(&im, 10.0, -1).is_none());
    }

    #[test]
    fn foreground() {
        let im = Image::new_palette(1, 1).expect("image");
        assert_eq!(first_black(&im), 0);
        im.color_allocate(255, 255, 255);
        assert_eq!(first_black(&im), 1);
        im.color_allocate(0, 0, 0);
        assert_eq!(first_black(&im), 1);
    }
}
