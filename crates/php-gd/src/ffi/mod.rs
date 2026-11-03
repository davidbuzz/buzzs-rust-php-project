//! The only place with `unsafe`: safe wrappers over the system libgd.
//!
//! [`Image`] owns a `gdImagePtr`. Its methods take `&self` even when they
//! modify the image: the pixels live in C memory that Rust never borrows,
//! the type is neither `Send` nor `Sync`, and operations such as
//! `imagecopy($im, $im, ...)` legitimately pass the same image twice
//! (exactly as the C extension does). Images referenced by libgd from
//! another image (brush, tile) are kept alive through [`Rc`]s.

// The parameter names follow libgd's prototypes.
#![allow(clippy::many_single_char_names, clippy::borrow_as_ptr)]

pub mod errors;
mod io;
mod sys;

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::io::Write;
use std::os::raw::{c_int, c_void};
use std::ptr::NonNull;
use std::rc::Rc;

pub use sys::{gdPoint as Point, gdRect as Rect};

/// `gdAntiAliased`.
pub const GD_ANTI_ALIASED: i32 = -7;
/// `GD_QUANT_JQUANT`.
const GD_QUANT_JQUANT: c_int = 1;

/// A libgd image (`gdImagePtr`).
pub struct Image {
    ptr: NonNull<sys::gdImage>,
    /// Images libgd reads through `im->brush` / `im->tile`.
    brush: RefCell<Option<Rc<Image>>>,
    tile: RefCell<Option<Rc<Image>>>,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Image")
            .field("sx", &self.width())
            .field("sy", &self.height())
            .field("truecolor", &self.is_truecolor())
            .finish_non_exhaustive()
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        // SAFETY: the pointer came from libgd, is owned by this value and
        // is destroyed exactly once.
        unsafe { sys::gdImageDestroy(self.ptr.as_ptr()) };
    }
}

/// A decoder of [`Image::decode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoder {
    /// `gdImageCreateFromPngPtr`.
    Png,
    /// `gdImageCreateFromGifPtr`.
    Gif,
    /// `gdImageCreateFromJpegPtrEx` (`ignore_warning`).
    Jpeg(bool),
    /// `gdImageCreateFromWebpPtr`.
    Webp,
    /// `gdImageCreateFromWBMPPtr`.
    Wbmp,
    /// `gdImageCreateFromGdPtr`.
    Gd,
    /// `gdImageCreateFromGd2Ptr`.
    Gd2,
    /// `gdImageCreateFromGd2PartPtr(x, y, w, h)`.
    Gd2Part(i32, i32, i32, i32),
    /// `gdImageCreateFromBmpPtr`.
    Bmp,
    /// `gdImageCreateFromTgaPtr`.
    Tga,
    /// `gdImageCreateFromXbm` (through `fmemopen`).
    Xbm,
}

/// An encoder of [`Image::encode`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Encoder {
    /// `gdImagePngCtxEx(level)`.
    Png(i32),
    /// `gdImageGifCtx` (palette conversion with `GD_QUANT_JQUANT`).
    Gif,
    /// `gdImageJpegCtx(quality)`.
    Jpeg(i32),
    /// `gdImageWebpCtx(quality)`.
    Webp(i32),
    /// `gdImageWBMPCtx(foreground)`.
    Wbmp(i32),
    /// `gdImageBmpCtx(compression)`.
    Bmp(bool),
    /// `gdImageXbmCtx(file_name, foreground)`.
    Xbm(Vec<u8>, i32),
    /// `gdImageGd`.
    Gd,
    /// `gdImageGd2(chunk_size, fmt)`.
    Gd2(i32, i32),
}

/// Wraps a pointer returned by libgd.
fn wrap(p: *mut sys::gdImage) -> Option<Image> {
    NonNull::new(p).map(|ptr| Image {
        ptr,
        brush: RefCell::new(None),
        tile: RefCell::new(None),
    })
}

/// Copies a malloc'd buffer returned by libgd and frees it.
///
/// # Safety
/// `p` must be null or a `gdMalloc`ed buffer of at least `size` bytes.
unsafe fn take_buffer(p: *mut c_void, size: c_int) -> Option<Vec<u8>> {
    if p.is_null() {
        return None;
    }
    let n = usize::try_from(size).unwrap_or(0);
    // SAFETY: libgd returned `size` initialised bytes at `p`.
    let v = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), n) }.to_vec();
    // SAFETY: `p` was allocated by libgd and is freed once.
    unsafe { sys::gdFree(p) };
    Some(v)
}

impl Image {
    fn raw(&self) -> *mut sys::gdImage {
        self.ptr.as_ptr()
    }

    /// Reads the image header.
    fn hdr(&self) -> &sys::gdImage {
        // SAFETY: the pointer is valid for the life of `self`; no `&mut`
        // to the struct is ever created, and libgd only mutates it during
        // calls made through `&self` methods, which do not overlap with
        // this short-lived shared borrow.
        unsafe { self.ptr.as_ref() }
    }

    /// Writes header fields.
    fn with_hdr_mut<R>(&self, f: impl FnOnce(&mut sys::gdImage) -> R) -> R {
        // SAFETY: as in `hdr`; the exclusive borrow is confined to `f`,
        // which cannot call back into libgd or other `Image` methods
        // (it only receives the struct).
        f(unsafe { &mut *self.raw() })
    }

    /// `gdImageCreate`.
    pub fn new_palette(sx: i32, sy: i32) -> Option<Image> {
        errors::install();
        // SAFETY: plain constructor; libgd validates the sizes.
        wrap(unsafe { sys::gdImageCreate(sx, sy) })
    }

    /// `gdImageCreateTrueColor`.
    pub fn new_truecolor(sx: i32, sy: i32) -> Option<Image> {
        errors::install();
        // SAFETY: plain constructor; libgd validates the sizes.
        wrap(unsafe { sys::gdImageCreateTrueColor(sx, sy) })
    }

    /// Decodes an image held in memory. `None` when libgd fails (its
    /// messages are queued in [`errors`]) or the data exceeds `INT_MAX`.
    pub fn decode(dec: Decoder, data: &[u8]) -> Option<Image> {
        errors::install();
        let size = c_int::try_from(data.len()).ok()?;
        // libgd's dynamic context only reads through the pointer when the
        // free flag is 0 (it never writes or frees it).
        let p = data.as_ptr().cast::<c_void>().cast_mut();
        // SAFETY: `p` points to `size` readable bytes for the whole call.
        let im = unsafe {
            match dec {
                Decoder::Png => sys::gdImageCreateFromPngPtr(size, p),
                Decoder::Gif => sys::gdImageCreateFromGifPtr(size, p),
                Decoder::Jpeg(ignore) => {
                    sys::gdImageCreateFromJpegPtrEx(size, p, c_int::from(ignore))
                }
                Decoder::Webp => sys::gdImageCreateFromWebpPtr(size, p),
                Decoder::Wbmp => sys::gdImageCreateFromWBMPPtr(size, p),
                Decoder::Gd => sys::gdImageCreateFromGdPtr(size, p),
                Decoder::Gd2 => sys::gdImageCreateFromGd2Ptr(size, p),
                Decoder::Gd2Part(x, y, w, h) => {
                    sys::gdImageCreateFromGd2PartPtr(size, p, x, y, w, h)
                }
                Decoder::Bmp => sys::gdImageCreateFromBmpPtr(size, p),
                Decoder::Tga => sys::gdImageCreateFromTgaPtr(size, p),
                Decoder::Xbm => {
                    if data.is_empty() {
                        return None;
                    }
                    let f = libc::fmemopen(p, data.len(), c"rb".as_ptr());
                    if f.is_null() {
                        return None;
                    }
                    let im = sys::gdImageCreateFromXbm(f);
                    libc::fclose(f);
                    im
                }
            }
        };
        wrap(im)
    }

    /// `gdImageCreateFromXpm(filename)`: libXpm reads the file itself.
    pub fn decode_xpm_file(path: &[u8]) -> Option<Image> {
        errors::install();
        let c = CString::new(path).ok()?;
        let raw = c.into_raw();
        // SAFETY: `raw` is a valid NUL-terminated string that libgd only
        // reads; ownership is taken back right after.
        let im = unsafe { sys::gdImageCreateFromXpm(raw) };
        // SAFETY: `raw` came from `CString::into_raw` above.
        drop(unsafe { CString::from_raw(raw) });
        wrap(im)
    }

    /// Encodes the image into `out`.
    pub fn encode(&self, enc: &Encoder, out: &mut dyn Write) -> std::io::Result<()> {
        errors::install();
        match enc {
            Encoder::Gd | Encoder::Gd2(..) => {
                let mut size: c_int = 0;
                // SAFETY: valid image; libgd returns a malloc'd buffer of
                // `size` bytes (or null).
                let buf = unsafe {
                    let p = match enc {
                        Encoder::Gd2(cs, fmt) => {
                            sys::gdImageGd2Ptr(self.raw(), *cs, *fmt, &mut size)
                        }
                        _ => sys::gdImageGdPtr(self.raw(), &mut size),
                    };
                    take_buffer(p, size)
                };
                match buf {
                    Some(b) => out.write_all(&b),
                    None => Ok(()),
                }
            }
            _ => {
                let mut ctx = io::WriteCtx::new(out);
                let c = ctx.as_ctx();
                let im = self.raw();
                // SAFETY: `im` is a live image and `c` a context that
                // outlives the call (libgd never keeps either).
                unsafe {
                    match enc {
                        Encoder::Png(level) => sys::gdImagePngCtxEx(im, c, *level),
                        Encoder::Gif => {
                            let hdr = self.hdr();
                            let (tc, method, speed) = (
                                hdr.trueColor != 0,
                                hdr.paletteQuantizationMethod,
                                hdr.paletteQuantizationSpeed,
                            );
                            if tc {
                                sys::gdImageTrueColorToPaletteSetMethod(im, GD_QUANT_JQUANT, 0);
                            }
                            sys::gdImageGifCtx(im, c);
                            if tc {
                                sys::gdImageTrueColorToPaletteSetMethod(im, method, speed);
                            }
                        }
                        Encoder::Jpeg(q) => sys::gdImageJpegCtx(im, c, *q),
                        Encoder::Webp(q) => sys::gdImageWebpCtx(im, c, *q),
                        Encoder::Wbmp(fg) => sys::gdImageWBMPCtx(im, *fg, c),
                        Encoder::Bmp(z) => sys::gdImageBmpCtx(im, c, c_int::from(*z)),
                        Encoder::Xbm(name, fg) => {
                            let mut name = name.clone();
                            name.retain(|&b| b != 0);
                            let cname = CString::new(name).unwrap_or_default();
                            let raw = cname.into_raw();
                            sys::gdImageXbmCtx(im, raw, *fg, c);
                            drop(CString::from_raw(raw));
                        }
                        Encoder::Gd | Encoder::Gd2(..) => {}
                    }
                }
                ctx.finish()
            }
        }
    }

    /// `gdImageSX`.
    pub fn width(&self) -> i32 {
        self.hdr().sx
    }

    /// `gdImageSY`.
    pub fn height(&self) -> i32 {
        self.hdr().sy
    }

    /// `gdImageTrueColor`.
    pub fn is_truecolor(&self) -> bool {
        self.hdr().trueColor != 0
    }

    /// `gdImageColorsTotal`.
    pub fn colors_total(&self) -> i32 {
        self.hdr().colorsTotal
    }

    /// `gdImageGetTransparent`.
    pub fn transparent(&self) -> i32 {
        self.hdr().transparent
    }

    /// `gdImageGetInterlaced`.
    pub fn interlaced(&self) -> bool {
        self.hdr().interlace != 0
    }

    /// `im->AA`.
    pub fn antialias(&self) -> bool {
        self.hdr().AA != 0
    }

    /// `im->AA = v`.
    pub fn set_antialias_flag(&self, v: bool) {
        self.with_hdr_mut(|h| h.AA = c_int::from(v));
    }

    /// `im->interpolation_id`.
    pub fn interpolation_id(&self) -> i32 {
        self.hdr().interpolation_id
    }

    /// `gdImageResolutionX/Y`.
    pub fn resolution(&self) -> (u32, u32) {
        (self.hdr().res_x, self.hdr().res_y)
    }

    /// `gdImageRed/Green/Blue/Alpha(im, c)`: for truecolor images the
    /// components of the color value, otherwise palette entry `c`
    /// (`None` outside the 256-entry tables).
    pub fn color_components(&self, c: i32) -> Option<[i32; 4]> {
        if self.is_truecolor() {
            return Some([
                (c >> 16) & 0xff,
                (c >> 8) & 0xff,
                c & 0xff,
                (c >> 24) & 0x7f,
            ]);
        }
        let i = usize::try_from(c)
            .ok()
            .filter(|&i| i < sys::GD_MAX_COLORS)?;
        let h = self.hdr();
        Some([h.red[i], h.green[i], h.blue[i], h.alpha[i]])
    }

    /// Sets palette entry `i` (`imagecolorset`, `imagegammacorrect`).
    pub fn set_palette_entry(&self, i: i32, rgba: [i32; 4]) {
        let Some(i) = usize::try_from(i).ok().filter(|&i| i < sys::GD_MAX_COLORS) else {
            return;
        };
        self.with_hdr_mut(|h| {
            h.red[i] = rgba[0];
            h.green[i] = rgba[1];
            h.blue[i] = rgba[2];
            h.alpha[i] = rgba[3];
        });
    }

    /// `imagecolorat()`: the raw pixel value when `(x, y)` is inside the
    /// image (`gdImageBoundsSafe`, which honours the clip rectangle).
    pub fn pixel_at(&self, x: i32, y: i32) -> Option<i32> {
        if !self.bounds_safe(x, y) {
            return None;
        }
        let h = self.hdr();
        let (xi, yi) = (usize::try_from(x).ok()?, usize::try_from(y).ok()?);
        if xi >= usize::try_from(h.sx).ok()? || yi >= usize::try_from(h.sy).ok()? {
            return None;
        }
        // SAFETY: (x, y) is inside the sx×sy pixel arrays libgd allocated.
        unsafe {
            if h.trueColor != 0 {
                if h.tpixels.is_null() {
                    return None;
                }
                Some(*(*h.tpixels.add(yi)).add(xi))
            } else {
                if h.pixels.is_null() {
                    return None;
                }
                Some(i32::from(*(*h.pixels.add(yi)).add(xi)))
            }
        }
    }

    /// `gdImageBoundsSafe`.
    pub fn bounds_safe(&self, x: i32, y: i32) -> bool {
        // SAFETY: valid image.
        unsafe { sys::gdImageBoundsSafe(self.raw(), x, y) != 0 }
    }

    /// `gdImageGetPixel`.
    pub fn get_pixel(&self, x: i32, y: i32) -> i32 {
        // SAFETY: valid image; libgd bounds-checks.
        unsafe { sys::gdImageGetPixel(self.raw(), x, y) }
    }

    /// `gdImageGetTrueColorPixel`.
    pub fn get_truecolor_pixel(&self, x: i32, y: i32) -> i32 {
        // SAFETY: valid image; libgd bounds-checks.
        unsafe { sys::gdImageGetTrueColorPixel(self.raw(), x, y) }
    }

    /// `gdImageSetPixel`.
    pub fn set_pixel(&self, x: i32, y: i32, color: i32) {
        // SAFETY: valid image; libgd bounds-checks.
        unsafe { sys::gdImageSetPixel(self.raw(), x, y, color) }
    }

    /// `gdImageSetStyle`.
    pub fn set_style(&self, style: &[i32]) {
        let mut v = style.to_vec();
        let n = c_int::try_from(v.len()).unwrap_or(c_int::MAX);
        // SAFETY: libgd copies `n` ints from the buffer.
        unsafe { sys::gdImageSetStyle(self.raw(), v.as_mut_ptr(), n) }
    }

    /// `gdImageSetThickness`.
    pub fn set_thickness(&self, t: i32) {
        // SAFETY: valid image.
        unsafe { sys::gdImageSetThickness(self.raw(), t) }
    }

    /// `gdImageSetBrush`: `brush` is kept alive by `self`.
    pub fn set_brush(&self, brush: &Rc<Image>) {
        // SAFETY: both images are valid; `brush` outlives its use by libgd
        // because `self` holds a reference to it (or is it).
        unsafe { sys::gdImageSetBrush(self.raw(), brush.raw()) };
        *self.brush.borrow_mut() = (!std::ptr::eq(self, &**brush)).then(|| Rc::clone(brush));
    }

    /// `gdImageSetTile`: `tile` is kept alive by `self`.
    pub fn set_tile(&self, tile: &Rc<Image>) {
        // SAFETY: as in `set_brush`.
        unsafe { sys::gdImageSetTile(self.raw(), tile.raw()) };
        *self.tile.borrow_mut() = (!std::ptr::eq(self, &**tile)).then(|| Rc::clone(tile));
    }

    /// `gdImageSetAntiAliased`.
    pub fn set_antialiased_color(&self, c: i32) {
        // SAFETY: valid image.
        unsafe { sys::gdImageSetAntiAliased(self.raw(), c) }
    }

    /// `gdImageAlphaBlending`.
    pub fn alpha_blending(&self, mode: i32) {
        // SAFETY: valid image.
        unsafe { sys::gdImageAlphaBlending(self.raw(), mode) }
    }

    /// `gdImageSaveAlpha`.
    pub fn save_alpha(&self, v: bool) {
        // SAFETY: valid image.
        unsafe { sys::gdImageSaveAlpha(self.raw(), c_int::from(v)) }
    }

    /// `gdImageSetClip`.
    pub fn set_clip(&self, x1: i32, y1: i32, x2: i32, y2: i32) {
        // SAFETY: valid image; libgd clamps.
        unsafe { sys::gdImageSetClip(self.raw(), x1, y1, x2, y2) }
    }

    /// `gdImageGetClip`.
    pub fn clip(&self) -> [i32; 4] {
        let (mut a, mut b, mut c, mut d) = (0, 0, 0, 0);
        // SAFETY: valid image and out-pointers.
        unsafe { sys::gdImageGetClip(self.raw(), &mut a, &mut b, &mut c, &mut d) };
        [a, b, c, d]
    }

    /// `gdImageSetResolution`.
    pub fn set_resolution(&self, x: u32, y: u32) {
        // SAFETY: valid image.
        unsafe { sys::gdImageSetResolution(self.raw(), x, y) }
    }

    /// `gdImageInterlace`.
    pub fn set_interlace(&self, v: bool) {
        // SAFETY: valid image.
        unsafe { sys::gdImageInterlace(self.raw(), c_int::from(v)) }
    }

    /// `gdImageColorTransparent`.
    pub fn color_transparent(&self, c: i32) {
        // SAFETY: valid image; libgd range-checks.
        unsafe { sys::gdImageColorTransparent(self.raw(), c) }
    }

    /// `gdImageSetInterpolationMethod`.
    pub fn set_interpolation(&self, id: i32) -> bool {
        // SAFETY: valid image; libgd rejects unknown ids.
        unsafe { sys::gdImageSetInterpolationMethod(self.raw(), id) != 0 }
    }

    /// `gdImageColorAllocate`.
    pub fn color_allocate(&self, r: i32, g: i32, b: i32) -> i32 {
        // SAFETY: valid image.
        unsafe { sys::gdImageColorAllocate(self.raw(), r, g, b) }
    }

    /// `gdImageColorAllocateAlpha`.
    pub fn color_allocate_alpha(&self, r: i32, g: i32, b: i32, a: i32) -> i32 {
        // SAFETY: valid image.
        unsafe { sys::gdImageColorAllocateAlpha(self.raw(), r, g, b, a) }
    }

    /// `gdImageColorClosest`.
    pub fn color_closest(&self, r: i32, g: i32, b: i32) -> i32 {
        // SAFETY: valid image.
        unsafe { sys::gdImageColorClosest(self.raw(), r, g, b) }
    }

    /// `gdImageColorClosestAlpha`.
    pub fn color_closest_alpha(&self, r: i32, g: i32, b: i32, a: i32) -> i32 {
        // SAFETY: valid image.
        unsafe { sys::gdImageColorClosestAlpha(self.raw(), r, g, b, a) }
    }

    /// `gdImageColorClosestHWB`.
    pub fn color_closest_hwb(&self, r: i32, g: i32, b: i32) -> i32 {
        // SAFETY: valid image.
        unsafe { sys::gdImageColorClosestHWB(self.raw(), r, g, b) }
    }

    /// `gdImageColorExact`.
    pub fn color_exact(&self, r: i32, g: i32, b: i32) -> i32 {
        // SAFETY: valid image.
        unsafe { sys::gdImageColorExact(self.raw(), r, g, b) }
    }

    /// `gdImageColorExactAlpha`.
    pub fn color_exact_alpha(&self, r: i32, g: i32, b: i32, a: i32) -> i32 {
        // SAFETY: valid image.
        unsafe { sys::gdImageColorExactAlpha(self.raw(), r, g, b, a) }
    }

    /// `gdImageColorResolve`.
    pub fn color_resolve(&self, r: i32, g: i32, b: i32) -> i32 {
        // SAFETY: valid image.
        unsafe { sys::gdImageColorResolve(self.raw(), r, g, b) }
    }

    /// `gdImageColorResolveAlpha`.
    pub fn color_resolve_alpha(&self, r: i32, g: i32, b: i32, a: i32) -> i32 {
        // SAFETY: valid image.
        unsafe { sys::gdImageColorResolveAlpha(self.raw(), r, g, b, a) }
    }

    /// `gdImageColorDeallocate`.
    pub fn color_deallocate(&self, c: i32) {
        // SAFETY: valid image; libgd range-checks.
        unsafe { sys::gdImageColorDeallocate(self.raw(), c) }
    }

    /// `gdImagePaletteCopy(dst, src)`.
    pub fn palette_copy(dst: &Image, src: &Image) {
        // SAFETY: both images are valid (possibly the same).
        unsafe { sys::gdImagePaletteCopy(dst.raw(), src.raw()) }
    }

    /// `gdImageColorMatch(im1, im2)`: 0 or libgd's negative error code.
    pub fn color_match(im1: &Image, im2: &Image) -> i32 {
        // SAFETY: both images are valid.
        unsafe { sys::gdImageColorMatch(im1.raw(), im2.raw()) }
    }

    /// `gdImageTrueColorToPalette` with PHP's `GD_QUANT_JQUANT`.
    pub fn truecolor_to_palette(&self, dither: bool, colors: i32) -> bool {
        // SAFETY: valid image.
        unsafe {
            sys::gdImageTrueColorToPaletteSetMethod(self.raw(), GD_QUANT_JQUANT, 0);
            sys::gdImageTrueColorToPalette(self.raw(), c_int::from(dither), colors) != 0
        }
    }

    /// `gdImagePaletteToTrueColor`.
    pub fn palette_to_truecolor(&self) -> bool {
        // SAFETY: valid image.
        unsafe { sys::gdImagePaletteToTrueColor(self.raw()) != 0 }
    }

    /// `gdImageLine`.
    pub fn line(&self, x1: i32, y1: i32, x2: i32, y2: i32, c: i32) {
        // SAFETY: valid image; libgd clips.
        unsafe { sys::gdImageLine(self.raw(), x1, y1, x2, y2, c) }
    }

    /// `gdImageDashedLine`.
    pub fn dashed_line(&self, x1: i32, y1: i32, x2: i32, y2: i32, c: i32) {
        // SAFETY: valid image; libgd clips.
        unsafe { sys::gdImageDashedLine(self.raw(), x1, y1, x2, y2, c) }
    }

    /// `gdImageRectangle`.
    pub fn rectangle(&self, x1: i32, y1: i32, x2: i32, y2: i32, c: i32) {
        // SAFETY: valid image; libgd clips.
        unsafe { sys::gdImageRectangle(self.raw(), x1, y1, x2, y2, c) }
    }

    /// `gdImageFilledRectangle`.
    pub fn filled_rectangle(&self, x1: i32, y1: i32, x2: i32, y2: i32, c: i32) {
        // SAFETY: valid image; libgd clips.
        unsafe { sys::gdImageFilledRectangle(self.raw(), x1, y1, x2, y2, c) }
    }

    /// `gdImageArc`.
    #[allow(clippy::too_many_arguments)]
    pub fn arc(&self, cx: i32, cy: i32, w: i32, h: i32, s: i32, e: i32, c: i32) {
        // SAFETY: valid image; libgd clips.
        unsafe { sys::gdImageArc(self.raw(), cx, cy, w, h, s, e, c) }
    }

    /// `gdImageFilledArc`.
    #[allow(clippy::too_many_arguments)]
    pub fn filled_arc(&self, cx: i32, cy: i32, w: i32, h: i32, s: i32, e: i32, c: i32, style: i32) {
        // SAFETY: valid image; libgd clips.
        unsafe { sys::gdImageFilledArc(self.raw(), cx, cy, w, h, s, e, c, style) }
    }

    /// `gdImageEllipse`.
    pub fn ellipse(&self, cx: i32, cy: i32, w: i32, h: i32, c: i32) {
        // SAFETY: valid image; libgd clips.
        unsafe { sys::gdImageEllipse(self.raw(), cx, cy, w, h, c) }
    }

    /// `gdImageFilledEllipse`.
    pub fn filled_ellipse(&self, cx: i32, cy: i32, w: i32, h: i32, c: i32) {
        // SAFETY: valid image; libgd clips.
        unsafe { sys::gdImageFilledEllipse(self.raw(), cx, cy, w, h, c) }
    }

    /// `gdImageFillToBorder`.
    pub fn fill_to_border(&self, x: i32, y: i32, border: i32, c: i32) {
        // SAFETY: valid image; libgd bounds-checks.
        unsafe { sys::gdImageFillToBorder(self.raw(), x, y, border, c) }
    }

    /// `gdImageFill`.
    pub fn fill(&self, x: i32, y: i32, c: i32) {
        // SAFETY: valid image; libgd bounds-checks.
        unsafe { sys::gdImageFill(self.raw(), x, y, c) }
    }

    /// `gdImagePolygon` (`kind` 0), `gdImageOpenPolygon` (-1) or
    /// `gdImageFilledPolygon` (1).
    pub fn polygon(&self, points: &[Point], kind: i32, c: i32) {
        let mut pts = points.to_vec();
        let n = c_int::try_from(pts.len()).unwrap_or(c_int::MAX);
        let p = pts.as_mut_ptr();
        // SAFETY: `p` holds `n` points for the duration of the call.
        unsafe {
            match kind {
                -1 => sys::gdImageOpenPolygon(self.raw(), p, n, c),
                0 => sys::gdImagePolygon(self.raw(), p, n, c),
                _ => sys::gdImageFilledPolygon(self.raw(), p, n, c),
            }
        }
    }

    /// `gdImageChar`.
    pub fn draw_char(&self, font: &Font, x: i32, y: i32, ch: i32, color: i32) {
        // SAFETY: valid image and font (the font data stays alive while
        // `font` is borrowed); libgd range-checks the character.
        unsafe { sys::gdImageChar(self.raw(), font.raw(), x, y, ch, color) }
    }

    /// `gdImageCharUp`.
    pub fn draw_char_up(&self, font: &Font, x: i32, y: i32, ch: i32, color: i32) {
        // SAFETY: as in `draw_char`.
        unsafe { sys::gdImageCharUp(self.raw(), font.raw(), x, y, ch, color) }
    }

    /// `gdImageCopy`.
    #[allow(clippy::too_many_arguments)]
    pub fn copy(dst: &Image, src: &Image, dx: i32, dy: i32, sx: i32, sy: i32, w: i32, h: i32) {
        // SAFETY: both images valid (possibly the same); libgd clips.
        unsafe { sys::gdImageCopy(dst.raw(), src.raw(), dx, dy, sx, sy, w, h) }
    }

    /// `gdImageCopyMerge` (`gray` selects `gdImageCopyMergeGray`).
    #[allow(clippy::too_many_arguments)]
    pub fn copy_merge(
        dst: &Image,
        src: &Image,
        gray: bool,
        d: (i32, i32),
        s: (i32, i32),
        w: i32,
        h: i32,
        pct: i32,
    ) {
        // SAFETY: both images valid; libgd clips.
        unsafe {
            if gray {
                sys::gdImageCopyMergeGray(dst.raw(), src.raw(), d.0, d.1, s.0, s.1, w, h, pct);
            } else {
                sys::gdImageCopyMerge(dst.raw(), src.raw(), d.0, d.1, s.0, s.1, w, h, pct);
            }
        }
    }

    /// `gdImageCopyResized` / `gdImageCopyResampled`.
    #[allow(clippy::too_many_arguments)]
    pub fn copy_resized(
        dst: &Image,
        src: &Image,
        resample: bool,
        d: (i32, i32),
        s: (i32, i32),
        dsize: (i32, i32),
        ssize: (i32, i32),
    ) {
        // SAFETY: both images valid; libgd clips.
        unsafe {
            let f = if resample {
                sys::gdImageCopyResampled
            } else {
                sys::gdImageCopyResized
            };
            f(
                dst.raw(),
                src.raw(),
                d.0,
                d.1,
                s.0,
                s.1,
                dsize.0,
                dsize.1,
                ssize.0,
                ssize.1,
            );
        }
    }

    /// `gdImageRotateInterpolated`.
    pub fn rotate(&self, angle: f32, bg: i32) -> Option<Image> {
        // SAFETY: valid image; returns a new image or null.
        wrap(unsafe { sys::gdImageRotateInterpolated(self.raw(), angle, bg) })
    }

    /// `gdImageCrop`.
    pub fn crop(&self, r: Rect) -> Option<Image> {
        // SAFETY: valid image and rectangle.
        wrap(unsafe { sys::gdImageCrop(self.raw(), &r) })
    }

    /// `gdImageCropAuto`.
    pub fn crop_auto(&self, mode: u32) -> Option<Image> {
        // SAFETY: valid image.
        wrap(unsafe { sys::gdImageCropAuto(self.raw(), mode) })
    }

    /// `gdImageCropThreshold`.
    pub fn crop_threshold(&self, color: u32, threshold: f32) -> Option<Image> {
        // SAFETY: valid image.
        wrap(unsafe { sys::gdImageCropThreshold(self.raw(), color, threshold) })
    }

    /// `gdImageScale`.
    pub fn scale(&self, w: u32, h: u32) -> Option<Image> {
        // SAFETY: valid image.
        wrap(unsafe { sys::gdImageScale(self.raw(), w, h) })
    }

    /// `gdTransformAffineGetImage`.
    pub fn affine(&self, m: &[f64; 6], clip: Option<Rect>) -> Option<Image> {
        let mut dst: *mut sys::gdImage = std::ptr::null_mut();
        let mut rect = clip.unwrap_or_default();
        let area = if clip.is_some() {
            std::ptr::from_mut(&mut rect)
        } else {
            std::ptr::null_mut()
        };
        // SAFETY: valid image, matrix of 6 doubles, optional rectangle and
        // an out-pointer for the new image.
        let ok = unsafe { sys::gdTransformAffineGetImage(&mut dst, self.raw(), area, m.as_ptr()) };
        let im = wrap(dst);
        if ok == 1 {
            im
        } else {
            None
        }
    }

    /// `gdImageFlipHorizontal` (1), `gdImageFlipVertical` (2) or
    /// `gdImageFlipBoth` (3).
    pub fn flip(&self, mode: i32) {
        // SAFETY: valid image.
        unsafe {
            match mode {
                1 => sys::gdImageFlipHorizontal(self.raw()),
                2 => sys::gdImageFlipVertical(self.raw()),
                _ => sys::gdImageFlipBoth(self.raw()),
            }
        }
    }

    /// The `imagefilter()` filters implemented by libgd.
    pub fn filter(&self, f: &Filter) -> bool {
        let im = self.raw();
        // SAFETY: valid image; the color list lives for the call.
        unsafe {
            match f {
                Filter::Negate => sys::gdImageNegate(im) == 1,
                Filter::Grayscale => sys::gdImageGrayScale(im) == 1,
                Filter::Brightness(b) => sys::gdImageBrightness(im, *b) == 1,
                Filter::Contrast(c) => sys::gdImageContrast(im, *c) == 1,
                Filter::Colorize(r, g, b, a) => sys::gdImageColor(im, *r, *g, *b, *a) == 1,
                Filter::EdgeDetect => sys::gdImageEdgeDetectQuick(im) == 1,
                Filter::Emboss => sys::gdImageEmboss(im) == 1,
                Filter::GaussianBlur => sys::gdImageGaussianBlur(im) == 1,
                Filter::SelectiveBlur => sys::gdImageSelectiveBlur(im) == 1,
                Filter::MeanRemoval => sys::gdImageMeanRemoval(im) == 1,
                Filter::Smooth(w) => sys::gdImageSmooth(im, *w) == 1,
                Filter::Pixelate(bs, mode) => sys::gdImagePixelate(im, *bs, u32::from(*mode)) != 0,
                Filter::Scatter(sub, plus, colors) => {
                    if colors.is_empty() {
                        sys::gdImageScatter(im, *sub, *plus) != 0
                    } else {
                        let mut c = colors.clone();
                        let n = u32::try_from(c.len()).unwrap_or(u32::MAX);
                        sys::gdImageScatterColor(im, *sub, *plus, c.as_mut_ptr(), n) != 0
                    }
                }
                Filter::Convolution(m, div, offset) => {
                    let mut m = *m;
                    sys::gdImageConvolution(im, m.as_mut_ptr(), *div, *offset) != 0
                }
            }
        }
    }

    /// `gdImageStringFT` / `gdImageStringFTEx` (with `linespacing`): draws
    /// on `im` (or only measures when `None`). Returns the bounding box or
    /// libgd's error text.
    #[allow(clippy::too_many_arguments)]
    pub fn string_ft(
        im: Option<&Image>,
        fg: i32,
        font: &[u8],
        size: f64,
        angle: f64,
        x: i32,
        y: i32,
        text: &[u8],
        linespacing: Option<f64>,
    ) -> Result<[i32; 8], Vec<u8>> {
        errors::install();
        // C strings stop at the first NUL byte.
        let cut = |b: &[u8]| CString::new(b.split(|&c| c == 0).next().unwrap_or_default().to_vec());
        let (Ok(font), Ok(text)) = (cut(font), cut(text)) else {
            return Err(b"Invalid string".to_vec());
        };
        let mut brect = [0 as c_int; 8];
        let imp = im.map_or(std::ptr::null_mut(), Image::raw);
        // SAFETY: valid (or null) image, out-array of 8 ints, NUL-terminated
        // strings and an initialised extra structure.
        let err = unsafe {
            if let Some(ls) = linespacing {
                let mut ex = sys::gdFTStringExtra {
                    flags: sys::GD_FTEX_LINESPACE,
                    linespacing: ls,
                    charmap: 0,
                    hdpi: 0,
                    vdpi: 0,
                    xshow: std::ptr::null_mut(),
                    fontpath: std::ptr::null_mut(),
                };
                sys::gdImageStringFTEx(
                    imp,
                    brect.as_mut_ptr(),
                    fg,
                    font.as_ptr(),
                    size,
                    angle,
                    x,
                    y,
                    text.as_ptr(),
                    &mut ex,
                )
            } else {
                sys::gdImageStringFT(
                    imp,
                    brect.as_mut_ptr(),
                    fg,
                    font.as_ptr(),
                    size,
                    angle,
                    x,
                    y,
                    text.as_ptr(),
                )
            }
        };
        if err.is_null() {
            Ok(brect)
        } else {
            // SAFETY: libgd returns a static NUL-terminated error text.
            Err(unsafe { CStr::from_ptr(err) }.to_bytes().to_vec())
        }
    }
}

/// `gdFontCacheShutdown` (`PHP_RSHUTDOWN(gd)`).
pub fn font_cache_shutdown() {
    // SAFETY: no FreeType call is in progress (single-threaded request end).
    unsafe { sys::gdFontCacheShutdown() }
}

/// The `imagefilter()` filters.
#[derive(Clone, Debug, PartialEq)]
pub enum Filter {
    /// `IMG_FILTER_NEGATE`.
    Negate,
    /// `IMG_FILTER_GRAYSCALE`.
    Grayscale,
    /// `IMG_FILTER_BRIGHTNESS`.
    Brightness(i32),
    /// `IMG_FILTER_CONTRAST`.
    Contrast(f64),
    /// `IMG_FILTER_COLORIZE`.
    Colorize(i32, i32, i32, i32),
    /// `IMG_FILTER_EDGEDETECT`.
    EdgeDetect,
    /// `IMG_FILTER_EMBOSS`.
    Emboss,
    /// `IMG_FILTER_GAUSSIAN_BLUR`.
    GaussianBlur,
    /// `IMG_FILTER_SELECTIVE_BLUR`.
    SelectiveBlur,
    /// `IMG_FILTER_MEAN_REMOVAL`.
    MeanRemoval,
    /// `IMG_FILTER_SMOOTH`.
    Smooth(f32),
    /// `IMG_FILTER_PIXELATE` (block size, advanced mode).
    Pixelate(i32, bool),
    /// `IMG_FILTER_SCATTER` (sub, plus, colors).
    Scatter(i32, i32, Vec<i32>),
    /// `imageconvolution()`.
    Convolution([[f32; 3]; 3], f32, f32),
}

/// The affine matrix helpers of `gd_matrix.c`.
pub mod affine {
    use super::sys;

    /// `gdAffineTranslate`.
    pub fn translate(x: f64, y: f64) -> Option<[f64; 6]> {
        let mut m = [0.0; 6];
        // SAFETY: `m` has room for 6 doubles.
        (unsafe { sys::gdAffineTranslate(m.as_mut_ptr(), x, y) } == 1).then_some(m)
    }

    /// `gdAffineScale`.
    pub fn scale(x: f64, y: f64) -> Option<[f64; 6]> {
        let mut m = [0.0; 6];
        // SAFETY: `m` has room for 6 doubles.
        (unsafe { sys::gdAffineScale(m.as_mut_ptr(), x, y) } == 1).then_some(m)
    }

    /// `gdAffineRotate`.
    pub fn rotate(angle: f64) -> Option<[f64; 6]> {
        let mut m = [0.0; 6];
        // SAFETY: `m` has room for 6 doubles.
        (unsafe { sys::gdAffineRotate(m.as_mut_ptr(), angle) } == 1).then_some(m)
    }

    /// `gdAffineShearHorizontal`.
    pub fn shear_horizontal(angle: f64) -> Option<[f64; 6]> {
        let mut m = [0.0; 6];
        // SAFETY: `m` has room for 6 doubles.
        (unsafe { sys::gdAffineShearHorizontal(m.as_mut_ptr(), angle) } == 1).then_some(m)
    }

    /// `gdAffineShearVertical`.
    pub fn shear_vertical(angle: f64) -> Option<[f64; 6]> {
        let mut m = [0.0; 6];
        // SAFETY: `m` has room for 6 doubles.
        (unsafe { sys::gdAffineShearVertical(m.as_mut_ptr(), angle) } == 1).then_some(m)
    }

    /// `gdAffineConcat`.
    pub fn concat(m1: &[f64; 6], m2: &[f64; 6]) -> Option<[f64; 6]> {
        let mut m = [0.0; 6];
        // SAFETY: all three arrays hold 6 doubles.
        (unsafe { sys::gdAffineConcat(m.as_mut_ptr(), m1.as_ptr(), m2.as_ptr()) } == 1).then_some(m)
    }
}

/// A bitmap font (`gdFont`): one of libgd's five built-in fonts or one
/// loaded by `imageloadfont()`.
pub struct Font {
    raw: NonNull<sys::gdFont>,
    /// Owned storage of a loaded font (`raw` points into it).
    owned: Option<(Box<sys::gdFont>, Box<[u8]>)>,
}

impl std::fmt::Debug for Font {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Font")
            .field("nchars", &self.nchars())
            .field("offset", &self.offset())
            .field("w", &self.width())
            .field("h", &self.height())
            .finish()
    }
}

impl Font {
    /// `php_find_gd_font()` for an integer: 1..=5, clamped to tiny/giant.
    pub fn builtin(n: i64) -> Font {
        // SAFETY: the getters return pointers to libgd's static fonts.
        let p = unsafe {
            match n {
                i64::MIN..=1 => sys::gdFontGetTiny(),
                2 => sys::gdFontGetSmall(),
                3 => sys::gdFontGetMediumBold(),
                4 => sys::gdFontGetLarge(),
                _ => sys::gdFontGetGiant(),
            }
        };
        match NonNull::new(p) {
            Some(raw) => Font { raw, owned: None },
            None => Font::from_parts(0, 0, 0, 0, Vec::new()),
        }
    }

    /// A font with the given header and glyph bitmap (one byte per pixel).
    pub fn from_parts(nchars: i32, offset: i32, w: i32, h: i32, data: Vec<u8>) -> Font {
        let mut data = data.into_boxed_slice();
        let mut hdr = Box::new(sys::gdFont {
            nchars,
            offset,
            w,
            h,
            data: data.as_mut_ptr().cast(),
        });
        let raw = NonNull::from(&mut *hdr);
        Font {
            raw,
            owned: Some((hdr, data)),
        }
    }

    fn raw(&self) -> *mut sys::gdFont {
        self.raw.as_ptr()
    }

    fn hdr(&self) -> &sys::gdFont {
        // SAFETY: static libgd font or our boxed header; never mutated.
        unsafe { self.raw.as_ref() }
    }

    /// `nchars`.
    pub fn nchars(&self) -> i32 {
        self.hdr().nchars
    }

    /// `offset`.
    pub fn offset(&self) -> i32 {
        self.hdr().offset
    }

    /// `w`.
    pub fn width(&self) -> i32 {
        self.hdr().w
    }

    /// `h`.
    pub fn height(&self) -> i32 {
        self.hdr().h
    }

    /// The glyph bitmap (`nchars * w * h` bytes).
    pub fn data(&self) -> &[u8] {
        if let Some((_, d)) = &self.owned {
            return d;
        }
        let h = self.hdr();
        let len = [h.nchars, h.w, h.h]
            .iter()
            .try_fold(1usize, |acc, &v| acc.checked_mul(usize::try_from(v).ok()?))
            .unwrap_or(0);
        if h.data.is_null() || len == 0 {
            return &[];
        }
        // SAFETY: libgd's static fonts hold `nchars * w * h` bytes.
        unsafe { std::slice::from_raw_parts(h.data.cast::<u8>(), len) }
    }
}
