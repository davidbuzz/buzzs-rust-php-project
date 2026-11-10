//! Raw declarations of the system libgd 2.3.3 API (`gd.h`, `gd_io.h`,
//! `gdfont*.h`). Layouts are checked against the C headers in the unit
//! tests below (`sizeof(gdImage) == 7336` on LP64).

#![allow(non_snake_case, non_camel_case_types, dead_code)]

use std::os::raw::{c_char, c_double, c_float, c_int, c_long, c_uint, c_void};

/// `gdMaxColors`.
pub const GD_MAX_COLORS: usize = 256;

/// `struct gdImageStruct`.
#[repr(C)]
pub struct gdImage {
    pub pixels: *mut *mut u8,
    pub sx: c_int,
    pub sy: c_int,
    pub colorsTotal: c_int,
    pub red: [c_int; GD_MAX_COLORS],
    pub green: [c_int; GD_MAX_COLORS],
    pub blue: [c_int; GD_MAX_COLORS],
    pub open: [c_int; GD_MAX_COLORS],
    pub transparent: c_int,
    pub polyInts: *mut c_int,
    pub polyAllocated: c_int,
    pub brush: *mut gdImage,
    pub tile: *mut gdImage,
    pub brushColorMap: [c_int; GD_MAX_COLORS],
    pub tileColorMap: [c_int; GD_MAX_COLORS],
    pub styleLength: c_int,
    pub stylePos: c_int,
    pub style: *mut c_int,
    pub interlace: c_int,
    pub thick: c_int,
    pub alpha: [c_int; GD_MAX_COLORS],
    pub trueColor: c_int,
    pub tpixels: *mut *mut c_int,
    pub alphaBlendingFlag: c_int,
    pub saveAlphaFlag: c_int,
    pub AA: c_int,
    pub AA_color: c_int,
    pub AA_dont_blend: c_int,
    pub cx1: c_int,
    pub cy1: c_int,
    pub cx2: c_int,
    pub cy2: c_int,
    pub res_x: c_uint,
    pub res_y: c_uint,
    pub paletteQuantizationMethod: c_int,
    pub paletteQuantizationSpeed: c_int,
    pub paletteQuantizationMinQuality: c_int,
    pub paletteQuantizationMaxQuality: c_int,
    pub interpolation_id: c_int,
    pub interpolation: Option<unsafe extern "C" fn(c_double, c_double) -> c_double>,
}

pub type gdImagePtr = *mut gdImage;

/// `gdFont`.
#[repr(C)]
pub struct gdFont {
    pub nchars: c_int,
    pub offset: c_int,
    pub w: c_int,
    pub h: c_int,
    pub data: *mut c_char,
}

/// `gdIOCtx`.
#[repr(C)]
pub struct gdIOCtx {
    pub getC: Option<unsafe extern "C" fn(*mut gdIOCtx) -> c_int>,
    pub getBuf: Option<unsafe extern "C" fn(*mut gdIOCtx, *mut c_void, c_int) -> c_int>,
    pub putC: Option<unsafe extern "C" fn(*mut gdIOCtx, c_int)>,
    pub putBuf: Option<unsafe extern "C" fn(*mut gdIOCtx, *const c_void, c_int) -> c_int>,
    pub seek: Option<unsafe extern "C" fn(*mut gdIOCtx, c_int) -> c_int>,
    pub tell: Option<unsafe extern "C" fn(*mut gdIOCtx) -> c_long>,
    pub gd_free: Option<unsafe extern "C" fn(*mut gdIOCtx)>,
    pub data: *mut c_void,
}

/// `gdPoint`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct gdPoint {
    pub x: c_int,
    pub y: c_int,
}

/// `gdRect`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct gdRect {
    pub x: c_int,
    pub y: c_int,
    pub width: c_int,
    pub height: c_int,
}

/// `gdFTStringExtra`.
#[repr(C)]
pub struct gdFTStringExtra {
    pub flags: c_int,
    pub linespacing: c_double,
    pub charmap: c_int,
    pub hdpi: c_int,
    pub vdpi: c_int,
    pub xshow: *mut c_char,
    pub fontpath: *mut c_char,
}

/// `gdFTEX_LINESPACE`.
pub const GD_FTEX_LINESPACE: c_int = 1;

/// `gdErrorMethod`: the `va_list` arrives as a pointer (see
/// [`super::errors`]).
pub type gdErrorMethod = unsafe extern "C" fn(c_int, *const c_char, *mut c_void);

// libgd is bound lazily (see `build.rs`): the generated stubs define the
// functions below and load libgd and its image-format libraries (libheif,
// libtiff, libwebp, fontconfig, ...) on the first call, keeping them out of
// every start-up.
include!(concat!(env!("OUT_DIR"), "/lazy_gd.rs"));

extern "C" {
    pub fn gdSetErrorMethod(m: gdErrorMethod);
    pub fn gdFree(m: *mut c_void);

    pub fn gdImageCreate(sx: c_int, sy: c_int) -> gdImagePtr;
    pub fn gdImageCreateTrueColor(sx: c_int, sy: c_int) -> gdImagePtr;
    pub fn gdImageDestroy(im: gdImagePtr);
    pub fn gdImageClone(src: gdImagePtr) -> gdImagePtr;

    pub fn gdImageCreateFromPngPtr(size: c_int, data: *mut c_void) -> gdImagePtr;
    pub fn gdImageCreateFromGifPtr(size: c_int, data: *mut c_void) -> gdImagePtr;
    pub fn gdImageCreateFromWBMPPtr(size: c_int, data: *mut c_void) -> gdImagePtr;
    pub fn gdImageCreateFromJpegPtrEx(size: c_int, data: *mut c_void, ignore: c_int) -> gdImagePtr;
    pub fn gdImageCreateFromWebpPtr(size: c_int, data: *mut c_void) -> gdImagePtr;
    pub fn gdImageCreateFromTgaPtr(size: c_int, data: *mut c_void) -> gdImagePtr;
    pub fn gdImageCreateFromBmpPtr(size: c_int, data: *mut c_void) -> gdImagePtr;
    pub fn gdImageCreateFromGdPtr(size: c_int, data: *mut c_void) -> gdImagePtr;
    pub fn gdImageCreateFromGd2Ptr(size: c_int, data: *mut c_void) -> gdImagePtr;
    pub fn gdImageCreateFromGd2PartPtr(
        size: c_int,
        data: *mut c_void,
        srcx: c_int,
        srcy: c_int,
        w: c_int,
        h: c_int,
    ) -> gdImagePtr;
    pub fn gdImageCreateFromXbm(f: *mut libc::FILE) -> gdImagePtr;
    pub fn gdImageCreateFromXpm(filename: *mut c_char) -> gdImagePtr;

    pub fn gdImagePngCtxEx(im: gdImagePtr, out: *mut gdIOCtx, level: c_int);
    pub fn gdImageGifCtx(im: gdImagePtr, out: *mut gdIOCtx);
    pub fn gdImageJpegCtx(im: gdImagePtr, out: *mut gdIOCtx, quality: c_int);
    pub fn gdImageWebpCtx(im: gdImagePtr, out: *mut gdIOCtx, quality: c_int);
    pub fn gdImageWBMPCtx(im: gdImagePtr, fg: c_int, out: *mut gdIOCtx);
    pub fn gdImageBmpCtx(im: gdImagePtr, out: *mut gdIOCtx, compression: c_int);
    pub fn gdImageXbmCtx(im: gdImagePtr, name: *mut c_char, fg: c_int, out: *mut gdIOCtx);
    pub fn gdImageGdPtr(im: gdImagePtr, size: *mut c_int) -> *mut c_void;
    pub fn gdImageGd2Ptr(im: gdImagePtr, cs: c_int, fmt: c_int, size: *mut c_int) -> *mut c_void;

    pub fn gdImageSetPixel(im: gdImagePtr, x: c_int, y: c_int, color: c_int);
    pub fn gdImageGetPixel(im: gdImagePtr, x: c_int, y: c_int) -> c_int;
    pub fn gdImageGetTrueColorPixel(im: gdImagePtr, x: c_int, y: c_int) -> c_int;
    pub fn gdImageLine(im: gdImagePtr, x1: c_int, y1: c_int, x2: c_int, y2: c_int, c: c_int);
    pub fn gdImageDashedLine(im: gdImagePtr, x1: c_int, y1: c_int, x2: c_int, y2: c_int, c: c_int);
    pub fn gdImageRectangle(im: gdImagePtr, x1: c_int, y1: c_int, x2: c_int, y2: c_int, c: c_int);
    pub fn gdImageFilledRectangle(
        im: gdImagePtr,
        x1: c_int,
        y1: c_int,
        x2: c_int,
        y2: c_int,
        c: c_int,
    );
    pub fn gdImageSetClip(im: gdImagePtr, x1: c_int, y1: c_int, x2: c_int, y2: c_int);
    pub fn gdImageGetClip(
        im: gdImagePtr,
        x1: *mut c_int,
        y1: *mut c_int,
        x2: *mut c_int,
        y2: *mut c_int,
    );
    pub fn gdImageSetResolution(im: gdImagePtr, rx: c_uint, ry: c_uint);
    pub fn gdImageBoundsSafe(im: gdImagePtr, x: c_int, y: c_int) -> c_int;
    pub fn gdImageChar(im: gdImagePtr, f: *mut gdFont, x: c_int, y: c_int, c: c_int, color: c_int);
    pub fn gdImageCharUp(
        im: gdImagePtr,
        f: *mut gdFont,
        x: c_int,
        y: c_int,
        c: c_int,
        color: c_int,
    );
    pub fn gdFontCacheShutdown();
    pub fn gdImageStringFT(
        im: gdImagePtr,
        brect: *mut c_int,
        fg: c_int,
        fontlist: *const c_char,
        ptsize: c_double,
        angle: c_double,
        x: c_int,
        y: c_int,
        string: *const c_char,
    ) -> *mut c_char;
    pub fn gdImageStringFTEx(
        im: gdImagePtr,
        brect: *mut c_int,
        fg: c_int,
        fontlist: *const c_char,
        ptsize: c_double,
        angle: c_double,
        x: c_int,
        y: c_int,
        string: *const c_char,
        strex: *mut gdFTStringExtra,
    ) -> *mut c_char;
    pub fn gdImagePolygon(im: gdImagePtr, p: *mut gdPoint, n: c_int, c: c_int);
    pub fn gdImageOpenPolygon(im: gdImagePtr, p: *mut gdPoint, n: c_int, c: c_int);
    pub fn gdImageFilledPolygon(im: gdImagePtr, p: *mut gdPoint, n: c_int, c: c_int);

    pub fn gdImageColorAllocate(im: gdImagePtr, r: c_int, g: c_int, b: c_int) -> c_int;
    pub fn gdImageColorAllocateAlpha(
        im: gdImagePtr,
        r: c_int,
        g: c_int,
        b: c_int,
        a: c_int,
    ) -> c_int;
    pub fn gdImageColorClosest(im: gdImagePtr, r: c_int, g: c_int, b: c_int) -> c_int;
    pub fn gdImageColorClosestAlpha(
        im: gdImagePtr,
        r: c_int,
        g: c_int,
        b: c_int,
        a: c_int,
    ) -> c_int;
    pub fn gdImageColorClosestHWB(im: gdImagePtr, r: c_int, g: c_int, b: c_int) -> c_int;
    pub fn gdImageColorExact(im: gdImagePtr, r: c_int, g: c_int, b: c_int) -> c_int;
    pub fn gdImageColorExactAlpha(im: gdImagePtr, r: c_int, g: c_int, b: c_int, a: c_int) -> c_int;
    pub fn gdImageColorResolve(im: gdImagePtr, r: c_int, g: c_int, b: c_int) -> c_int;
    pub fn gdImageColorResolveAlpha(
        im: gdImagePtr,
        r: c_int,
        g: c_int,
        b: c_int,
        a: c_int,
    ) -> c_int;
    pub fn gdImageColorDeallocate(im: gdImagePtr, color: c_int);
    pub fn gdImageTrueColorToPalette(im: gdImagePtr, dither: c_int, colors: c_int) -> c_int;
    pub fn gdImagePaletteToTrueColor(im: gdImagePtr) -> c_int;
    pub fn gdImageColorMatch(im1: gdImagePtr, im2: gdImagePtr) -> c_int;
    pub fn gdImageTrueColorToPaletteSetMethod(im: gdImagePtr, method: c_int, speed: c_int)
        -> c_int;
    pub fn gdImageColorTransparent(im: gdImagePtr, color: c_int);
    pub fn gdImagePaletteCopy(dst: gdImagePtr, src: gdImagePtr);

    pub fn gdImageFilledArc(
        im: gdImagePtr,
        cx: c_int,
        cy: c_int,
        w: c_int,
        h: c_int,
        s: c_int,
        e: c_int,
        color: c_int,
        style: c_int,
    );
    pub fn gdImageArc(
        im: gdImagePtr,
        cx: c_int,
        cy: c_int,
        w: c_int,
        h: c_int,
        s: c_int,
        e: c_int,
        color: c_int,
    );
    pub fn gdImageEllipse(im: gdImagePtr, cx: c_int, cy: c_int, w: c_int, h: c_int, c: c_int);
    pub fn gdImageFilledEllipse(im: gdImagePtr, cx: c_int, cy: c_int, w: c_int, h: c_int, c: c_int);
    pub fn gdImageFillToBorder(im: gdImagePtr, x: c_int, y: c_int, border: c_int, c: c_int);
    pub fn gdImageFill(im: gdImagePtr, x: c_int, y: c_int, c: c_int);
    pub fn gdImageCopy(
        dst: gdImagePtr,
        src: gdImagePtr,
        dx: c_int,
        dy: c_int,
        sx: c_int,
        sy: c_int,
        w: c_int,
        h: c_int,
    );
    pub fn gdImageCopyMerge(
        dst: gdImagePtr,
        src: gdImagePtr,
        dx: c_int,
        dy: c_int,
        sx: c_int,
        sy: c_int,
        w: c_int,
        h: c_int,
        pct: c_int,
    );
    pub fn gdImageCopyMergeGray(
        dst: gdImagePtr,
        src: gdImagePtr,
        dx: c_int,
        dy: c_int,
        sx: c_int,
        sy: c_int,
        w: c_int,
        h: c_int,
        pct: c_int,
    );
    pub fn gdImageCopyResized(
        dst: gdImagePtr,
        src: gdImagePtr,
        dx: c_int,
        dy: c_int,
        sx: c_int,
        sy: c_int,
        dw: c_int,
        dh: c_int,
        sw: c_int,
        sh: c_int,
    );
    pub fn gdImageCopyResampled(
        dst: gdImagePtr,
        src: gdImagePtr,
        dx: c_int,
        dy: c_int,
        sx: c_int,
        sy: c_int,
        dw: c_int,
        dh: c_int,
        sw: c_int,
        sh: c_int,
    );
    pub fn gdImageSetBrush(im: gdImagePtr, brush: gdImagePtr);
    pub fn gdImageSetTile(im: gdImagePtr, tile: gdImagePtr);
    pub fn gdImageSetAntiAliased(im: gdImagePtr, c: c_int);
    pub fn gdImageSetStyle(im: gdImagePtr, style: *mut c_int, n: c_int);
    pub fn gdImageSetThickness(im: gdImagePtr, t: c_int);
    pub fn gdImageInterlace(im: gdImagePtr, v: c_int);
    pub fn gdImageAlphaBlending(im: gdImagePtr, v: c_int);
    pub fn gdImageSaveAlpha(im: gdImagePtr, v: c_int);

    pub fn gdImagePixelate(im: gdImagePtr, block: c_int, mode: c_uint) -> c_int;
    pub fn gdImageScatter(im: gdImagePtr, sub: c_int, plus: c_int) -> c_int;
    pub fn gdImageScatterColor(
        im: gdImagePtr,
        sub: c_int,
        plus: c_int,
        colors: *mut c_int,
        n: c_uint,
    ) -> c_int;
    pub fn gdImageSmooth(im: gdImagePtr, weight: c_float) -> c_int;
    pub fn gdImageMeanRemoval(im: gdImagePtr) -> c_int;
    pub fn gdImageEmboss(im: gdImagePtr) -> c_int;
    pub fn gdImageGaussianBlur(im: gdImagePtr) -> c_int;
    pub fn gdImageEdgeDetectQuick(im: gdImagePtr) -> c_int;
    pub fn gdImageSelectiveBlur(im: gdImagePtr) -> c_int;
    pub fn gdImageConvolution(
        im: gdImagePtr,
        filter: *mut [c_float; 3],
        div: c_float,
        offset: c_float,
    ) -> c_int;
    pub fn gdImageColor(im: gdImagePtr, r: c_int, g: c_int, b: c_int, a: c_int) -> c_int;
    pub fn gdImageContrast(im: gdImagePtr, contrast: c_double) -> c_int;
    pub fn gdImageBrightness(im: gdImagePtr, brightness: c_int) -> c_int;
    pub fn gdImageGrayScale(im: gdImagePtr) -> c_int;
    pub fn gdImageNegate(im: gdImagePtr) -> c_int;

    pub fn gdImageFlipHorizontal(im: gdImagePtr);
    pub fn gdImageFlipVertical(im: gdImagePtr);
    pub fn gdImageFlipBoth(im: gdImagePtr);
    pub fn gdImageCrop(im: gdImagePtr, crop: *const gdRect) -> gdImagePtr;
    pub fn gdImageCropAuto(im: gdImagePtr, mode: c_uint) -> gdImagePtr;
    pub fn gdImageCropThreshold(im: gdImagePtr, color: c_uint, threshold: c_float) -> gdImagePtr;
    pub fn gdImageSetInterpolationMethod(im: gdImagePtr, id: c_int) -> c_int;
    pub fn gdImageScale(im: gdImagePtr, w: c_uint, h: c_uint) -> gdImagePtr;
    pub fn gdImageRotateInterpolated(im: gdImagePtr, angle: c_float, bg: c_int) -> gdImagePtr;
    pub fn gdAffineConcat(dst: *mut c_double, m1: *const c_double, m2: *const c_double) -> c_int;
    pub fn gdAffineScale(dst: *mut c_double, x: c_double, y: c_double) -> c_int;
    pub fn gdAffineRotate(dst: *mut c_double, angle: c_double) -> c_int;
    pub fn gdAffineShearHorizontal(dst: *mut c_double, angle: c_double) -> c_int;
    pub fn gdAffineShearVertical(dst: *mut c_double, angle: c_double) -> c_int;
    pub fn gdAffineTranslate(dst: *mut c_double, x: c_double, y: c_double) -> c_int;
    pub fn gdTransformAffineGetImage(
        dst: *mut gdImagePtr,
        src: gdImagePtr,
        area: *mut gdRect,
        affine: *const c_double,
    ) -> c_int;

    pub fn gdFontGetTiny() -> *mut gdFont;
    pub fn gdFontGetSmall() -> *mut gdFont;
    pub fn gdFontGetMediumBold() -> *mut gdFont;
    pub fn gdFontGetLarge() -> *mut gdFont;
    pub fn gdFontGetGiant() -> *mut gdFont;
}

extern "C" {
    /// C `vsnprintf`; `ap` is the `va_list` handed to the error method
    /// (see [`super::errors`]).
    pub fn vsnprintf(buf: *mut c_char, n: usize, fmt: *const c_char, ap: *mut c_void) -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// Offsets printed by a C program compiled against `/usr/include/gd.h`.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn layout_matches_gd_h() {
        assert_eq!(size_of::<gdImage>(), 7336);
        assert_eq!(offset_of!(gdImage, sx), 8);
        assert_eq!(offset_of!(gdImage, transparent), 4116);
        assert_eq!(offset_of!(gdImage, brush), 4136);
        assert_eq!(offset_of!(gdImage, style), 6208);
        assert_eq!(offset_of!(gdImage, alpha), 6224);
        assert_eq!(offset_of!(gdImage, trueColor), 7248);
        assert_eq!(offset_of!(gdImage, cx1), 7284);
        assert_eq!(offset_of!(gdImage, res_x), 7300);
        assert_eq!(offset_of!(gdImage, interpolation_id), 7324);
        assert_eq!(offset_of!(gdImage, interpolation), 7328);
        assert_eq!(size_of::<gdFTStringExtra>(), 48);
        assert_eq!(size_of::<gdIOCtx>(), 64);
    }
}
