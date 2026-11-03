//! `ext/uri` for the phprust VM: the `Uri\Rfc3986\*` and `Uri\WhatWg\*`
//! classes, their enums and exceptions, and `Uri\WhatWg\url_percent_encode()`.
//!
//! Parsing, normalization, resolution and every exception message live in
//! the `php-uri` library (`php_uri::api`, one Rust method per PHP method);
//! this crate keeps the parsed URI as native object state, converts values
//! and turns `UriError`s into PHP exceptions (including the
//! `Uri\WhatWg\UrlValidationError` lists of `InvalidUrlException::$errors`
//! and the by-reference `$errors`/`$softErrors` arguments).
//!
//! The extension API cannot build native enums yet, so the six enums are
//! declared by compiling their PHP source at startup (like
//! `php-ext-random` does for `Random\IntervalBoundary`).

mod common;
mod memory;
mod rfc3986;
mod whatwg;

use php_runtime::ZStr;
use php_vm::class::cflags;
use php_vm::ext::{ClassBuilder, Extension, ModuleBuilder};
use php_vm::Vm;

/// The `uri` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Uri;

/// The enum declarations of `php_uri.stub.php`.
const ENUMS: &str = r"<?php
namespace Uri {
    enum UriComparisonMode { case IncludeFragment; case ExcludeFragment; }
}
namespace Uri\Rfc3986 {
    enum UriType { case AbsolutePathReference; case RelativePathReference; case NetworkPathReference; case Uri; }
    enum UriHostType { case IpV4; case IpV6; case IpVFuture; case RegisteredName; }
}
namespace Uri\WhatWg {
    enum UrlValidationErrorType {
        case DomainToAscii; case DomainToUnicode; case DomainInvalidCodePoint; case HostInvalidCodePoint;
        case Ipv4EmptyPart; case Ipv4TooManyParts; case Ipv4NonNumericPart; case Ipv4NonDecimalPart;
        case Ipv4OutOfRangePart; case Ipv6Unclosed; case Ipv6InvalidCompression; case Ipv6TooManyPieces;
        case Ipv6MultipleCompression; case Ipv6InvalidCodePoint; case Ipv6TooFewPieces;
        case Ipv4InIpv6TooManyPieces; case Ipv4InIpv6InvalidCodePoint; case Ipv4InIpv6OutOfRangePart;
        case Ipv4InIpv6TooFewParts; case InvalidUrlUnit; case SpecialSchemeMissingFollowingSolidus;
        case MissingSchemeNonRelativeUrl; case InvalidReverseSoldius; case InvalidCredentials;
        case HostMissing; case PortOutOfRange; case PortInvalid; case FileInvalidWindowsDriveLetter;
        case FileInvalidWindowsDriveLetterHost;
    }
    enum UrlHostType { case IpV4; case IpV6; case Domain; case Opaque; case Empty; }
    enum UrlPercentEncodingMode {
        case Username; case Password; case OpaqueHost; case Path; case OpaquePath; case PathSegment;
        case Query; case SpecialQuery; case FormQuery; case Fragment;
    }
}
";

/// Declares the enums from PHP source (no native enum builder exists).
fn register_enums(vm: &mut Vm) {
    if vm.find_class(b"Uri\\UriComparisonMode").is_some() {
        return;
    }
    let _ = vm.run_main(ENUMS.as_bytes(), &ZStr::from("[ext/uri]"));
}

/// The plain exception hierarchy of `Uri\`.
fn register_exceptions(m: &mut ModuleBuilder<'_>) {
    ClassBuilder::new("Uri\\UriException")
        .extends(m.vm(), "Exception")
        .flags(cflags::NO_DYNAMIC_PROPERTIES)
        .register(m.vm());
    ClassBuilder::new("Uri\\UriError")
        .extends(m.vm(), "Error")
        .flags(cflags::NO_DYNAMIC_PROPERTIES)
        .register(m.vm());
    ClassBuilder::new("Uri\\InvalidUriException")
        .extends(m.vm(), "Uri\\UriException")
        .flags(cflags::NO_DYNAMIC_PROPERTIES)
        .register(m.vm());
}

impl Extension for Uri {
    fn name(&self) -> &'static str {
        "uri"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        register_enums(m.vm());
        rfc3986::register(m);
        register_exceptions(m);
        whatwg::register_exception(m);
        whatwg::register_validation_error(m);
        whatwg::register(m);
    }
}
