//! The image handler table of `ext/standard/image.c` (`php_image.h`):
//! formats contributed by other extensions at module startup through
//! `php_image_register_handler()` (php-src master registers SVG from
//! `ext/libxml`).
//!
//! A registered handler gets the next type id after the built-in
//! `IMAGE_FILETYPE_FIXED_COUNT` ones. `php_getimagetype()` asks every
//! handler, in registration order, after all built-in checks failed;
//! `getimagesize()`, `image_type_to_mime_type()` and
//! `image_type_to_extension()` consult the handler of a non-built-in type.
//!
//! The table is process-wide (C keeps it in a persistent hash); registering
//! the same constant name again returns the id it already has, so several
//! VMs in one process share the ids.

use std::io::{Read, Seek};
use std::sync::{PoisonError, RwLock};

use crate::imagesize::GfxInfo;
use crate::imagetype::IMAGETYPE_COUNT;
use crate::stream::PhpStream;

/// The `php_stream` operations a handler may use on the image data.
pub trait ImageInput {
    /// `php_stream_rewind()`; `true` on success.
    fn rewind(&mut self) -> bool;
    /// `php_stream_read()` looped until `buf` is full or the data ends;
    /// the number of bytes read.
    fn read(&mut self, buf: &mut [u8]) -> usize;
    /// `php_stream_getc()`: one byte, `None` at end of data.
    fn getc(&mut self) -> Option<u8>;

    /// Reads everything from the current position to the end.
    fn read_rest(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = self.read(&mut buf);
            out.extend_from_slice(&buf[..n]);
            if n < buf.len() {
                return out;
            }
        }
    }
}

impl<R: Read + Seek> ImageInput for PhpStream<R> {
    fn rewind(&mut self) -> bool {
        PhpStream::rewind(self)
    }

    fn read(&mut self, buf: &mut [u8]) -> usize {
        PhpStream::read(self, buf)
    }

    fn getc(&mut self) -> Option<u8> {
        PhpStream::getc(self)
    }
}

/// `struct php_image_handler`.
#[derive(Debug)]
pub struct ImageHandler {
    /// The MIME type (`image_type_to_mime_type()`, `getimagesize()['mime']`).
    pub mime_type: &'static str,
    /// The extension including its dot (`image_type_to_extension()`).
    pub extension: &'static str,
    /// The constant naming the type id (`PHP_IMAGE_CONST_NAME("SVG")`,
    /// i.e. `"IMAGETYPE_SVG"`).
    pub const_name: &'static str,
    /// `php_image_identify`: whether the data is of this type. The stream
    /// position is wherever the built-in checks left it.
    pub identify: fn(&mut dyn ImageInput) -> bool,
    /// `php_image_get_info`: the size information, `None` on failure.
    pub get_info: fn(&mut dyn ImageInput) -> Option<GfxInfo>,
}

static HANDLERS: RwLock<Vec<&'static ImageHandler>> = RwLock::new(Vec::new());

/// `php_image_register_handler()`: adds `handler` to the table and returns
/// its type id (the value of its constant). A handler whose constant name
/// is already registered keeps its id.
pub fn register_handler(handler: &'static ImageHandler) -> i64 {
    let mut table = HANDLERS.write().unwrap_or_else(PoisonError::into_inner);
    let existing = table
        .iter()
        .position(|h| h.const_name == handler.const_name);
    let index = if let Some(i) = existing {
        table[i] = handler;
        i
    } else {
        table.push(handler);
        table.len() - 1
    };
    id_of(index)
}

fn id_of(index: usize) -> i64 {
    IMAGETYPE_COUNT + i64::try_from(index).unwrap_or(i64::MAX - IMAGETYPE_COUNT)
}

/// The handler registered for `image_type`, if any.
pub fn handler(image_type: i64) -> Option<&'static ImageHandler> {
    let index = usize::try_from(image_type.checked_sub(IMAGETYPE_COUNT)?).ok()?;
    HANDLERS
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get(index)
        .copied()
}

/// Every registered handler with its type id, in registration order.
pub fn handlers() -> Vec<(i64, &'static ImageHandler)> {
    HANDLERS
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .enumerate()
        .map(|(i, h)| (id_of(i), *h))
        .collect()
}

/// The value of `IMAGETYPE_COUNT`: the built-in types plus one per
/// registered handler.
pub fn image_type_count() -> i64 {
    id_of(
        HANDLERS
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len(),
    )
}

/// The registered handlers' turn in `php_getimagetype()`: the id of the
/// first one that identifies the data.
pub(crate) fn identify(stream: &mut dyn ImageInput) -> Option<i64> {
    handlers()
        .into_iter()
        .find(|(_, h)| (h.identify)(stream))
        .map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imagesize::getimagesizefromstring;
    use crate::imagetype::{exif_imagetype, image_type_to_extension, image_type_to_mime_type};
    use php_runtime::{ArrayKey, Value};

    fn is_test(s: &mut dyn ImageInput) -> bool {
        s.rewind() && s.read_rest().starts_with(b"TESTIMG:")
    }

    fn test_info(s: &mut dyn ImageInput) -> Option<GfxInfo> {
        if !is_test(s) || !s.rewind() {
            return None;
        }
        let rest = s.read_rest();
        // Built-in detection needs 12 bytes: the unit is space padded.
        let unit = rest
            .get(8..)
            .map(<[u8]>::trim_ascii)
            .filter(|u| !u.is_empty());
        Some(GfxInfo {
            width: 3,
            height: 4,
            width_unit: unit.map(|u| String::from_utf8_lossy(u).into_owned()),
            ..GfxInfo::default()
        })
    }

    static TEST: ImageHandler = ImageHandler {
        mime_type: "image/x-test",
        extension: ".tst",
        const_name: "IMAGETYPE_PHPRUST_TEST",
        identify: is_test,
        get_info: test_info,
    };

    #[test]
    fn registered_handlers_extend_the_type_functions() {
        let id = register_handler(&TEST);
        assert!(id >= IMAGETYPE_COUNT);
        assert_eq!(register_handler(&TEST), id);
        assert!(image_type_count() > id);
        assert!(handler(id).is_some_and(|h| h.const_name == TEST.const_name));
        assert!(handler(IMAGETYPE_COUNT - 1).is_none());
        assert!(handler(-1).is_none());
        assert_eq!(image_type_to_mime_type(id), "image/x-test");
        assert_eq!(image_type_to_extension(id, true), Some(".tst"));
        assert_eq!(image_type_to_extension(id, false), Some("tst"));

        let (t, diags) = exif_imagetype(std::io::Cursor::new(b"TESTIMG:    "), b"x");
        assert_eq!(t, Some(id));
        assert!(diags.is_empty());

        let field = |data: &[u8], k: ArrayKey| -> Option<Vec<u8>> {
            let Value::Array(a) = getimagesizefromstring(data, false).to_value() else {
                panic!("expected an array");
            };
            match a.get(&k)? {
                Value::Int(i) => Some(i.to_string().into_bytes()),
                Value::Str(s) => Some(s.as_bytes().to_vec()),
                _ => None,
            }
        };
        let name = |k: &str| ArrayKey::from_bytes(k.as_bytes());
        let px = b"TESTIMG:    ".as_slice();
        assert_eq!(
            field(px, ArrayKey::Int(2)),
            Some(id.to_string().into_bytes())
        );
        assert_eq!(
            field(px, ArrayKey::Int(3)).as_deref(),
            Some(b"width=\"3\" height=\"4\"".as_slice())
        );
        assert_eq!(
            field(px, name("mime")).as_deref(),
            Some(b"image/x-test".as_slice())
        );
        assert_eq!(
            field(px, name("width_unit")).as_deref(),
            Some(b"px".as_slice())
        );

        // A unit other than px drops the HTML attribute string.
        let cm = b"TESTIMG:cm  ".as_slice();
        assert_eq!(field(cm, ArrayKey::Int(3)), None);
        assert_eq!(
            field(cm, name("width_unit")).as_deref(),
            Some(b"cm".as_slice())
        );
        assert_eq!(
            field(cm, name("height_unit")).as_deref(),
            Some(b"px".as_slice())
        );

        // Data no handler knows stays unknown.
        assert_eq!(
            exif_imagetype(std::io::Cursor::new(b"NOTANIMAGE!!"), b"x").0,
            None
        );
    }
}
