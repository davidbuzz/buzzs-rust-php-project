//! Image creation, loading and saving: `imagecreate*()`, `imagecreatefrom*()`,
//! the output functions, `imageloadfont()`, `gd_info()`, `imagetypes()`.

use php_ext_streams::{bridge, res};
use php_gd::font::{self, HEADER_SIZE};
use php_gd::formats::{self, ImageType};
use php_gd::info::{self, InfoValue};
use php_gd::{filters, Decoder, Encoder, Image};
use php_runtime::{Value, ZArray};
use php_streams::wrapper::REPORT_ERRORS;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::io::{self, Opened, Sink};
use crate::objects::{image_arg, image_or_false, new_font, new_image};
use crate::util::{ci, flush, warn, INT_MAX, INT_MIN};

/// `gd_info()`.
#[allow(clippy::unnecessary_wraps)]
fn gd_info(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(ZArray::from_pairs(
        info::gd_info().into_iter().map(|(k, v)| {
            (
                k,
                match v {
                    InfoValue::Str(s) => Value::string(s),
                    InfoValue::Bool(b) => Value::Bool(b),
                },
            )
        }),
    )))
}

/// `imagetypes()`.
#[allow(clippy::unnecessary_wraps)]
fn imagetypes(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(info::image_types()))
}

/// The size checks of `imagecreate()` / `imagecreatetruecolor()`.
fn size_args(vm: &mut Vm, a: &Args) -> VmResult<(i32, i32)> {
    let (x, y) = (a.int(0), a.int(1));
    if x <= 0 || x >= INT_MAX {
        return Err(a.value_error(vm, 1, "must be greater than 0"));
    }
    if y <= 0 || y >= INT_MAX {
        return Err(a.value_error(vm, 2, "must be greater than 0"));
    }
    Ok((ci(x), ci(y)))
}

fn imagecreatetruecolor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (x, y) = size_args(vm, a)?;
    let im = Image::new_truecolor(x, y);
    flush(vm)?;
    image_or_false(vm, im)
}

fn imagecreate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (x, y) = size_args(vm, a)?;
    let im = Image::new_palette(x, y);
    flush(vm)?;
    image_or_false(vm, im)
}

/// `_php_image_create_from_string()`.
fn from_string(vm: &mut Vm, data: &[u8], t: ImageType, dec: Decoder) -> VmResult<Option<Image>> {
    let im = Image::decode(dec, data);
    if t == ImageType::Gd2 {
        let _ = php_gd::errors::take();
    } else {
        flush(vm)?;
    }
    if im.is_none() {
        warn(
            vm,
            format!("Passed data is not in \"{}\" format", t.name()).as_bytes(),
        )?;
    }
    Ok(im)
}

fn jpeg_ignore_warning(vm: &Vm) -> bool {
    vm.ini_bool("gd.jpeg_ignore_warning")
}

fn imagecreatefromstring(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = a.str(0);
    let data = data.as_bytes();
    if i64::try_from(data.len()).map_or(true, |l| l > INT_MAX) {
        return Err(a.value_error(vm, 1, "is too long"));
    }
    let Some(t) = formats::image_type(data) else {
        warn(vm, b"Data is not in a recognized format")?;
        return Ok(Value::Bool(false));
    };
    let dec = match t {
        // `gdImageCreateFromJpegCtx()` ignores libjpeg warnings.
        ImageType::Jpeg => Decoder::Jpeg(true),
        ImageType::Png => Decoder::Png,
        ImageType::Gif => Decoder::Gif,
        ImageType::Wbmp => Decoder::Wbmp,
        ImageType::Gd2 => Decoder::Gd2,
        ImageType::Bmp => Decoder::Bmp,
        ImageType::Webp => Decoder::Webp,
        ImageType::Avif => {
            warn(vm, b"No AVIF support in this PHP build")?;
            return Ok(Value::Bool(false));
        }
    };
    let im = from_string(vm, data, t, dec)?;
    if let Some(im) = im {
        return new_image(vm, im);
    }
    warn(vm, b"Couldn't create GD Image Stream out of Data")?;
    Ok(Value::Bool(false))
}

/// The formats of `_php_image_create_from()`.
#[derive(Clone, Copy)]
enum From {
    Gif,
    Jpeg,
    Png,
    Webp,
    Xbm,
    Xpm,
    Wbmp,
    Gd,
    Gd2,
    Gd2Part,
    Bmp,
    Tga,
}

impl From {
    fn name(self) -> &'static str {
        match self {
            From::Gif => "GIF",
            From::Jpeg => "JPEG",
            From::Png => "PNG",
            From::Webp => "WEBP",
            From::Xbm => "XBM",
            From::Xpm => "XPM",
            From::Wbmp => "WBMP",
            From::Gd => "GD",
            From::Gd2 | From::Gd2Part => "GD2",
            From::Bmp => "BMP",
            From::Tga => "TGA",
        }
    }
}

/// `_php_image_create_from()`.
fn create_from(vm: &mut Vm, a: &Args, kind: From) -> VmResult<Value> {
    let path = res::path_arg(vm, a, 0)?;
    let mut part = (0, 0, 0, 0);
    if let From::Gd2Part = kind {
        let (w, h) = (a.int(3), a.int(4));
        if w < 1 {
            return Err(a.value_error(vm, 4, "must be greater than or equal to 1"));
        }
        if h < 1 {
            return Err(a.value_error(vm, 5, "must be greater than or equal to 1"));
        }
        part = (ci(a.int(1)), ci(a.int(2)), ci(w), ci(h));
    }
    let (data, stdio) = match io::read_path(vm, &path)? {
        Opened::Failed => return Ok(Value::Bool(false)),
        Opened::File(d) => (d, true),
        Opened::Other(d) => (d, false),
    };
    if !stdio && matches!(kind, From::Xbm | From::Xpm) {
        // Neither has an IO-context reader; only FILE*-castable streams work.
        let mut msg = b"\"".to_vec();
        msg.extend_from_slice(&path);
        msg.extend_from_slice(format!("\" is not a valid {} file", kind.name()).as_bytes());
        warn(vm, &msg)?;
        return Ok(Value::Bool(false));
    }
    if !stdio && i64::try_from(data.len()).map_or(true, |l| l > INT_MAX) {
        warn(
            vm,
            format!("Cannot read images with more than {INT_MAX} bytes").as_bytes(),
        )?;
        return Ok(Value::Bool(false));
    }
    let im = match kind {
        From::Xpm => Image::decode_xpm_file(&path),
        From::Jpeg => Image::decode(Decoder::Jpeg(jpeg_ignore_warning(vm)), &data),
        From::Gd2Part => Image::decode(Decoder::Gd2Part(part.0, part.1, part.2, part.3), &data),
        From::Gif => Image::decode(Decoder::Gif, &data),
        From::Png => Image::decode(Decoder::Png, &data),
        From::Webp => Image::decode(Decoder::Webp, &data),
        From::Xbm => Image::decode(Decoder::Xbm, &data),
        From::Wbmp => Image::decode(Decoder::Wbmp, &data),
        From::Gd => Image::decode(Decoder::Gd, &data),
        From::Gd2 => Image::decode(Decoder::Gd2, &data),
        From::Bmp => Image::decode(Decoder::Bmp, &data),
        From::Tga => Image::decode(Decoder::Tga, &data),
    };
    flush(vm)?;
    if let Some(im) = im {
        return new_image(vm, im);
    }
    let mut msg = b"\"".to_vec();
    msg.extend_from_slice(&path);
    msg.extend_from_slice(format!("\" is not a valid {} file", kind.name()).as_bytes());
    warn(vm, &msg)?;
    Ok(Value::Bool(false))
}

macro_rules! create_from_fn {
    ($($name:ident => $kind:ident),* $(,)?) => {
        $(
            fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
                create_from(vm, a, From::$kind)
            }
        )*
    };
}

create_from_fn!(
    imagecreatefromgif => Gif,
    imagecreatefromjpeg => Jpeg,
    imagecreatefrompng => Png,
    imagecreatefromwebp => Webp,
    imagecreatefromxbm => Xbm,
    imagecreatefromxpm => Xpm,
    imagecreatefromwbmp => Wbmp,
    imagecreatefromgd => Gd,
    imagecreatefromgd2 => Gd2,
    imagecreatefromgd2part => Gd2Part,
    imagecreatefrombmp => Bmp,
    imagecreatefromtga => Tga,
);

/// Encodes into a buffer, raising libgd's messages.
fn encode(vm: &mut Vm, im: &Image, enc: &Encoder) -> VmResult<Vec<u8>> {
    let mut buf = Vec::new();
    let _ = im.encode(enc, &mut buf);
    flush(vm)?;
    Ok(buf)
}

/// Common tail of the context-based output functions.
fn emit(vm: &mut Vm, sink: Sink, im: &Image, enc: &Encoder) -> VmResult<Value> {
    let buf = encode(vm, im, enc)?;
    sink.finish(vm, &buf)?;
    Ok(Value::Bool(true))
}

fn imagegif(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let Some(sink) = io::output_sink(vm, a, 1)? else {
        return Ok(Value::Bool(false));
    };
    emit(vm, sink, &im, &Encoder::Gif)
}

fn imagepng(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let Some(sink) = io::output_sink(vm, a, 1)? else {
        return Ok(Value::Bool(false));
    };
    let quality = a.int(2);
    if !(-1..=9).contains(&quality) {
        sink.discard(vm)?;
        return Err(a.value_error(vm, 3, "must be between -1 and 9"));
    }
    emit(vm, sink, &im, &Encoder::Png(ci(quality)))
}

fn imagewebp(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let Some(sink) = io::output_sink(vm, a, 1)? else {
        return Ok(Value::Bool(false));
    };
    let quality = a.int(2);
    if quality < -1 {
        sink.discard(vm)?;
        return Err(a.value_error(vm, 3, "must be greater than or equal to -1"));
    }
    emit(vm, sink, &im, &Encoder::Webp(ci(quality)))
}

fn imagejpeg(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let Some(sink) = io::output_sink(vm, a, 1)? else {
        return Ok(Value::Bool(false));
    };
    let quality = a.int(2);
    if !(-1..=100).contains(&quality) {
        sink.discard(vm)?;
        return Err(a.value_error(vm, 3, "must be at between -1 and 100"));
    }
    emit(vm, sink, &im, &Encoder::Jpeg(ci(quality)))
}

fn imagewbmp(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let Some(sink) = io::output_sink(vm, a, 1)? else {
        return Ok(Value::Bool(false));
    };
    let fg = match a.opt(2) {
        Some(_) => ci(a.int(2)),
        None => filters::first_black(&im),
    };
    emit(vm, sink, &im, &Encoder::Wbmp(fg))
}

fn imagebmp(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let Some(sink) = io::output_sink(vm, a, 1)? else {
        return Ok(Value::Bool(false));
    };
    let compressed = a.bool(2);
    emit(vm, sink, &im, &Encoder::Bmp(compressed))
}

fn imagexbm(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let file = match a.opt(1) {
        Some(_) => Some(res::path_arg(vm, a, 1)?),
        None => None,
    };
    let sink = match &file {
        Some(path) => match io::open_for_writing(vm, path)? {
            Some(s) => s,
            None => return Ok(Value::Bool(false)),
        },
        None => Sink::Output,
    };
    let fg = match a.opt(2) {
        Some(_) => ci(a.int(2)),
        None => filters::first_black(&im),
    };
    emit(vm, sink, &im, &Encoder::Xbm(file.unwrap_or_default(), fg))
}

/// `_php_image_output()`: `imagegd()` / `imagegd2()`, written with stdio.
fn gd_output(vm: &mut Vm, a: &Args, gd2: bool) -> VmResult<Value> {
    let im = image_arg(vm, a, 0)?;
    let file = match a.opt(1) {
        Some(_) => Some(res::path_arg(vm, a, 1)?),
        None => None,
    };
    let (mut quality, kind) = if gd2 { (a.int(2), a.int(3)) } else { (128, 1) };
    if !(INT_MIN..=INT_MAX).contains(&quality) {
        warn(
            vm,
            format!("Argument #3 ($chunk_size) must be between {INT_MIN} and {INT_MAX}").as_bytes(),
        )?;
        return Ok(Value::Bool(false));
    }
    if quality == -1 {
        quality = 128;
    }
    let enc = if gd2 {
        Encoder::Gd2(ci(quality), ci(kind))
    } else {
        Encoder::Gd
    };
    if let Some(path) = file.filter(|f| !f.is_empty()) {
        {
            if !bridge::run(vm, || php_streams::paths::check_open_basedir(&path))? {
                warn(vm, b"Invalid filename")?;
                return Ok(Value::Bool(false));
            }
            let f = {
                use std::os::unix::ffi::OsStrExt;
                std::fs::File::create(std::ffi::OsStr::from_bytes(&path))
            };
            let Ok(mut f) = f else {
                let mut msg = b"Unable to open \"".to_vec();
                msg.extend_from_slice(&path);
                msg.extend_from_slice(b"\" for writing");
                warn(vm, &msg)?;
                return Ok(Value::Bool(false));
            };
            let buf = encode(vm, &im, &enc)?;
            let _ = std::io::Write::write_all(&mut f, &buf);
        }
    } else {
        let buf = encode(vm, &im, &enc)?;
        Sink::Output.finish(vm, &buf)?;
    }
    Ok(Value::Bool(true))
}

fn imagegd(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    gd_output(vm, a, false)
}

fn imagegd2(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    gd_output(vm, a, true)
}

fn imagedestroy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    image_arg(vm, a, 0)?;
    Ok(Value::Bool(true))
}

/// The outcome of reading a font file.
enum FontRead {
    Failed,
    Warn(&'static str),
    Header(font::HeaderError),
    Ok(font::FontHeader, Vec<u8>),
}

/// Reads until `buf` is full or a read returns nothing; the last count.
fn read_full(s: &mut php_streams::stream::Stream, buf: &mut [u8]) -> (usize, bool) {
    let mut b = 0;
    let mut last_ok = true;
    while b < buf.len() {
        match s.read(&mut buf[b..]) {
            Ok(n) if n > 0 => b += n,
            _ => {
                last_ok = false;
                break;
            }
        }
    }
    (b, last_ok)
}

fn imageloadfont(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let path = res::path_arg(vm, a, 0)?;
    io::check_not_empty(vm, &path)?;
    let out = bridge::run(vm, || {
        let mut opened = None;
        let Some(mut s) =
            php_streams::open::open_wrapper(&path, "rb", REPORT_ERRORS, &mut opened, None)
        else {
            return FontRead::Failed;
        };
        let mut hdr = [0u8; HEADER_SIZE];
        let (_, ok) = read_full(&mut s, &mut hdr);
        if !ok {
            let msg = if s.eof() {
                "End of file while reading header"
            } else {
                "Error while reading header"
            };
            s.close();
            return FontRead::Warn(msg);
        }
        let pos = s.tell();
        s.seek(0, 2);
        let check = s.tell() - i64::try_from(HEADER_SIZE).unwrap_or(16);
        s.seek(pos, 0);
        let h = match font::parse_header(&hdr, check) {
            Ok(h) => h,
            Err(e) => {
                s.close();
                return FontRead::Header(e);
            }
        };
        let mut body = vec![0u8; h.body_size()];
        let (_, ok) = read_full(&mut s, &mut body);
        if !ok {
            let msg = if s.eof() {
                "End of file while reading body"
            } else {
                "Error while reading body"
            };
            s.close();
            return FontRead::Warn(msg);
        }
        s.close();
        FontRead::Ok(h, body)
    })?;
    flush(vm)?;
    match out {
        FontRead::Failed => Ok(Value::Bool(false)),
        FontRead::Warn(m) => {
            vm.php_warning(m)?;
            Ok(Value::Bool(false))
        }
        FontRead::Header(e) => {
            vm.php_warning(e.message())?;
            Ok(Value::Bool(false))
        }
        FontRead::Ok(h, body) => new_font(vm, font::font_from(h, body)),
    }
}

/// Registers the functions of this module.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function_static("gd_info(): array", gd_info)
        .function_static("imageloadfont(string $filename): GdFont|false", imageloadfont)
        .function_static(
            "imagecreatetruecolor(int $width, int $height): GdImage|false",
            imagecreatetruecolor,
        )
        .function_static("imagecreate(int $width, int $height): GdImage|false", imagecreate)
        .function_static("imagetypes(): int", imagetypes)
        .function_static(
            "imagecreatefromstring(string $data): GdImage|false",
            imagecreatefromstring,
        )
        .function_static("imagecreatefromgif(string $filename): GdImage|false", imagecreatefromgif)
        .function_static("imagecreatefromjpeg(string $filename): GdImage|false", imagecreatefromjpeg)
        .function_static("imagecreatefrompng(string $filename): GdImage|false", imagecreatefrompng)
        .function_static("imagecreatefromwebp(string $filename): GdImage|false", imagecreatefromwebp)
        .function_static("imagecreatefromxbm(string $filename): GdImage|false", imagecreatefromxbm)
        .function_static("imagecreatefromxpm(string $filename): GdImage|false", imagecreatefromxpm)
        .function_static("imagecreatefromwbmp(string $filename): GdImage|false", imagecreatefromwbmp)
        .function_static("imagecreatefromgd(string $filename): GdImage|false", imagecreatefromgd)
        .function_static("imagecreatefromgd2(string $filename): GdImage|false", imagecreatefromgd2)
        .function_static(
            "imagecreatefromgd2part(string $filename, int $x, int $y, int $width, int $height): GdImage|false",
            imagecreatefromgd2part,
        )
        .function_static("imagecreatefrombmp(string $filename): GdImage|false", imagecreatefrombmp)
        .function_static("imagecreatefromtga(string $filename): GdImage|false", imagecreatefromtga)
        .function_static(
            "imagexbm(GdImage $image, ?string $filename, ?int $foreground_color = null): bool",
            imagexbm,
        )
        .function_static("imagegif(GdImage $image, $file = null): bool", imagegif)
        .function_static(
            "imagepng(GdImage $image, $file = null, int $quality = -1, int $filters = -1): bool",
            imagepng,
        )
        .function_static("imagewebp(GdImage $image, $file = null, int $quality = -1): bool", imagewebp)
        .function_static("imagejpeg(GdImage $image, $file = null, int $quality = -1): bool", imagejpeg)
        .function_static(
            "imagewbmp(GdImage $image, $file = null, ?int $foreground_color = null): bool",
            imagewbmp,
        )
        .function_static("imagegd(GdImage $image, ?string $file = null): bool", imagegd)
        .function_static(
            "imagegd2(GdImage $image, ?string $file = null, int $chunk_size = 128, int $mode = IMG_GD2_RAW): bool",
            imagegd2,
        )
        .function_static(
            "imagebmp(GdImage $image, $file = null, bool $compressed = true): bool",
            imagebmp,
        )
        .function_static("imagedestroy(GdImage $image): true", imagedestroy)
        .deprecated("8.5", "as it has no effect since PHP 8.0");
}
