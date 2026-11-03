//! Image type detection for `imagecreatefromstring()` (`_php_image_type()`
//! in gd.c, after ext/standard/image.c).

/// The formats `imagecreatefromstring()` recognises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageType {
    /// `gd2` signature.
    Gd2,
    /// JPEG (`FF D8 FF`).
    Jpeg,
    /// PNG.
    Png,
    /// GIF.
    Gif,
    /// BMP (`BM`).
    Bmp,
    /// WebP (`RIFF....WEBP`).
    Webp,
    /// AVIF (an `ftyp` box with an `avif`/`avis` brand).
    Avif,
    /// WBMP (first byte 0).
    Wbmp,
}

impl ImageType {
    /// The name used in `Passed data is not in "%s" format`.
    pub fn name(self) -> &'static str {
        match self {
            ImageType::Gd2 => "GD2",
            ImageType::Jpeg => "JPEG",
            ImageType::Png => "PNG",
            ImageType::Gif => "GIF",
            ImageType::Bmp => "BMP",
            ImageType::Webp => "WEBP",
            ImageType::Avif => "AVIF",
            ImageType::Wbmp => "WBMP",
        }
    }
}

const SIG_PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// `_php_image_type()`: `None` for unknown data (and anything shorter than
/// 12 bytes).
pub fn image_type(data: &[u8]) -> Option<ImageType> {
    if data.len() < 12 {
        return None;
    }
    if data.starts_with(b"gd2") {
        Some(ImageType::Gd2)
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(ImageType::Jpeg)
    } else if data.starts_with(&SIG_PNG) {
        Some(ImageType::Png)
    } else if data.starts_with(b"GIF") {
        Some(ImageType::Gif)
    } else if data.starts_with(b"BM") {
        Some(ImageType::Bmp)
    } else if data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        Some(ImageType::Webp)
    } else if is_avif(data) {
        Some(ImageType::Avif)
    } else if data[0] == 0 {
        Some(ImageType::Wbmp)
    } else {
        None
    }
}

/// libavifinfo's `AvifInfoIdentify()`: the first box must be an `ftyp`
/// box listing `avif` or `avis` as its major or a compatible brand.
pub fn is_avif(data: &[u8]) -> bool {
    let be32 = |o: usize| -> Option<u32> {
        data.get(o..o + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let Some(size32) = be32(0) else {
        return false;
    };
    if data.get(4..8) != Some(b"ftyp".as_slice()) {
        return false;
    }
    let (header, size) = match size32 {
        1 => {
            let Some(hi) = be32(8) else { return false };
            let Some(lo) = be32(12) else { return false };
            (16u64, (u64::from(hi) << 32) | u64::from(lo))
        }
        // "box extends to the end of the file": not allowed for ftyp.
        0 => return false,
        s => (8u64, u64::from(s)),
    };
    if size < header {
        return false;
    }
    let content = size - header;
    if content < 8 {
        return false;
    }
    let start = usize::try_from(header).unwrap_or(usize::MAX);
    let mut i: u64 = 0;
    while i + 4 <= content {
        let Some(off) = usize::try_from(i).ok().and_then(|i| start.checked_add(i)) else {
            return false;
        };
        let Some(brand) = data.get(off..off + 4) else {
            return false;
        };
        if i != 4 && (brand == b"avif" || brand == b"avis") {
            return true;
        }
        if i > 32 * 4 {
            return false;
        }
        i += 4;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures() {
        let pad = |p: &[u8]| {
            let mut v = p.to_vec();
            v.resize(16, b'x');
            v
        };
        assert_eq!(image_type(&pad(b"gd2")), Some(ImageType::Gd2));
        assert_eq!(image_type(&pad(&[0xff, 0xd8, 0xff])), Some(ImageType::Jpeg));
        assert_eq!(image_type(&pad(&SIG_PNG)), Some(ImageType::Png));
        assert_eq!(image_type(&pad(b"GIF89a")), Some(ImageType::Gif));
        assert_eq!(image_type(&pad(b"BM")), Some(ImageType::Bmp));
        assert_eq!(image_type(b"RIFF\0\0\0\0WEBPxxxx"), Some(ImageType::Webp));
        assert_eq!(image_type(&pad(&[0])), Some(ImageType::Wbmp));
        assert_eq!(image_type(&pad(b"zz")), None);
        assert_eq!(image_type(b"GIF"), None);
    }

    #[test]
    fn avif_brands() {
        let mut major = Vec::new();
        major.extend_from_slice(&16u32.to_be_bytes());
        major.extend_from_slice(b"ftypavif\0\0\0\0");
        assert!(is_avif(&major));
        assert_eq!(image_type(&major), Some(ImageType::Avif));

        let mut compat = Vec::new();
        compat.extend_from_slice(&24u32.to_be_bytes());
        compat.extend_from_slice(b"ftypmif1\0\0\0\0miafavis");
        assert!(is_avif(&compat));

        let mut minor_only = Vec::new();
        minor_only.extend_from_slice(&16u32.to_be_bytes());
        minor_only.extend_from_slice(b"ftypmif1avif");
        assert!(!is_avif(&minor_only));
    }
}
