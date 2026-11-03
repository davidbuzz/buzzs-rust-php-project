//! `mb_get_info` and `mb_output_handler`.
//!
//! Ref: `ext/mbstring/mbstring.c`.

use crate::convert::ConvertBuf;
use crate::encoding::{self, EncodingNo};
use crate::state::{MbState, SubstituteChar};

/// A value in `mb_get_info()`'s result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InfoValue {
    /// A string.
    Str(String),
    /// An integer.
    Int(i64),
    /// A list of strings.
    List(Vec<&'static str>),
    /// An associative array (for `mb_get_info()` / `"all"`).
    Assoc(Vec<(&'static str, InfoValue)>),
    /// `null`.
    Null,
    /// `false` (invalid type; a warning was emitted).
    False,
}

fn on_off(b: bool) -> InfoValue {
    InfoValue::Str(if b { "On" } else { "Off" }.into())
}

impl MbState {
    fn substitute_info(&self) -> InfoValue {
        match self.mb_substitute_character_get() {
            SubstituteChar::None => InfoValue::Str("none".into()),
            SubstituteChar::Long => InfoValue::Str("long".into()),
            SubstituteChar::Entity => InfoValue::Str("entity".into()),
            SubstituteChar::Char(c) => InfoValue::Int(i64::from(c)),
        }
    }

    /// `mb_get_info($type)`. For `"detect_order"` with an empty list PHP
    /// returns `null` (`InfoValue::Null`).
    pub fn mb_get_info(&mut self, ty: Option<&[u8]>) -> InfoValue {
        let lang = self.language();
        let name = |n: EncodingNo| encoding::get(n).name.to_string();
        let ty = ty.map(<[u8]>::to_ascii_lowercase);
        let detect: Vec<&'static str> = self.detect_order().iter().map(|e| e.name).collect();
        let mimetypes = String::from_utf8_lossy(&self.http_output_conv_mimetypes).into_owned();
        match ty.as_deref() {
            None | Some(b"all") => {
                let mut v = vec![(
                    "internal_encoding",
                    InfoValue::Str(self.internal_encoding().name.into()),
                )];
                if let Some(e) = self.http_input_identify {
                    v.push(("http_input", InfoValue::Str(e.name.into())));
                }
                v.push((
                    "http_output",
                    InfoValue::Str(self.http_output_encoding().name.into()),
                ));
                v.push(("http_output_conv_mimetypes", InfoValue::Str(mimetypes)));
                v.push(("mail_charset", InfoValue::Str(name(lang.mail_charset))));
                v.push((
                    "mail_header_encoding",
                    InfoValue::Str(name(lang.mail_header_encoding)),
                ));
                v.push((
                    "mail_body_encoding",
                    InfoValue::Str(name(lang.mail_body_encoding)),
                ));
                v.push((
                    "illegal_chars",
                    InfoValue::Int(i64::try_from(self.illegalchars).unwrap_or(i64::MAX)),
                ));
                v.push(("encoding_translation", on_off(self.encoding_translation)));
                v.push(("language", InfoValue::Str(lang.name.into())));
                if !detect.is_empty() {
                    v.push(("detect_order", InfoValue::List(detect)));
                }
                v.push(("substitute_character", self.substitute_info()));
                v.push(("strict_detection", on_off(self.strict_detection)));
                InfoValue::Assoc(v)
            }
            Some(b"internal_encoding") => InfoValue::Str(self.internal_encoding().name.into()),
            Some(b"http_input") => self
                .http_input_identify
                .map_or(InfoValue::Null, |e| InfoValue::Str(e.name.into())),
            Some(b"http_output") => InfoValue::Str(self.http_output_encoding().name.into()),
            Some(b"http_output_conv_mimetypes") => InfoValue::Str(mimetypes),
            Some(b"mail_charset") => InfoValue::Str(name(lang.mail_charset)),
            Some(b"mail_header_encoding") => InfoValue::Str(name(lang.mail_header_encoding)),
            Some(b"mail_body_encoding") => InfoValue::Str(name(lang.mail_body_encoding)),
            Some(b"illegal_chars") => {
                InfoValue::Int(i64::try_from(self.illegalchars).unwrap_or(i64::MAX))
            }
            Some(b"encoding_translation") => on_off(self.encoding_translation),
            Some(b"language") => InfoValue::Str(lang.name.into()),
            Some(b"detect_order") => {
                if detect.is_empty() {
                    InfoValue::Null
                } else {
                    InfoValue::List(detect)
                }
            }
            Some(b"substitute_character") => self.substitute_info(),
            Some(b"strict_detection") => on_off(self.strict_detection),
            Some(_) => {
                self.warn("argument #1 ($type) must be a valid type");
                InfoValue::False
            }
        }
    }
}

/// SAPI hooks needed by [`MbState::mb_output_handler`].
pub trait OutputSapi {
    /// `SG(sapi_headers).mimetype`.
    fn mimetype(&self) -> Option<Vec<u8>>;
    /// Whether `pattern` (`mbstring.http_output_conv_mimetypes`) matches
    /// `mimetype` (PCRE, case-insensitive).
    fn mimetype_matches(&self, pattern: &[u8], mimetype: &[u8]) -> bool;
    /// `SG(sapi_headers).send_default_content_type`.
    fn send_default_content_type(&self) -> bool;
    /// Clear `send_default_content_type`.
    fn clear_send_default_content_type(&mut self);
    /// `SG(default_mimetype)` (or `"text/html"`).
    fn default_mimetype(&self) -> Vec<u8>;
    /// Whether an output handler is currently running
    /// (`php_output_handler_hook(GET_FLAGS)` succeeds).
    fn in_output_handler(&self) -> bool;
    /// `sapi_add_header`; returns success.
    fn add_header(&mut self, header: &[u8]) -> bool;
}

/// `PHP_OUTPUT_HANDLER_START`.
pub const OUTPUT_HANDLER_START: i64 = 0x01;
/// `PHP_OUTPUT_HANDLER_END` / `FINAL`.
pub const OUTPUT_HANDLER_END: i64 = 0x08;

impl MbState {
    /// `mb_output_handler($string, $status)`.
    pub fn mb_output_handler(
        &mut self,
        s: &[u8],
        status: i64,
        sapi: &mut dyn OutputSapi,
    ) -> Vec<u8> {
        let enc = self.http_output_encoding();
        if enc.no == EncodingNo::Pass {
            return s.to_vec();
        }
        if status & OUTPUT_HANDLER_START != 0 {
            let mut converted_mimetype = false;
            let mut mimetype = None;
            if let Some(m) = sapi.mimetype().filter(|m| {
                !self.http_output_conv_mimetypes.is_empty()
                    && sapi.mimetype_matches(&self.http_output_conv_mimetypes, m)
            }) {
                let end = m.iter().position(|&b| b == b';').unwrap_or(m.len());
                mimetype = Some(m[..end].to_vec());
                converted_mimetype = true;
            } else if sapi.send_default_content_type() {
                mimetype = Some(sapi.default_mimetype());
            }
            if sapi.send_default_content_type() || converted_mimetype {
                if let Some(charset) = enc.mime_name {
                    if !sapi.in_output_handler() {
                        let mut h = b"Content-Type: ".to_vec();
                        h.extend(mimetype.unwrap_or_default());
                        h.extend(b"; charset=");
                        h.extend(charset.as_bytes());
                        if sapi.add_header(&h) {
                            sapi.clear_send_default_content_type();
                        }
                    }
                }
                self.outconv_enabled = true;
            }
        }
        if !self.outconv_enabled {
            return s.to_vec();
        }
        let from = self.internal_encoding();
        let last = status & OUTPUT_HANDLER_END != 0;
        let mut buf = ConvertBuf::new(s.len(), self.illegal_substchar(), self.illegal_mode());
        let mut wb = [0u32; 128];
        let mut rest = s;
        while !rest.is_empty() {
            let n = (from.to_wchar)(&mut rest, &mut wb, &mut self.outconv_state);
            (enc.from_wchar)(&wb[..n], &mut buf, rest.is_empty() && last);
        }
        self.illegalchars += u64::from(buf.errors);
        if last {
            self.outconv_enabled = false;
            self.outconv_state = 0;
        }
        buf.into_bytes()
    }
}
