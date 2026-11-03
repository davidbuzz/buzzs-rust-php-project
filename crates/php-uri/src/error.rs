//! Error values raised by the URI classes.
//!
//! The crate never depends on the VM, so every failure is described by an
//! [`UriError`]: the PHP class that must be thrown, the exact message, and —
//! for `Uri\WhatWg\InvalidUrlException` — the list of
//! `Uri\WhatWg\UrlValidationError` objects stored in its `errors` property.

use std::fmt;

/// The PHP class of a thrown error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorClass {
    /// `Uri\UriError` (extends `\Error`): internal failures that should not
    /// happen in practice.
    UriError,
    /// `Uri\InvalidUriException` (extends `Uri\UriException`).
    InvalidUriException,
    /// `Uri\WhatWg\InvalidUrlException` (extends `Uri\InvalidUriException`).
    InvalidUrlException,
    /// Plain `\Error` (e.g. "Cannot modify readonly object of class ...").
    Error,
    /// Plain `\Exception` (e.g. "Invalid serialization data for ... object").
    Exception,
    /// `\ValueError`.
    ValueError,
}

impl ErrorClass {
    /// The fully qualified PHP class name.
    pub fn class_name(self) -> &'static str {
        match self {
            ErrorClass::UriError => "Uri\\UriError",
            ErrorClass::InvalidUriException => "Uri\\InvalidUriException",
            ErrorClass::InvalidUrlException => "Uri\\WhatWg\\InvalidUrlException",
            ErrorClass::Error => "Error",
            ErrorClass::Exception => "Exception",
            ErrorClass::ValueError => "ValueError",
        }
    }
}

/// The cases of the `Uri\WhatWg\UrlValidationErrorType` enum, in declaration
/// order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UrlValidationErrorType {
    DomainToAscii,
    DomainToUnicode,
    DomainInvalidCodePoint,
    HostInvalidCodePoint,
    Ipv4EmptyPart,
    Ipv4TooManyParts,
    Ipv4NonNumericPart,
    Ipv4NonDecimalPart,
    Ipv4OutOfRangePart,
    Ipv6Unclosed,
    Ipv6InvalidCompression,
    Ipv6TooManyPieces,
    Ipv6MultipleCompression,
    Ipv6InvalidCodePoint,
    Ipv6TooFewPieces,
    Ipv4InIpv6TooManyPieces,
    Ipv4InIpv6InvalidCodePoint,
    Ipv4InIpv6OutOfRangePart,
    Ipv4InIpv6TooFewParts,
    InvalidUrlUnit,
    SpecialSchemeMissingFollowingSolidus,
    MissingSchemeNonRelativeUrl,
    InvalidReverseSoldius,
    InvalidCredentials,
    HostMissing,
    PortOutOfRange,
    PortInvalid,
    FileInvalidWindowsDriveLetter,
    FileInvalidWindowsDriveLetterHost,
}

impl UrlValidationErrorType {
    /// All cases in declaration order (the order of `cases()` in PHP).
    pub const ALL: [UrlValidationErrorType; 29] = [
        Self::DomainToAscii,
        Self::DomainToUnicode,
        Self::DomainInvalidCodePoint,
        Self::HostInvalidCodePoint,
        Self::Ipv4EmptyPart,
        Self::Ipv4TooManyParts,
        Self::Ipv4NonNumericPart,
        Self::Ipv4NonDecimalPart,
        Self::Ipv4OutOfRangePart,
        Self::Ipv6Unclosed,
        Self::Ipv6InvalidCompression,
        Self::Ipv6TooManyPieces,
        Self::Ipv6MultipleCompression,
        Self::Ipv6InvalidCodePoint,
        Self::Ipv6TooFewPieces,
        Self::Ipv4InIpv6TooManyPieces,
        Self::Ipv4InIpv6InvalidCodePoint,
        Self::Ipv4InIpv6OutOfRangePart,
        Self::Ipv4InIpv6TooFewParts,
        Self::InvalidUrlUnit,
        Self::SpecialSchemeMissingFollowingSolidus,
        Self::MissingSchemeNonRelativeUrl,
        Self::InvalidReverseSoldius,
        Self::InvalidCredentials,
        Self::HostMissing,
        Self::PortOutOfRange,
        Self::PortInvalid,
        Self::FileInvalidWindowsDriveLetter,
        Self::FileInvalidWindowsDriveLetterHost,
    ];

    /// The enum case name, e.g. `"InvalidUrlUnit"`.
    pub fn name(self) -> &'static str {
        match self {
            Self::DomainToAscii => "DomainToAscii",
            Self::DomainToUnicode => "DomainToUnicode",
            Self::DomainInvalidCodePoint => "DomainInvalidCodePoint",
            Self::HostInvalidCodePoint => "HostInvalidCodePoint",
            Self::Ipv4EmptyPart => "Ipv4EmptyPart",
            Self::Ipv4TooManyParts => "Ipv4TooManyParts",
            Self::Ipv4NonNumericPart => "Ipv4NonNumericPart",
            Self::Ipv4NonDecimalPart => "Ipv4NonDecimalPart",
            Self::Ipv4OutOfRangePart => "Ipv4OutOfRangePart",
            Self::Ipv6Unclosed => "Ipv6Unclosed",
            Self::Ipv6InvalidCompression => "Ipv6InvalidCompression",
            Self::Ipv6TooManyPieces => "Ipv6TooManyPieces",
            Self::Ipv6MultipleCompression => "Ipv6MultipleCompression",
            Self::Ipv6InvalidCodePoint => "Ipv6InvalidCodePoint",
            Self::Ipv6TooFewPieces => "Ipv6TooFewPieces",
            Self::Ipv4InIpv6TooManyPieces => "Ipv4InIpv6TooManyPieces",
            Self::Ipv4InIpv6InvalidCodePoint => "Ipv4InIpv6InvalidCodePoint",
            Self::Ipv4InIpv6OutOfRangePart => "Ipv4InIpv6OutOfRangePart",
            Self::Ipv4InIpv6TooFewParts => "Ipv4InIpv6TooFewParts",
            Self::InvalidUrlUnit => "InvalidUrlUnit",
            Self::SpecialSchemeMissingFollowingSolidus => "SpecialSchemeMissingFollowingSolidus",
            Self::MissingSchemeNonRelativeUrl => "MissingSchemeNonRelativeUrl",
            Self::InvalidReverseSoldius => "InvalidReverseSoldius",
            Self::InvalidCredentials => "InvalidCredentials",
            Self::HostMissing => "HostMissing",
            Self::PortOutOfRange => "PortOutOfRange",
            Self::PortInvalid => "PortInvalid",
            Self::FileInvalidWindowsDriveLetter => "FileInvalidWindowsDriveLetter",
            Self::FileInvalidWindowsDriveLetterHost => "FileInvalidWindowsDriveLetterHost",
        }
    }

    /// Looks up a case by its name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.name() == name)
    }

    /// Whether PHP marks this error type as a parse failure (the `failure`
    /// property of `UrlValidationError`), mirroring
    /// `get_reason_from_error_type()` in `uri_parser_whatwg.c`.
    pub fn is_failure(self) -> bool {
        !matches!(
            self,
            Self::DomainToUnicode
                | Self::Ipv4EmptyPart
                | Self::Ipv4NonDecimalPart
                | Self::InvalidUrlUnit
                | Self::SpecialSchemeMissingFollowingSolidus
                | Self::InvalidReverseSoldius
                | Self::InvalidCredentials
                | Self::FileInvalidWindowsDriveLetter
                | Self::FileInvalidWindowsDriveLetterHost
        )
    }
}

impl fmt::Display for UrlValidationErrorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One `Uri\WhatWg\UrlValidationError` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlValidationError {
    /// The `context` property: the remainder of the input starting at the
    /// position where the error was detected (as lexbor reports it).
    pub context: Vec<u8>,
    /// The `type` property.
    pub error_type: UrlValidationErrorType,
    /// The `failure` property.
    pub failure: bool,
}

impl UrlValidationError {
    /// Creates an error with the default `failure` flag of its type.
    pub fn new(context: impl Into<Vec<u8>>, error_type: UrlValidationErrorType) -> Self {
        UrlValidationError {
            context: context.into(),
            error_type,
            failure: error_type.is_failure(),
        }
    }
}

/// A PHP throwable produced by this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UriError {
    /// Which class to instantiate.
    pub class: ErrorClass,
    /// The exception message.
    pub message: String,
    /// The `errors` property of `Uri\WhatWg\InvalidUrlException` (empty for
    /// the other classes).
    pub errors: Vec<UrlValidationError>,
}

impl UriError {
    /// Creates an error without validation errors.
    pub fn new(class: ErrorClass, message: impl Into<String>) -> Self {
        UriError {
            class,
            message: message.into(),
            errors: Vec::new(),
        }
    }

    /// `Uri\InvalidUriException` with the given message.
    pub fn invalid_uri(message: impl Into<String>) -> Self {
        Self::new(ErrorClass::InvalidUriException, message)
    }

    /// `Uri\WhatWg\InvalidUrlException` with the given message and errors.
    pub fn invalid_url(message: impl Into<String>, errors: Vec<UrlValidationError>) -> Self {
        UriError {
            class: ErrorClass::InvalidUrlException,
            message: message.into(),
            errors,
        }
    }
}

impl fmt::Display for UriError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.class.class_name(), self.message)
    }
}

impl std::error::Error for UriError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_flags_match_php() {
        assert!(UrlValidationErrorType::DomainToAscii.is_failure());
        assert!(!UrlValidationErrorType::DomainToUnicode.is_failure());
        assert!(!UrlValidationErrorType::InvalidUrlUnit.is_failure());
        assert!(UrlValidationErrorType::PortInvalid.is_failure());
        assert!(UrlValidationErrorType::Ipv4InIpv6TooFewParts.is_failure());
    }

    #[test]
    fn names_round_trip() {
        for t in UrlValidationErrorType::ALL {
            assert_eq!(UrlValidationErrorType::from_name(t.name()), Some(t));
        }
        assert_eq!(UrlValidationErrorType::from_name("Nope"), None);
    }

    #[test]
    fn display() {
        let e = UriError::invalid_uri("The specified URI is malformed");
        assert_eq!(
            e.to_string(),
            "Uri\\InvalidUriException: The specified URI is malformed"
        );
    }
}
