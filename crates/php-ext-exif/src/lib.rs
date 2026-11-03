//! `ext/exif` and the image functions of `ext/standard` (`image.c`,
//! `iptc.c`) for the phprust VM.
//!
//! The parsing lives in the `php-exif` library; this crate translates PHP
//! arguments, opens files through the stream layer (`php-ext-streams`),
//! adapts open streams to `Read + Seek`, raises the library's diagnostics
//! with the running function's prefix and turns its out-of-memory
//! diagnostic into the "Allowed memory size" fatal error.
//!
//! | PHP | here |
//! |-----|------|
//! | `exif_read_data`, `exif_thumbnail`, `exif_imagetype`, `exif_tagname` | [`exif`] |
//! | `getimagesize`, `getimagesizefromstring`, `image_type_to_*`, `iptc*` | [`image`] |
//! | `IMAGETYPE_*`, `EXIF_USE_MBSTRING`, `exif.*` INI | [`Exif::register`] |

mod convert;
mod exif;
mod image;
mod io;

use php_exif::budget::MemoryLimit;
use php_exif::{Diagnostic, Level};
use php_runtime::Value;
use php_support::ini::{Access, IniDirective, ModifyHandler};
use php_vm::ext::{Extension, InfoItem, ModuleBuilder, VmResult};
use php_vm::Vm;

/// The `exif` extension (also providing ext/standard's image functions,
/// which share its parsers).
#[derive(Debug, Clone, Copy, Default)]
pub struct Exif;

/// The `memory_limit` the parsers must respect (`None` = unlimited), with
/// the script's current usage from `php-memlimit`.
pub(crate) fn memory_limit(_vm: &Vm) -> Option<MemoryLimit> {
    php_memlimit::limit().map(|limit| MemoryLimit {
        limit: limit as u64,
        used: php_memlimit::usage() as u64,
    })
}

/// Raises the library diagnostics in order, as `func(): message`; the
/// out-of-memory `E_ERROR` aborts the script.
pub(crate) fn emit(vm: &mut Vm, diags: &[Diagnostic]) -> VmResult<()> {
    // The real allocations crossed `memory_limit` while parsing: the stream
    // adapter stopped feeding the parser, so its late diagnostics are
    // artefacts; PHP would have died at that allocation.
    if let Some(over) = php_memlimit::take() {
        return Err(vm.fatal(&over.to_string()));
    }
    for d in diags {
        match d.level {
            Level::Warning => vm.php_warning_bytes(&d.message)?,
            Level::Notice => vm.php_error_bytes(Level::Notice.code(), &d.message)?,
            Level::Error => return Err(vm.fatal(&d.message_lossy())),
        }
    }
    Ok(())
}

/// The `exif.*` `on_modify` handlers (`OnUpdateEncode`/`OnUpdateDecode`):
/// an unknown encoding is rejected with "Illegal encoding ignored".
fn validate_encoding(vm: &mut Vm, name: &str, value: &[u8]) -> VmResult<bool> {
    let mut cfg = php_exif::ExifConfig::default();
    let value = String::from_utf8_lossy(value);
    match cfg.set(name, &value, &convert::MbConverter) {
        Ok(_) => Ok(true),
        Err(d) => {
            emit(vm, &[d])?;
            Ok(false)
        }
    }
}

macro_rules! ini_handlers {
    ($($f:ident => $name:literal),* $(,)?) => {
        $(fn $f(vm: &mut Vm, value: &[u8]) -> VmResult<bool> {
            validate_encoding(vm, $name, value)
        })*
        /// The handlers by directive name.
        const INI_HANDLERS: &[(&str, php_vm::ext::IniModifyFn)] = &[$(($name, $f)),*];
    };
}

ini_handlers! {
    on_encode_unicode => "exif.encode_unicode",
    on_decode_unicode_motorola => "exif.decode_unicode_motorola",
    on_decode_unicode_intel => "exif.decode_unicode_intel",
    on_encode_jis => "exif.encode_jis",
    on_decode_jis_motorola => "exif.decode_jis_motorola",
    on_decode_jis_intel => "exif.decode_jis_intel",
}

/// `PHP_MINFO_FUNCTION(exif)`.
fn info(_vm: &mut Vm) -> Vec<InfoItem> {
    let row = |k: &str, v: &str| InfoItem::Row(vec![k.to_string(), v.to_string()]);
    vec![
        InfoItem::TableStart,
        row("EXIF Support", "enabled"),
        row("Supported EXIF Version", "0220"),
        row("Supported filetypes", "JPEG, TIFF, HEIF, WebP"),
        row("Multibyte decoding support using mbstring", "enabled"),
        row(
            "Extended EXIF tag formats",
            "Canon, Casio, Fujifilm, Nikon, Olympus, Samsung, Panasonic, DJI, Sony, Pentax, Minolta, Sigma, Foveon, Kyocera, Ricoh, AGFA, Epson",
        ),
        InfoItem::TableEnd,
        InfoItem::IniEntries,
    ]
}

impl Extension for Exif {
    fn name(&self) -> &'static str {
        "exif"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        for (name, default) in php_exif::exif::INI_ENTRIES {
            m.ini(IniDirective::new(
                name,
                Some(default),
                Access::ALL,
                ModifyHandler::String,
                "exif",
            ));
        }
        for (name, f) in INI_HANDLERS {
            m.ini_on_modify(name, *f);
        }
        m.info(info);
        m.constant("EXIF_USE_MBSTRING", Value::Int(php_exif::EXIF_USE_MBSTRING));
        for (name, value) in php_exif::imagetype::IMAGETYPE_CONSTANTS {
            m.constant(name, Value::Int(*value));
        }
        exif::register(m);
        image::register(m);
    }
}
