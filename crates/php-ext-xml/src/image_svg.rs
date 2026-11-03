//! `ext/libxml/image_svg.c`: the SVG image type (`IMAGETYPE_SVG`) that
//! `ext/libxml` adds to `getimagesize()` / `image_type_to_*()` through the
//! image handler table of `php-ext-exif`.
//!
//! A document is SVG when its root element's local name is `svg` (any case)
//! and it carries `width` and `height` attributes of the form
//! `[0-9]+[a-zA-Z]*`; the number is the size and the letters its unit.

use php_ext_exif::{GfxInfo, ImageHandler, ImageInput};
use php_xmlreader::{PropValue, Property, Source, XmlReader};

/// `XML_PARSE_NOERROR | XML_PARSE_NOWARNING | XML_PARSE_NONET`.
const PARSE_FLAGS: i64 = (1 << 5) | (1 << 6) | (1 << 11);
/// `XML_READER_TYPE_ELEMENT`.
const READER_TYPE_ELEMENT: i64 = 1;

/// `svg_image_handler`.
pub(crate) static HANDLER: ImageHandler = ImageHandler {
    mime_type: "image/svg+xml",
    extension: ".svg",
    const_name: "IMAGETYPE_SVG",
    identify,
    get_info,
};

/// `php_libxml_svg_image_identify`.
fn identify(stream: &mut dyn ImageInput) -> bool {
    handle(stream).is_some()
}

/// `php_libxml_svg_image_get_info`.
fn get_info(stream: &mut dyn ImageInput) -> Option<GfxInfo> {
    handle(stream)
}

/// `php_libxml_parse_dimension`: splits `[0-9]+[a-zA-Z]*` into the number
/// (`ZEND_STRTOL` stored in an `unsigned int`) and the unit (`None` when
/// empty); `None` for anything else.
fn parse_dimension(input: &[u8]) -> Option<(u32, Option<String>)> {
    let digits = input.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let (number, unit) = input.split_at(digits);
    if !unit.iter().all(u8::is_ascii_alphabetic) {
        return None;
    }
    // `strtol` saturates at `LONG_MAX`; the assignment keeps the low bits.
    let value = number.iter().fold(0i64, |acc, d| {
        acc.saturating_mul(10).saturating_add(i64::from(d - b'0'))
    });
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let value = value as u32;
    let unit = (!unit.is_empty()).then(|| String::from_utf8_lossy(unit).into_owned());
    Some((value, unit))
}

/// `php_libxml_svg_image_handle`: reads up to the first element with
/// libxml's pull parser.
fn handle(stream: &mut dyn ImageInput) -> Option<GfxInfo> {
    // Early check before doing more expensive work.
    if !stream.rewind() || stream.getc() != Some(b'<') || !stream.rewind() {
        return None;
    }
    // `xmlReaderForIO` over the stream; the reader needs an owned input.
    let data = std::io::Cursor::new(stream.read_rest());
    let mut reader = XmlReader::new();
    reader.attach(Source::from_stream(Box::new(data), None, PARSE_FLAGS, None).ok()?);
    while reader.read().ok()? {
        let Ok(PropValue::Int(READER_TYPE_ELEMENT)) = reader.read_property(Property::NodeType)
        else {
            continue;
        };
        // The root must be an svg element.
        let Ok(PropValue::Str(name)) = reader.read_property(Property::LocalName) else {
            return None;
        };
        if !name.eq_ignore_ascii_case(b"svg") {
            return None;
        }
        let width = reader.get_attribute(b"width").ok()??;
        let height = reader.get_attribute(b"height").ok()??;
        let (width, width_unit) = parse_dimension(&width)?;
        let (height, height_unit) = parse_dimension(&height)?;
        return Some(GfxInfo {
            width,
            height,
            width_unit,
            height_unit,
            ..GfxInfo::default()
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions() {
        assert_eq!(parse_dimension(b"12"), Some((12, None)));
        assert_eq!(parse_dimension(b"4cm"), Some((4, Some("cm".into()))));
        assert_eq!(parse_dimension(b"cm"), None);
        assert_eq!(parse_dimension(b""), None);
        assert_eq!(parse_dimension(b"1.5"), None);
        assert_eq!(parse_dimension(b"1cm2"), None);
        assert_eq!(parse_dimension("é".as_bytes()), None);
        assert_eq!(parse_dimension(b"4294967297"), Some((1, None)));
    }

    #[test]
    fn svg_root_with_size_is_identified() {
        let id = php_exif::handlers::register_handler(&HANDLER);
        let size = |data: &[u8]| php_exif::getimagesizefromstring(data, false);
        let out = size(b"<x:svg width='4cm' height=\"8\" xmlns:x=\"http://www.w3.org/2000/svg\">");
        assert_eq!(out.image_type, id);
        let info = out.info.expect("svg info");
        assert_eq!((info.width, info.height), (4, 8));
        assert_eq!(info.width_unit.as_deref(), Some("cm"));
        assert_eq!(info.height_unit, None);
        assert!(size(b"<foo width='1' height='1'/>").info.is_none());
        assert!(size(b"<svg width='1'/>                 ").info.is_none());
        assert!(size(b"<?xml version=\"1.0\"?>").info.is_none());
    }
}
