//! Image operations gd.c implements itself on top of libgd.

use crate::ffi::Image;

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
    fn foreground() {
        let im = Image::new_palette(1, 1).expect("image");
        assert_eq!(first_black(&im), 0);
        im.color_allocate(255, 255, 255);
        assert_eq!(first_black(&im), 1);
        im.color_allocate(0, 0, 0);
        assert_eq!(first_black(&im), 1);
    }
}
