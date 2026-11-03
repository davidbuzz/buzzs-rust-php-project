//! Class registration (`phar_object.stub.php`).

use php_phar::consts::CLASS_CONSTANTS;
use php_runtime::Value;
use php_vm::ext::{acc, ClassBuilder, ModuleBuilder, NativeFn};

use crate::{archive as ar, fileinfo as fi, statics as st};

const PUB: u32 = acc::PUBLIC;
const STATIC: u32 = acc::PUBLIC | acc::STATIC | acc::FINAL;

/// The instance methods shared by `Phar` and `PharData` (the latter with
/// its own return types).
fn instance_methods(data: bool) -> Vec<(&'static str, NativeFn)> {
    let v: Vec<(&'static str, &'static str, NativeFn)> = vec![
        (
            "__construct(string $filename, int $flags = FilesystemIterator::SKIP_DOTS|FilesystemIterator::UNIX_PATHS, ?string $alias = null)",
            "__construct(string $filename, int $flags = FilesystemIterator::SKIP_DOTS|FilesystemIterator::UNIX_PATHS, ?string $alias = null, int $format = 0)",
            ar::construct,
        ),
        ("__destruct()", "__destruct()", ar::destruct),
        ("addEmptyDir(string $directory): void", "addEmptyDir(string $directory): void", ar::add_empty_dir),
        ("addFile(string $filename, ?string $localName = null): void", "addFile(string $filename, ?string $localName = null): void", ar::add_file),
        ("addFromString(string $localName, string $contents): void", "addFromString(string $localName, string $contents): void", ar::add_from_string),
        ("buildFromDirectory(string $directory, string $pattern = \"\"): array", "buildFromDirectory(string $directory, string $pattern = \"\"): array", ar::build_from_directory),
        ("buildFromIterator(Traversable $iterator, ?string $baseDirectory = null): array", "buildFromIterator(Traversable $iterator, ?string $baseDirectory = null): array", ar::build_from_iterator),
        ("compressFiles(int $compression): void", "compressFiles(int $compression): void", ar::compress_files),
        ("decompressFiles(): true", "decompressFiles(): true", ar::decompress_files),
        ("compress(int $compression, ?string $extension = null): ?Phar", "compress(int $compression, ?string $extension = null): ?PharData", ar::compress),
        ("decompress(?string $extension = null): ?Phar", "decompress(?string $extension = null): ?PharData", ar::decompress),
        ("convertToExecutable(?int $format = null, ?int $compression = null, ?string $extension = null): ?Phar", "convertToExecutable(?int $format = null, ?int $compression = null, ?string $extension = null): ?Phar", ar::convert_to_executable),
        ("convertToData(?int $format = null, ?int $compression = null, ?string $extension = null): ?PharData", "convertToData(?int $format = null, ?int $compression = null, ?string $extension = null): ?PharData", ar::convert_to_data),
        ("copy(string $from, string $to): true", "copy(string $from, string $to): true", ar::copy),
        ("count(int $mode = COUNT_NORMAL): int", "count(int $mode = COUNT_NORMAL): int", ar::count),
        ("delete(string $localName): true", "delete(string $localName): true", ar::delete),
        ("delMetadata(): true", "delMetadata(): true", ar::del_metadata),
        ("extractTo(string $directory, array|string|null $files = null, bool $overwrite = false): bool", "extractTo(string $directory, array|string|null $files = null, bool $overwrite = false): bool", ar::extract_to),
        ("getAlias(): ?string", "getAlias(): ?string", ar::get_alias),
        ("getPath(): string", "getPath(): string", ar::get_path),
        ("getMetadata(array $unserializeOptions = []): mixed", "getMetadata(array $unserializeOptions = []): mixed", ar::get_metadata),
        ("getModified(): bool", "getModified(): bool", ar::get_modified),
        ("getSignature(): array|false", "getSignature(): array|false", ar::get_signature),
        ("getStub(): string", "getStub(): string", ar::get_stub),
        ("getVersion(): string", "getVersion(): string", ar::get_version),
        ("hasMetadata(): bool", "hasMetadata(): bool", ar::has_metadata),
        ("isBuffering(): bool", "isBuffering(): bool", ar::is_buffering),
        ("isCompressed(): int|false", "isCompressed(): int|false", ar::is_compressed),
        ("isFileFormat(int $format): bool", "isFileFormat(int $format): bool", ar::is_file_format),
        ("isWritable(): bool", "isWritable(): bool", ar::is_writable),
        ("offsetExists($localName): bool", "offsetExists($localName): bool", ar::offset_exists),
        ("offsetGet($localName): SplFileInfo", "offsetGet($localName): SplFileInfo", ar::offset_get),
        ("offsetSet($localName, $value): void", "offsetSet($localName, $value): void", ar::offset_set),
        ("offsetUnset($localName): void", "offsetUnset($localName): void", ar::offset_unset),
        ("setAlias(string $alias): true", "setAlias(string $alias): bool", ar::set_alias),
        ("setDefaultStub(?string $index = null, ?string $webIndex = null): true", "setDefaultStub(?string $index = null, ?string $webIndex = null): bool", ar::set_default_stub),
        ("setMetadata(mixed $metadata): void", "setMetadata(mixed $metadata): void", ar::set_metadata),
        ("setSignatureAlgorithm(int $algo, ?string $privateKey = null): void", "setSignatureAlgorithm(int $algo, ?string $privateKey = null): void", ar::set_signature_algorithm),
        ("setStub($stub, int $length = UNKNOWN): true", "setStub($stub, int $length = UNKNOWN): true", ar::set_stub),
        ("startBuffering(): void", "startBuffering(): void", ar::start_buffering),
        ("stopBuffering(): void", "stopBuffering(): void", ar::stop_buffering),
    ];
    v.into_iter()
        .map(|(p, d, f)| (if data { d } else { p }, f))
        .collect()
}

const STATIC_METHODS: &[(&str, NativeFn)] = &[
    ("apiVersion(): string", st::api_version),
    ("canCompress(int $compression = 0): bool", st::can_compress),
    ("canWrite(): bool", st::can_write),
    ("createDefaultStub(?string $index = null, ?string $webIndex = null): string", st::create_default_stub),
    ("getSupportedCompression(): array", st::get_supported_compression),
    ("getSupportedSignatures(): array", st::get_supported_signatures),
    ("interceptFileFuncs(): void", st::intercept_file_funcs),
    ("isValidPharFilename(string $filename, bool $executable = true): bool", st::is_valid_phar_filename),
    ("loadPhar(string $filename, ?string $alias = null): bool", st::load_phar),
    ("mapPhar(?string $alias = null, int $offset = 0): bool", st::map_phar),
    ("running(bool $returnPhar = true): string", st::running),
    ("mount(string $pharPath, string $externalPath): void", st::mount),
    ("mungServer(array $variables): void", st::mung_server),
    ("unlinkArchive(string $filename): true", st::unlink_archive),
    ("webPhar(?string $alias = null, ?string $index = null, ?string $fileNotFoundScript = null, array $mimeTypes = [], ?callable $rewrite = null): void", st::web_phar),
];

const FILEINFO_METHODS: &[(&str, NativeFn)] = &[
    ("__construct(string $filename)", fi::construct),
    ("__destruct()", fi::destruct),
    ("chmod(int $perms): void", fi::chmod),
    ("compress(int $compression): true", fi::compress),
    ("decompress(): true", fi::decompress),
    ("delMetadata(): true", fi::del_metadata),
    ("getCompressedSize(): int", fi::get_compressed_size),
    ("getCRC32(): int", fi::get_crc32),
    ("getContent(): string", fi::get_content),
    (
        "getMetadata(array $unserializeOptions = []): mixed",
        fi::get_metadata,
    ),
    ("getPharFlags(): int", fi::get_phar_flags),
    ("hasMetadata(): bool", fi::has_metadata),
    (
        "isCompressed(?int $compression = null): bool",
        fi::is_compressed,
    ),
    ("isCRCChecked(): bool", fi::is_crc_checked),
    ("setMetadata(mixed $metadata): void", fi::set_metadata),
];

fn archive_class(m: &mut ModuleBuilder<'_>, name: &str, data: bool) {
    let vm = m.vm();
    let mut b = ClassBuilder::new(name)
        .extends(vm, "RecursiveDirectoryIterator")
        .implements(vm, &["Countable", "ArrayAccess"]);
    if !data {
        for (n, v) in CLASS_CONSTANTS {
            b = b.constant(n, Value::Int(*v));
        }
    }
    for (sig, f) in instance_methods(data) {
        b = b.method(sig, PUB, f);
    }
    for (sig, f) in STATIC_METHODS {
        b = b.method(sig, STATIC, *f);
    }
    b.register(vm);
}

/// Registers `PharException`, `Phar`, `PharData` and `PharFileInfo`.
pub fn register(m: &mut ModuleBuilder<'_>) {
    let vm = m.vm();
    ClassBuilder::new("PharException")
        .extends(vm, "Exception")
        .register(vm);
    archive_class(m, "Phar", false);
    archive_class(m, "PharData", true);
    let vm = m.vm();
    let mut b = ClassBuilder::new("PharFileInfo").extends(vm, "SplFileInfo");
    for (sig, f) in FILEINFO_METHODS {
        b = b.method(sig, PUB, *f);
    }
    b.register(vm);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_tables_match() {
        assert_eq!(instance_methods(false).len(), instance_methods(true).len());
        assert_eq!(STATIC_METHODS.len(), 15);
        assert_eq!(FILEINFO_METHODS.len(), 15);
    }
}
