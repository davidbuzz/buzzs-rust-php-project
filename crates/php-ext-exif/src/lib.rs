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
use php_vm::ext::{Extension, ModuleBuilder, VmResult};
use php_vm::Vm;

/// The `exif` extension (also providing ext/standard's image functions,
/// which share its parsers).
#[derive(Debug, Clone, Copy, Default)]
pub struct Exif;

/// Memory a fresh script already uses, for the `memory_limit` emulation.
const BASELINE_USAGE: u64 = 512 * 1024;

/// The `memory_limit` the parsers must respect (`None` = unlimited).
pub(crate) fn memory_limit(vm: &Vm) -> Option<MemoryLimit> {
    let raw = vm
        .ini_string("memory_limit")
        .unwrap_or_else(|| b"128M".to_vec());
    let (v, _) = php_support::ini::parse_quantity(&raw);
    u64::try_from(v)
        .ok()
        .filter(|&v| v > 0)
        .map(|limit| MemoryLimit {
            limit,
            used: BASELINE_USAGE,
        })
}

/// Raises the library diagnostics in order, as `func(): message`; the
/// out-of-memory `E_ERROR` aborts the script.
pub(crate) fn emit(vm: &mut Vm, diags: &[Diagnostic]) -> VmResult<()> {
    for d in diags {
        match d.level {
            Level::Warning => vm.php_warning_bytes(&d.message)?,
            Level::Notice => vm.php_error_bytes(Level::Notice.code(), &d.message)?,
            Level::Error => return Err(vm.fatal(&d.message_lossy())),
        }
    }
    Ok(())
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
        m.constant("EXIF_USE_MBSTRING", Value::Int(php_exif::EXIF_USE_MBSTRING));
        for (name, value) in php_exif::imagetype::IMAGETYPE_CONSTANTS {
            m.constant(name, Value::Int(*value));
        }
        exif::register(m);
        image::register(m);
    }
}
