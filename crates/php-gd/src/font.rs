//! `imageloadfont()`'s font file format and the bitmap text helpers of
//! gd.c (`php_find_gd_font()`, `php_gdimagecharup()`, `overflow2()`).

use crate::ffi::errors::{self, Level};
use crate::ffi::{Font, Image};

/// Size of the header: `sizeof(gdFont) - sizeof(char *)` (four C ints).
pub const HEADER_SIZE: usize = 16;

/// `overflow2()` of `gd_compat.c`: whether `a * b` would overflow an `int`
/// (or either is not positive); raises its warning.
pub fn overflow2(a: i32, b: i32) -> bool {
    if a <= 0 || b <= 0 {
        errors::push(
            Level::Warning,
            b"One parameter to a memory allocation multiplication is negative or zero, failing operation gracefully\n".to_vec(),
        );
        return true;
    }
    if a > i32::MAX / b {
        errors::push(
            Level::Warning,
            b"Product of memory allocation multiplication would exceed INT_MAX, failing operation gracefully\n".to_vec(),
        );
        return true;
    }
    false
}

/// A validated font header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontHeader {
    /// Number of glyphs.
    pub nchars: i32,
    /// First character.
    pub offset: i32,
    /// Glyph width.
    pub w: i32,
    /// Glyph height.
    pub h: i32,
}

impl FontHeader {
    /// The body size in bytes.
    pub fn body_size(&self) -> usize {
        usize::try_from(i64::from(self.w) * i64::from(self.h) * i64::from(self.nchars)).unwrap_or(0)
    }
}

/// Why a header was rejected (the warning texts of `imageloadfont()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderError {
    /// `Error reading font, invalid font header`.
    Invalid,
    /// `Error reading font`.
    SizeMismatch,
}

impl HeaderError {
    /// The warning text.
    pub fn message(self) -> &'static str {
        match self {
            HeaderError::Invalid => "Error reading font, invalid font header",
            HeaderError::SizeMismatch => "Error reading font",
        }
    }
}

/// `FLIPWORD`.
fn flip(v: i32) -> i32 {
    i32::from_be_bytes(v.to_le_bytes())
}

/// Validates the native-endian header against the body size found in the
/// file (`file size - 16`), byte-swapping the fields if needed, like
/// `imageloadfont()`.
pub fn parse_header(
    hdr: &[u8; HEADER_SIZE],
    body_size_check: i64,
) -> Result<FontHeader, HeaderError> {
    let int = |i: usize| i32::from_ne_bytes([hdr[i], hdr[i + 1], hdr[i + 2], hdr[i + 3]]);
    let mut f = FontHeader {
        nchars: int(0),
        offset: int(4),
        w: int(8),
        h: int(12),
    };
    let check =
        |f: &FontHeader| overflow2(f.nchars, f.h) || overflow2(f.nchars.wrapping_mul(f.h), f.w);
    if check(&f) {
        return Err(HeaderError::Invalid);
    }
    let size = |f: &FontHeader| i64::from(f.w.wrapping_mul(f.h).wrapping_mul(f.nchars));
    if size(&f) != body_size_check {
        f.w = flip(f.w);
        f.h = flip(f.h);
        f.nchars = flip(f.nchars);
        if check(&f) {
            return Err(HeaderError::Invalid);
        }
    }
    if size(&f) != body_size_check {
        return Err(HeaderError::SizeMismatch);
    }
    Ok(f)
}

/// Builds the loaded font.
pub fn font_from(h: FontHeader, body: Vec<u8>) -> Font {
    Font::from_parts(h.nchars, h.offset, h.w, h.h, body)
}

#[allow(clippy::many_single_char_names)] // gd.c's names
/// `php_gdimagecharup()` (gd.c's workaround for a bug in gd 1.2), used by
/// `imagecharup()`.
pub fn char_up(im: &Image, f: &Font, x: i32, y: i32, c: i32, color: i32) {
    let (w, h) = (f.width(), f.height());
    if c < f.offset() || c >= f.offset().saturating_add(f.nchars()) {
        return;
    }
    let data = f.data();
    let Ok(fline) = usize::try_from(i64::from(c - f.offset()) * i64::from(h) * i64::from(w)) else {
        return;
    };
    let wu = usize::try_from(w).unwrap_or(0);
    let mut cx = 0usize;
    let mut py = y;
    while py > y.saturating_sub(w) {
        let mut cy = 0usize;
        let mut px = x;
        while px < x.saturating_add(h) {
            if data.get(fline + cy * wu + cx).is_some_and(|&b| b != 0) {
                im.set_pixel(px, py, color);
            }
            cy += 1;
            px += 1;
        }
        cx += 1;
        py -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(v: [i32; 4]) -> [u8; 16] {
        let mut out = [0u8; 16];
        for (i, x) in v.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&x.to_ne_bytes());
        }
        out
    }

    #[test]
    fn header_native_and_swapped() {
        let h = parse_header(&hdr([2, 32, 3, 4]), 24).expect("valid");
        assert_eq!((h.nchars, h.offset, h.w, h.h), (2, 32, 3, 4));
        assert_eq!(h.body_size(), 24);
        // Swapped small values overflow before the swap is tried (as in C).
        assert_eq!(
            parse_header(&hdr([flip(2), 32, flip(3), flip(4)]), 24),
            Err(HeaderError::Invalid)
        );
        assert_eq!(
            parse_header(&hdr([2, 32, 3, 4]), 25),
            Err(HeaderError::Invalid)
        );
    }

    #[test]
    fn header_overflow_warns() {
        let _ = errors::take();
        assert_eq!(
            parse_header(&hdr([0x4141_4141, 0, 1, 1]), 0),
            Err(HeaderError::Invalid)
        );
        let m = errors::take();
        assert_eq!(m.len(), 1);
        assert!(m[0].text.starts_with(b"Product of memory allocation"));
    }

    #[test]
    fn builtin_fonts() {
        assert_eq!(Font::builtin(1).width(), 5);
        assert_eq!(Font::builtin(-3).height(), 8);
        assert_eq!(Font::builtin(5).width(), 9);
        assert_eq!(Font::builtin(99).height(), 15);
        let f = Font::builtin(3);
        assert_eq!(
            f.data().len(),
            usize::try_from(f.nchars() * f.width() * f.height()).unwrap_or(0)
        );
    }

    #[test]
    fn char_up_draws() {
        let im = Image::new_truecolor(20, 20).expect("image");
        let f = Font::from_parts(1, 65, 2, 2, vec![1, 0, 0, 1]);
        char_up(&im, &f, 5, 10, 65, 0x00ff_0000);
        assert_eq!(im.get_pixel(5, 10), 0x00ff_0000);
        assert_eq!(im.get_pixel(6, 9), 0x00ff_0000);
        assert_eq!(im.get_pixel(6, 10), 0);
        char_up(&im, &f, 0, 0, 66, 1);
    }
}
