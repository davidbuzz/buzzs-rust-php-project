//! `gd_info()`, `imagetypes()`, the `phpinfo()` rows and the constants of
//! gd.stub.php, for a build against the system libgd 2.3.3 with the
//! features the Ubuntu package provides (`FreeType`, GIF, JPEG, PNG, WBMP,
//! XPM, XBM, WebP, BMP, TGA; no AVIF).

/// `PHP_GD_VERSION_STRING` (`GD_VERSION_STRING` of the system headers).
pub const VERSION_STRING: &str = "2.3.3";

/// A `gd_info()` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfoValue {
    /// A string entry.
    Str(&'static str),
    /// A boolean entry.
    Bool(bool),
}

/// `gd_info()`, in order.
pub fn gd_info() -> Vec<(&'static str, InfoValue)> {
    use InfoValue::{Bool, Str};
    vec![
        ("GD Version", Str(VERSION_STRING)),
        ("FreeType Support", Bool(true)),
        ("FreeType Linkage", Str("with freetype")),
        ("GIF Read Support", Bool(true)),
        ("GIF Create Support", Bool(true)),
        ("JPEG Support", Bool(true)),
        ("PNG Support", Bool(true)),
        ("WBMP Support", Bool(true)),
        ("XPM Support", Bool(true)),
        ("XBM Support", Bool(true)),
        ("WebP Support", Bool(true)),
        ("BMP Support", Bool(true)),
        ("AVIF Support", Bool(false)),
        ("TGA Read Support", Bool(true)),
        ("JIS-mapped Japanese Font Support", Bool(false)),
    ]
}

/// The `phpinfo()` table rows of `PHP_MINFO_FUNCTION(gd)`.
pub fn minfo_rows() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GD Support", "enabled"),
        ("GD headers Version", VERSION_STRING),
        ("GD library Version", VERSION_STRING),
        ("FreeType Support", "enabled"),
        ("FreeType Linkage", "with freetype"),
        ("GIF Read Support", "enabled"),
        ("GIF Create Support", "enabled"),
        ("JPEG Support", "enabled"),
        ("PNG Support", "enabled"),
        ("WBMP Support", "enabled"),
        ("XPM Support", "enabled"),
        ("XBM Support", "enabled"),
        ("WebP Support", "enabled"),
        ("BMP Support", "enabled"),
        ("TGA Read Support", "enabled"),
    ]
}

/// `PHP_IMG_*` bits.
pub mod img {
    /// `IMG_GIF`.
    pub const GIF: i64 = 1;
    /// `IMG_JPG`.
    pub const JPG: i64 = 2;
    /// `IMG_PNG`.
    pub const PNG: i64 = 4;
    /// `IMG_WBMP`.
    pub const WBMP: i64 = 8;
    /// `IMG_XPM`.
    pub const XPM: i64 = 16;
    /// `IMG_WEBP`.
    pub const WEBP: i64 = 32;
    /// `IMG_BMP`.
    pub const BMP: i64 = 64;
    /// `IMG_TGA`.
    pub const TGA: i64 = 128;
    /// `IMG_AVIF`.
    pub const AVIF: i64 = 256;
}

/// `imagetypes()`.
pub fn image_types() -> i64 {
    img::GIF | img::JPG | img::PNG | img::WBMP | img::XPM | img::WEBP | img::BMP | img::TGA
}

/// A constant value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstValue {
    /// `int`.
    Int(i64),
    /// `string`.
    Str(&'static str),
}

/// The constants of gd.stub.php, in declaration order.
pub fn constants() -> Vec<(&'static str, ConstValue)> {
    use ConstValue::{Int, Str};
    let ints: &[(&str, i64)] = &[
        ("IMG_AVIF", img::AVIF),
        ("IMG_GIF", img::GIF),
        ("IMG_JPG", img::JPG),
        ("IMG_JPEG", img::JPG),
        ("IMG_PNG", img::PNG),
        ("IMG_WBMP", img::WBMP),
        ("IMG_XPM", img::XPM),
        ("IMG_WEBP", img::WEBP),
        ("IMG_BMP", img::BMP),
        ("IMG_TGA", img::TGA),
        ("IMG_WEBP_LOSSLESS", 101),
        ("IMG_COLOR_TILED", -5),
        ("IMG_COLOR_STYLED", -2),
        ("IMG_COLOR_BRUSHED", -3),
        ("IMG_COLOR_STYLEDBRUSHED", -4),
        ("IMG_COLOR_TRANSPARENT", -6),
        ("IMG_ARC_ROUNDED", 0),
        ("IMG_ARC_PIE", 0),
        ("IMG_ARC_CHORD", 1),
        ("IMG_ARC_NOFILL", 2),
        ("IMG_ARC_EDGED", 4),
        ("IMG_GD2_RAW", 1),
        ("IMG_GD2_COMPRESSED", 2),
        ("IMG_FLIP_HORIZONTAL", 1),
        ("IMG_FLIP_VERTICAL", 2),
        ("IMG_FLIP_BOTH", 3),
        ("IMG_EFFECT_REPLACE", 0),
        ("IMG_EFFECT_ALPHABLEND", 1),
        ("IMG_EFFECT_NORMAL", 2),
        ("IMG_EFFECT_OVERLAY", 3),
        ("IMG_EFFECT_MULTIPLY", 4),
        ("IMG_CROP_DEFAULT", 0),
        ("IMG_CROP_TRANSPARENT", 1),
        ("IMG_CROP_BLACK", 2),
        ("IMG_CROP_WHITE", 3),
        ("IMG_CROP_SIDES", 4),
        ("IMG_CROP_THRESHOLD", 5),
        ("IMG_BELL", 1),
        ("IMG_BESSEL", 2),
        ("IMG_BILINEAR_FIXED", 3),
        ("IMG_BICUBIC", 4),
        ("IMG_BICUBIC_FIXED", 5),
        ("IMG_BLACKMAN", 6),
        ("IMG_BOX", 7),
        ("IMG_BSPLINE", 8),
        ("IMG_CATMULLROM", 9),
        ("IMG_GAUSSIAN", 10),
        ("IMG_GENERALIZED_CUBIC", 11),
        ("IMG_HERMITE", 12),
        ("IMG_HAMMING", 13),
        ("IMG_HANNING", 14),
        ("IMG_MITCHELL", 15),
        ("IMG_POWER", 17),
        ("IMG_QUADRATIC", 18),
        ("IMG_SINC", 19),
        ("IMG_NEAREST_NEIGHBOUR", 16),
        ("IMG_WEIGHTED4", 21),
        ("IMG_TRIANGLE", 20),
        ("IMG_AFFINE_TRANSLATE", 0),
        ("IMG_AFFINE_SCALE", 1),
        ("IMG_AFFINE_ROTATE", 2),
        ("IMG_AFFINE_SHEAR_HORIZONTAL", 3),
        ("IMG_AFFINE_SHEAR_VERTICAL", 4),
        ("GD_BUNDLED", 0),
        ("IMG_FILTER_NEGATE", 0),
        ("IMG_FILTER_GRAYSCALE", 1),
        ("IMG_FILTER_BRIGHTNESS", 2),
        ("IMG_FILTER_CONTRAST", 3),
        ("IMG_FILTER_COLORIZE", 4),
        ("IMG_FILTER_EDGEDETECT", 5),
        ("IMG_FILTER_GAUSSIAN_BLUR", 7),
        ("IMG_FILTER_SELECTIVE_BLUR", 8),
        ("IMG_FILTER_EMBOSS", 6),
        ("IMG_FILTER_MEAN_REMOVAL", 9),
        ("IMG_FILTER_SMOOTH", 10),
        ("IMG_FILTER_PIXELATE", 11),
        ("IMG_FILTER_SCATTER", 12),
    ];
    let mut out: Vec<(&'static str, ConstValue)> = ints.iter().map(|&(n, v)| (n, Int(v))).collect();
    out.push(("GD_VERSION", Str(VERSION_STRING)));
    out.push(("GD_MAJOR_VERSION", Int(2)));
    out.push(("GD_MINOR_VERSION", Int(3)));
    out.push(("GD_RELEASE_VERSION", Int(3)));
    out.push(("GD_EXTRA_VERSION", Str("")));
    for (n, v) in [
        ("PNG_NO_FILTER", 0),
        ("PNG_FILTER_NONE", 0x08),
        ("PNG_FILTER_SUB", 0x10),
        ("PNG_FILTER_UP", 0x20),
        ("PNG_FILTER_AVG", 0x40),
        ("PNG_FILTER_PAETH", 0x80),
        ("PNG_ALL_FILTERS", 0xf8),
    ] {
        out.push((n, Int(v)));
    }
    out
}

/// `IMAGE_FILTER_MAX`.
pub const IMAGE_FILTER_MAX: i64 = 12;
/// `GD_METHOD_COUNT`.
pub const GD_METHOD_COUNT: i64 = 30;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables() {
        assert_eq!(image_types(), 255);
        assert_eq!(gd_info().len(), 15);
        let c = constants();
        assert_eq!(c.len(), 89);
        assert!(c.contains(&("PNG_ALL_FILTERS", ConstValue::Int(248))));
        assert!(c.contains(&("GD_VERSION", ConstValue::Str("2.3.3"))));
    }
}
