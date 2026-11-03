//! The basic URL parser, ported from lexbor's `lxb_url_parse_basic_h`
//! (`lexbor/url/url.c`), including its path fast/slow paths, so that
//! validation errors are reported in the same order and with the same
//! context strings.

use super::encode::{
    percent_encode_into, MAP_C0, MAP_FRAGMENT, MAP_PATH, MAP_QUERY, MAP_SPECIAL_QUERY,
    MAP_USERINFO, URL_MAP,
};
use super::host::{parse_host, HostOpt};
use super::utf8::{decode_valid_utf8_single, decode_valid_utf8_single_reverse, DECODE_ERROR};
use super::{Host, SchemeType, Url};
use crate::{ErrorContext, UrlValidationError, UrlValidationErrorType as E};
use std::sync::Arc;

/// A failed parse (lexbor returned a non-OK status).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fail;

/// A buffer that validation-error contexts can share.
#[derive(Debug, Clone)]
pub struct SharedBuf {
    bytes: Arc<[u8]>,
    /// Positions of NUL bytes (where PHP's C-string reading stops).
    nuls: Vec<usize>,
}

impl SharedBuf {
    /// Copies `data` into a shareable buffer.
    pub fn new(data: &[u8]) -> Self {
        SharedBuf {
            bytes: Arc::from(data),
            nuls: data
                .iter()
                .enumerate()
                .filter_map(|(i, &c)| (c == 0).then_some(i))
                .collect(),
        }
    }

    /// The context starting at `pos`: up to the next NUL byte or the end.
    fn context(&self, pos: usize) -> ErrorContext {
        let pos = pos.min(self.bytes.len());
        let idx = self.nuls.partition_point(|&n| n < pos);
        let end = self.nuls.get(idx).copied().unwrap_or(self.bytes.len());
        ErrorContext::from_shared(Arc::clone(&self.bytes), pos, end)
    }
}

impl std::ops::Deref for SharedBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

/// The validation error log of one parser run.
#[derive(Debug, Clone, Default)]
pub struct Log {
    /// Errors in the order they were reported.
    pub errors: Vec<UrlValidationError>,
}

impl Log {
    /// Appends an error whose context is `buf[pos..]` up to the first NUL
    /// byte (lexbor stores a pointer that PHP reads as a C string).
    pub(crate) fn push(&mut self, buf: &SharedBuf, pos: usize, t: E) {
        self.errors
            .push(UrlValidationError::new(buf.context(pos), t));
    }
}

/// Parser states (`lxb_url_state_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    SchemeStart,
    Scheme,
    NoScheme,
    SpecialRelativeOrAuthority,
    PathOrAuthority,
    Relative,
    RelativeSlash,
    SpecialAuthoritySlashes,
    SpecialAuthorityIgnoreSlashes,
    Authority,
    Host,
    Hostname,
    Port,
    File,
    FileSlash,
    FileHost,
    PathStart,
    Path,
    OpaquePath,
    Query,
    Fragment,
}

fn is_hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

/// URL code points below U+00A0 (`lxb_url_codepoint_alphanumeric`).
fn is_ascii_url_cp(c: u8) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            b'!' | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b'-'
                | b'.'
                | b'/'
                | b':'
                | b';'
                | b'='
                | b'?'
                | b'@'
                | b'_'
                | b'~'
        )
}

fn is_noncharacter(cp: u32) -> bool {
    (0xFDD0..=0xFDEF).contains(&cp) || (cp & 0xFFFE) == 0xFFFE
}

/// `lxb_url_is_url_codepoint`.
fn is_url_codepoint(cp: u32) -> bool {
    if (0xA0..=0x1F_FFFF).contains(&cp) {
        if (0xD800..=0xDFFF).contains(&cp) {
            return false;
        }
        return !(cp > 0x10_FFFF || is_noncharacter(cp));
    }
    cp < 0xA0 && is_ascii_url_cp(u8::try_from(cp).unwrap_or(0))
}

/// `lxb_url_path_map`: bytes the fast path cannot copy verbatim.
fn path_needs_care(c: u8) -> bool {
    !(0x21..0x7F).contains(&c)
        || matches!(
            c,
            b'"' | b'#'
                | b'%'
                | b'.'
                | b'/'
                | b'<'
                | b'>'
                | b'?'
                | b'['
                | b'\\'
                | b']'
                | b'^'
                | b'`'
                | b'{'
                | b'|'
                | b'}'
        )
}

fn is_windows_letter(c: u8) -> bool {
    c.is_ascii_alphabetic()
}

fn starts_with_windows_drive_letter(d: &[u8]) -> bool {
    if d.len() < 2 || !is_windows_letter(d[0]) || (d[1] != b':' && d[1] != b'|') {
        return false;
    }
    d.len() == 2 || matches!(d[2], b'/' | b'\\' | b'?' | b'#')
}

fn is_windows_drive_letter(d: &[u8]) -> bool {
    d.len() >= 2 && is_windows_letter(d[0]) && (d[1] == b':' || d[1] == b'|')
}

fn is_normalized_windows_drive_letter(d: &[u8]) -> bool {
    d.len() >= 2 && is_windows_letter(d[0]) && d[1] == b':'
}

/// `lxb_url_scheme_find`.
fn scheme_find(name: &[u8]) -> SchemeType {
    [
        SchemeType::Http,
        SchemeType::Https,
        SchemeType::Ws,
        SchemeType::Wss,
        SchemeType::Ftp,
        SchemeType::File,
    ]
    .into_iter()
    .find(|t| t.name().as_bytes().eq_ignore_ascii_case(name))
    .unwrap_or(SchemeType::Unknown)
}

impl Url {
    /// `lxb_url_path_shorten`.
    pub(crate) fn path_shorten(&mut self) {
        if self.scheme_type == SchemeType::File
            && self.path_len == 1
            && self.path.len() >= 3
            && is_windows_letter(self.path[1])
            && self.path[2] == b':'
        {
            return;
        }
        if self.path_len == 0 {
            return;
        }
        self.path_len -= 1;
        let cut = self.path.iter().rposition(|&c| c == b'/').unwrap_or(0);
        self.path.truncate(cut);
    }

    /// `lxb_url_path_append`: appends `/` + data.
    fn path_append(&mut self, data: &[u8]) {
        self.path.push(b'/');
        self.path.extend_from_slice(data);
    }

    /// `lxb_url_path_set_null`.
    pub(crate) fn path_set_null(&mut self) {
        self.path.clear();
        self.path_len = 0;
        self.path_opaque = false;
    }
}

/// Runs the basic URL parser.
///
/// * `url == None`: parse a fresh URL (leading/trailing C0-control-or-space
///   trimming applies) and return it.
/// * `url == Some(_)`: modify the URL in place starting at `state_override`.
pub(crate) fn parse_basic(
    log: &mut Log,
    url: Option<&mut Url>,
    base: Option<&Url>,
    input: &[u8],
    state_override: Option<State>,
) -> Result<Option<Url>, Fail> {
    let mut fresh = Url::default();
    let (url, is_fresh) = match url {
        Some(u) => (u, false),
        None => (&mut fresh, true),
    };
    let input = SharedBuf::new(input);
    let (mut start, mut end) = (0, input.len());
    if is_fresh {
        (start, end) = leading_trailing(log, &input);
    }
    let data = match remove_tab_newline(log, &input, start, end) {
        Some(buf) => {
            start = 0;
            end = buf.len();
            buf
        }
        None => input,
    };
    let mut p = Parser {
        log,
        data: &data,
        start,
        end,
        base,
        state_override,
    };
    p.run(url)?;
    Ok(if is_fresh { Some(fresh) } else { None })
}

/// `lxb_url_leading_trailing`: trims C0 controls and spaces.
fn leading_trailing(log: &mut Log, input: &SharedBuf) -> (usize, usize) {
    let mut p = 0;
    let end0 = input.len();
    while p < end0 {
        let c = input[p];
        if c > 0x1F && c < 0x80 && c != 0x20 {
            break;
        } else if c >= 0x80 {
            let tmp = p;
            let cp = decode_valid_utf8_single(input, &mut p);
            if cp > 0x1F && cp != DECODE_ERROR {
                p = tmp;
                break;
            }
            continue;
        }
        p += 1;
    }
    if p != 0 {
        log.push(input, 0, E::InvalidUrlUnit);
    }
    let mut end = end0;
    while end > p {
        let tmp = end;
        let cp = decode_valid_utf8_single_reverse(input, &mut end, p);
        if cp > 0x1F && cp != 0x20 && cp != DECODE_ERROR {
            end = tmp;
            break;
        }
    }
    if end != end0 {
        log.push(input, end, E::InvalidUrlUnit);
    }
    (p, end)
}

/// `lxb_url_remove_tab_newline`.
fn remove_tab_newline(
    log: &mut Log,
    input: &SharedBuf,
    start: usize,
    end: usize,
) -> Option<SharedBuf> {
    let slice = &input[start..end];
    let first = slice
        .iter()
        .position(|&c| matches!(c, b'\t' | b'\n' | b'\r'))?;
    log.push(input, start + first, E::InvalidUrlUnit);
    let cleaned: Vec<u8> = slice
        .iter()
        .copied()
        .filter(|c| !matches!(c, b'\t' | b'\n' | b'\r'))
        .collect();
    Some(SharedBuf::new(&cleaned))
}

struct Parser<'a, 'l> {
    log: &'l mut Log,
    data: &'a SharedBuf,
    start: usize,
    end: usize,
    base: Option<&'a Url>,
    state_override: Option<State>,
}

impl Parser<'_, '_> {
    fn err(&mut self, pos: usize, t: E) {
        self.log.push(self.data, pos, t);
    }

    fn at(&self, p: usize) -> u8 {
        if p < self.end {
            self.data[p]
        } else {
            0
        }
    }

    fn encode_append(&self, begin: usize, end: usize, dst: &mut Vec<u8>, set: u8) {
        percent_encode_into(&self.data[begin..end], dst, &URL_MAP, set, false);
    }

    /// Validates code points of a query/fragment/opaque path segment.
    fn check_url_units(&mut self, p: &mut usize) {
        let tmp = *p;
        let mut q = *p;
        let cp = decode_valid_utf8_single(&self.data[..self.end], &mut q);
        *p = q;
        let bad_pct = cp == u32::from(b'%')
            && (self.end - q < 2 || !is_hex(self.data[q]) || !is_hex(self.data[q + 1]));
        if (!is_url_codepoint(cp) && cp != u32::from(b'%')) || bad_pct {
            self.err(tmp, E::InvalidUrlUnit);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn run(&mut self, url: &mut Url) -> Result<(), Fail> {
        let ov = self.state_override;
        let end = self.end;
        let mut p = self.start;
        let mut state = ov.unwrap_or(State::SchemeStart);
        let mut inside_bracket = false;
        loop {
            match state {
                State::SchemeStart | State::Scheme => {
                    if state == State::SchemeStart
                        && (p >= end || !self.data[p].is_ascii_alphabetic())
                    {
                        if ov.is_none() {
                            state = State::NoScheme;
                            continue;
                        }
                        return Err(Fail);
                    }
                    loop {
                        p += 1;
                        if !(p < end
                            && (self.data[p].is_ascii_alphanumeric()
                                || matches!(self.data[p], b'+' | b'-' | b'.')))
                        {
                            break;
                        }
                    }
                    if p >= end || self.data[p] != b':' {
                        if ov.is_none() {
                            p = self.start;
                            state = State::NoScheme;
                            continue;
                        } else if p < end || ov != Some(State::SchemeStart) {
                            return Err(Fail);
                        }
                    }
                    let name = &self.data[self.start..p];
                    let schm = scheme_find(name);
                    if ov.is_some() {
                        if url.is_special() != schm.is_special() {
                            return Ok(());
                        }
                        if (url.port.is_some() || url.includes_credentials())
                            && schm == SchemeType::File
                        {
                            return Ok(());
                        }
                        if url.scheme_type == SchemeType::File && url.host == Host::Empty {
                            return Ok(());
                        }
                    }
                    url.scheme_type = schm;
                    url.scheme = name.to_ascii_lowercase();
                    p += 1;
                    if ov.is_some() {
                        if url.port.is_some() && url.port == schm.default_port() {
                            url.port = None;
                        }
                        return Ok(());
                    }
                    if schm == SchemeType::File {
                        if end - p.min(end) < 2 || self.data[p] != b'/' || self.data[p + 1] != b'/'
                        {
                            self.err(p, E::SpecialSchemeMissingFollowingSolidus);
                        }
                        state = State::File;
                        continue;
                    }
                    if schm.is_special() {
                        state = if self.base.is_some_and(|b| url.scheme_equals(b)) {
                            State::SpecialRelativeOrAuthority
                        } else {
                            State::SpecialAuthoritySlashes
                        };
                        continue;
                    }
                    if p < end && self.data[p] == b'/' {
                        p += 1;
                        state = State::PathOrAuthority;
                        continue;
                    }
                    url.path_set_null();
                    state = State::OpaquePath;
                }
                State::NoScheme => {
                    let Some(base) = self.base else {
                        self.err(p, E::MissingSchemeNonRelativeUrl);
                        return Err(Fail);
                    };
                    if base.path_opaque {
                        if p >= end || self.data[p] != b'#' {
                            self.err(p, E::MissingSchemeNonRelativeUrl);
                            return Err(Fail);
                        }
                        p += 1;
                        url.scheme.clone_from(&base.scheme);
                        url.scheme_type = base.scheme_type;
                        url.copy_path_from(base);
                        url.query.clone_from(&base.query);
                        url.fragment = None;
                        state = State::Fragment;
                        continue;
                    }
                    state = if base.scheme_type == SchemeType::File {
                        State::File
                    } else {
                        State::Relative
                    };
                }
                State::SpecialRelativeOrAuthority => {
                    if end - p > 1 && self.data[p] == b'/' && self.data[p + 1] == b'/' {
                        p += 2;
                        state = State::SpecialAuthorityIgnoreSlashes;
                        continue;
                    }
                    self.err(p, E::SpecialSchemeMissingFollowingSolidus);
                    state = State::Relative;
                }
                State::PathOrAuthority => {
                    if p < end && self.data[p] == b'/' {
                        p += 1;
                        state = State::Authority;
                    } else {
                        state = State::Path;
                    }
                }
                State::Relative => {
                    let base = self.base.ok_or(Fail)?;
                    url.scheme.clone_from(&base.scheme);
                    url.scheme_type = base.scheme_type;
                    if p < end {
                        if self.data[p] == b'/' {
                            p += 1;
                            state = State::RelativeSlash;
                            continue;
                        }
                        if url.is_special() && self.data[p] == b'\\' {
                            p += 1;
                            self.err(p, E::InvalidReverseSoldius);
                            state = State::RelativeSlash;
                            continue;
                        }
                    }
                    url.username.clone_from(&base.username);
                    url.password.clone_from(&base.password);
                    url.host = base.host.clone();
                    if base.port.is_some() {
                        url.port = base.port;
                    }
                    url.copy_path_from(base);
                    url.query.clone_from(&base.query);
                    if p >= end {
                        return Ok(());
                    }
                    match self.data[p] {
                        b'?' => {
                            p += 1;
                            url.query = None;
                            state = State::Query;
                        }
                        b'#' => {
                            p += 1;
                            url.fragment = None;
                            state = State::Fragment;
                        }
                        _ => {
                            url.query = None;
                            url.path_shorten();
                            state = State::Path;
                        }
                    }
                }
                State::RelativeSlash => {
                    let base = self.base.ok_or(Fail)?;
                    let c = self.at(p);
                    if url.is_special() && (c == b'/' || c == b'\\') {
                        if c == b'\\' {
                            self.err(p, E::InvalidReverseSoldius);
                        }
                        p += 1;
                        state = State::SpecialAuthorityIgnoreSlashes;
                        continue;
                    }
                    if c == b'/' {
                        p += 1;
                        state = State::Authority;
                        continue;
                    }
                    url.username.clone_from(&base.username);
                    url.password.clone_from(&base.password);
                    url.host = base.host.clone();
                    if base.port.is_some() {
                        url.port = base.port;
                    }
                    state = State::Path;
                }
                State::SpecialAuthoritySlashes => {
                    state = State::SpecialAuthorityIgnoreSlashes;
                    if end - p > 1 && self.data[p] == b'/' && self.data[p + 1] == b'/' {
                        p += 2;
                    } else {
                        self.err(p, E::SpecialSchemeMissingFollowingSolidus);
                    }
                }
                State::SpecialAuthorityIgnoreSlashes => {
                    if p >= end || (self.data[p] != b'/' && self.data[p] != b'\\') {
                        state = State::Authority;
                        continue;
                    }
                    self.err(p, E::SpecialSchemeMissingFollowingSolidus);
                    p += 1;
                }
                State::Authority => {
                    p = self.authority(url, p)?;
                    state = State::Host;
                }
                State::Host | State::Hostname => {
                    let begin = p;
                    if ov.is_some() && url.scheme_type == SchemeType::File {
                        state = State::FileHost;
                        continue;
                    }
                    let mut opt = HostOpt {
                        not_special: !url.is_special(),
                        ..HostOpt::default()
                    };
                    let mut to_port = false;
                    while p < end {
                        let c = self.data[p];
                        match c {
                            b'/' | b'?' | b'#' => break,
                            b'\\' if url.is_special() => break,
                            b':' if !inside_bracket => {
                                if p == begin {
                                    self.err(p, E::HostMissing);
                                    return Err(Fail);
                                }
                                if ov == Some(State::Hostname) {
                                    return Err(Fail);
                                }
                                url.host = parse_host(self.log, self.data, begin, p, opt)?;
                                p += 1;
                                to_port = true;
                                break;
                            }
                            b'[' => inside_bracket = true,
                            b']' => inside_bracket = false,
                            b'%' => opt.decode = true,
                            b'x' | b'X' => {
                                if p + 4 <= end
                                    && matches!(self.data[p + 1], b'n' | b'N')
                                    && self.data[p + 2] == b'-'
                                    && self.data[p + 3] == b'-'
                                {
                                    opt.idna = true;
                                    p += 3;
                                }
                            }
                            _ => {
                                if c >= 0x80 {
                                    opt.idna = true;
                                }
                            }
                        }
                        p += 1;
                    }
                    if to_port {
                        state = State::Port;
                        continue;
                    }
                    if begin == p && url.is_special() {
                        self.err(p, E::HostMissing);
                        return Err(Fail);
                    }
                    if ov.is_some()
                        && begin == p
                        && (url.includes_credentials() || url.port.is_some())
                    {
                        return Ok(());
                    }
                    url.host = parse_host(self.log, self.data, begin, p, opt)?;
                    if ov.is_some() {
                        return Ok(());
                    }
                    state = State::PathStart;
                }
                State::Port => {
                    let begin = p;
                    while p < end && self.data[p].is_ascii_digit() {
                        p += 1;
                    }
                    let c = self.at(p);
                    if p >= end
                        || matches!(c, b'/' | b'?' | b'#')
                        || (url.is_special() && c == b'\\')
                        || ov.is_some()
                    {
                        if begin == p {
                            if ov.is_some() {
                                return Err(Fail);
                            }
                            state = State::PathStart;
                            continue;
                        }
                        let mut port: u32 = 0;
                        for &d in &self.data[begin..p] {
                            port = u32::from(d - b'0') + port * 10;
                            if port > 65535 {
                                self.err(p, E::PortOutOfRange);
                                return Err(Fail);
                            }
                        }
                        let port = u16::try_from(port).map_err(|_| Fail)?;
                        if !url.is_special() || url.scheme_type.default_port() != Some(port) {
                            url.port = Some(port);
                        } else {
                            url.port = None;
                        }
                        if ov.is_some() {
                            return Ok(());
                        }
                        state = State::PathStart;
                        continue;
                    }
                    self.err(p, E::PortInvalid);
                    return Err(Fail);
                }
                State::File => {
                    url.scheme_type = SchemeType::File;
                    url.scheme = b"file".to_vec();
                    url.host = Host::Empty;
                    let c = self.at(p);
                    if c == b'/' || c == b'\\' {
                        if c == b'\\' {
                            self.err(p, E::InvalidReverseSoldius);
                        }
                        p += 1;
                        state = State::FileSlash;
                        continue;
                    }
                    if let Some(base) = self.base.filter(|b| b.scheme_type == SchemeType::File) {
                        url.host = base.host.clone();
                        url.copy_path_from(base);
                        url.query.clone_from(&base.query);
                        if c == b'?' {
                            p += 1;
                            url.query = None;
                            state = State::Query;
                            continue;
                        }
                        if c == b'#' {
                            p += 1;
                            url.fragment = None;
                            state = State::Fragment;
                            continue;
                        }
                        if p >= end {
                            return Ok(());
                        }
                        url.query = None;
                        if starts_with_windows_drive_letter(&self.data[p..end]) {
                            self.err(p, E::FileInvalidWindowsDriveLetter);
                            url.path_set_null();
                        } else {
                            url.path_shorten();
                        }
                    }
                    state = State::Path;
                }
                State::FileSlash => {
                    let c = self.at(p);
                    if c == b'/' || c == b'\\' {
                        if c == b'\\' {
                            self.err(p, E::InvalidReverseSoldius);
                        }
                        p += 1;
                        state = State::FileHost;
                        continue;
                    }
                    if let Some(base) = self.base.filter(|b| b.scheme_type == SchemeType::File) {
                        url.host = base.host.clone();
                        if !starts_with_windows_drive_letter(&self.data[p..end])
                            && !base.path_opaque
                            && base.path_len >= 1
                        {
                            let index = usize::from(base.path.first() == Some(&b'/'));
                            if let Some((seg_start, seg_len)) =
                                path_part_by_index(&base.path, index)
                            {
                                let seg = &base.path[seg_start..seg_start + seg_len];
                                if is_normalized_windows_drive_letter(seg) {
                                    url.path
                                        .extend_from_slice(&base.path[..seg_start + seg_len]);
                                    url.path_len += 1;
                                }
                            }
                        }
                    }
                    state = State::Path;
                }
                State::FileHost => {
                    let begin = p;
                    let mut opt = HostOpt {
                        not_special: !url.is_special(),
                        ..HostOpt::default()
                    };
                    while p < end {
                        match self.data[p] {
                            b'/' | b'\\' | b'?' | b'#' => break,
                            b'%' => opt.decode = true,
                            b'x' | b'X' => {
                                if p + 4 <= end
                                    && matches!(self.data[p + 1], b'n' | b'N')
                                    && self.data[p + 2] == b'-'
                                    && self.data[p + 3] == b'-'
                                {
                                    opt.idna = true;
                                    p += 3;
                                }
                            }
                            c => {
                                if c >= 0x80 {
                                    opt.idna = true;
                                }
                            }
                        }
                        p += 1;
                    }
                    if ov.is_none()
                        && p - begin == 2
                        && is_windows_drive_letter(&self.data[begin..p])
                    {
                        self.err(begin, E::FileInvalidWindowsDriveLetterHost);
                        p = begin;
                        state = State::Path;
                        continue;
                    }
                    if p == begin {
                        url.host = Host::Empty;
                        if ov.is_some() {
                            return Ok(());
                        }
                        state = State::PathStart;
                        continue;
                    }
                    url.host = parse_host(self.log, self.data, begin, p, opt)?;
                    if matches!(&url.host, Host::Domain(d) | Host::Opaque(d) if d == b"localhost") {
                        url.host = Host::Empty;
                    }
                    if ov.is_some() {
                        return Ok(());
                    }
                    state = State::PathStart;
                }
                State::PathStart => {
                    let c = self.at(p);
                    if url.is_special() {
                        if c == b'\\' {
                            self.err(p, E::InvalidReverseSoldius);
                            p += 1;
                        } else if c == b'/' {
                            p += 1;
                        }
                        state = State::Path;
                        continue;
                    }
                    if ov.is_none() {
                        if c == b'?' {
                            p += 1;
                            state = State::Query;
                            continue;
                        }
                        if c == b'#' {
                            p += 1;
                            state = State::Fragment;
                            continue;
                        }
                    }
                    if p < end {
                        if c == b'/' {
                            p += 1;
                        }
                        state = State::Path;
                        continue;
                    }
                    if ov.is_some() && url.host == Host::None {
                        url.path_append(b"");
                        url.path_len += 1;
                    }
                    return Ok(());
                }
                State::Path => {
                    p = self.path_fast_path(url, p, ov.is_none());
                    if p >= end {
                        return Ok(());
                    }
                    match self.data[p] {
                        b'?' => {
                            p += 1;
                            state = State::Query;
                        }
                        b'#' => {
                            p += 1;
                            state = State::Fragment;
                        }
                        _ => {}
                    }
                }
                State::OpaquePath => {
                    let begin = p;
                    url.path_opaque = true;
                    loop {
                        if p >= end {
                            let mut tmp = Vec::new();
                            self.encode_append(begin, p, &mut tmp, MAP_C0);
                            url.path.extend_from_slice(&tmp);
                            return Ok(());
                        }
                        let c = self.data[p];
                        if c == b'#' || c == b'?' {
                            let mut tmp = Vec::new();
                            self.encode_append(begin, p, &mut tmp, MAP_C0);
                            if p > begin && self.data[p - 1] == b' ' {
                                tmp.pop();
                                tmp.extend_from_slice(b"%20");
                            }
                            url.path.extend_from_slice(&tmp);
                            p += 1;
                            state = if c == b'#' {
                                State::Fragment
                            } else {
                                State::Query
                            };
                            break;
                        }
                        self.check_url_units(&mut p);
                    }
                }
                State::Query => {
                    let begin = p;
                    loop {
                        if p >= end || (ov.is_none() && self.data[p] == b'#') {
                            let set = if url.is_special() {
                                MAP_SPECIAL_QUERY
                            } else {
                                MAP_QUERY
                            };
                            let mut q = url.query.take().unwrap_or_default();
                            self.encode_append(begin, p, &mut q, set);
                            url.query = Some(q);
                            if p < end {
                                p += 1;
                                state = State::Fragment;
                                break;
                            }
                            return Ok(());
                        }
                        self.check_url_units(&mut p);
                    }
                }
                State::Fragment => {
                    let begin = p;
                    while p < end {
                        self.check_url_units(&mut p);
                    }
                    let mut f = url.fragment.take().unwrap_or_default();
                    self.encode_append(begin, p, &mut f, MAP_FRAGMENT);
                    url.fragment = Some(f);
                    return Ok(());
                }
            }
        }
    }

    /// The authority state: credentials up to the last `@`.
    fn authority(&mut self, url: &mut Url, mut p: usize) -> Result<usize, Fail> {
        let end = self.end;
        let mut begin = p;
        let mut pswd: Option<usize> = None;
        let mut at_sign = false;
        while p < end {
            let c = self.data[p];
            match c {
                b'@' => {
                    self.err(p, E::InvalidCredentials);
                    if p == begin {
                        at_sign = true;
                        p += 1;
                        continue;
                    }
                    let tmp = pswd.map_or(p, |pw| pw - 1);
                    if tmp > begin {
                        let mut u = std::mem::take(&mut url.username);
                        self.encode_append(begin, tmp, &mut u, MAP_USERINFO);
                        url.username = u;
                    }
                    if let Some(pw) = pswd {
                        if p > pw {
                            let mut w = std::mem::take(&mut url.password);
                            self.encode_append(pw, p, &mut w, MAP_USERINFO);
                            url.password = w;
                            pswd = Some(p);
                        }
                    }
                    at_sign = true;
                    begin = p;
                }
                b':' if pswd.is_none() => pswd = Some(p + 1),
                b'/' | b'?' | b'#' => break,
                b'\\' if url.is_special() => break,
                _ => {}
            }
            p += 1;
        }
        if at_sign {
            if begin == p || begin + 1 == p {
                self.err(p, E::HostMissing);
                return Err(Fail);
            }
            begin += 1;
        }
        Ok(begin)
    }

    #[allow(clippy::too_many_lines)]
    fn path_fast_path(&mut self, url: &mut Url, mut p: usize, bqs: bool) -> usize {
        let end = self.end;
        let special = url.is_special();
        let mut begin = p;
        let mut last = p;
        let mut count = url.path_len;
        let after_sep = |data: &[u8], p: usize, begin: usize| {
            p == begin || data[p - 1] == b'/' || (data[p - 1] == b'\\' && special)
        };
        while p < end {
            let c = self.data[p];
            if path_needs_care(c) {
                if c == b'/' {
                    count += 1;
                    last = p + 1;
                } else if c == b'%' {
                    if end - p < 3 || !is_hex(self.data[p + 1]) || !is_hex(self.data[p + 2]) {
                        return self.goto_slow(url, begin, last, count, end, bqs);
                    } else if self.data[p + 1] == b'2'
                        && matches!(self.data[p + 2], b'e' | b'E')
                        && after_sep(self.data, p, begin)
                    {
                        url.path_len = count;
                        self.path_try_dot(url, &mut begin, &mut last, &mut p, bqs);
                        count = url.path_len;
                    } else {
                        p += 2;
                    }
                } else if (c == b'?' || c == b'#') && bqs {
                    break;
                } else if c == b'\\' && special {
                    count += 1;
                    self.err(p, E::InvalidReverseSoldius);
                    url.path_append(&self.data[begin..p]);
                    begin = p + 1;
                    last = p + 1;
                } else if c == b'.' {
                    if after_sep(self.data, p, begin) {
                        url.path_len = count;
                        self.path_try_dot(url, &mut begin, &mut last, &mut p, bqs);
                        count = url.path_len;
                    }
                } else {
                    return self.goto_slow(url, begin, last, count, end, bqs);
                }
            }
            p += 1;
        }
        url.path_append(&self.data[begin..p]);
        url.path_len = count + 1;
        p
    }

    fn goto_slow(
        &mut self,
        url: &mut Url,
        begin: usize,
        last: usize,
        count: usize,
        end: usize,
        bqs: bool,
    ) -> usize {
        url.path_len = count;
        if last > begin {
            url.path_append(&self.data[begin..last - 1]);
        }
        self.path_slow_path(url, last, end, bqs)
    }

    fn path_try_dot(
        &mut self,
        url: &mut Url,
        begin: &mut usize,
        last: &mut usize,
        start: &mut usize,
        bqs: bool,
    ) {
        let end = self.end;
        let special = url.is_special();
        let mut p = *start;
        let mut count = 0;
        while p < end {
            let c = self.data[p];
            if c == b'/' || (c == b'\\' && special) || ((c == b'?' || c == b'#') && bqs) {
                break;
            } else if c == b'.' {
                count += 1;
            } else if c == b'%' {
                if p + 3 <= end
                    && self.data[p + 1] == b'2'
                    && matches!(self.data[p + 2], b'e' | b'E')
                {
                    count += 1;
                } else {
                    return;
                }
                p += 2;
            } else {
                return;
            }
            p += 1;
        }
        if count == 0 || count > 2 {
            return;
        }
        if *start > *begin {
            url.path_append(&self.data[*begin..*start - 1]);
        }
        if count == 2 {
            url.path_shorten();
        }
        if p >= end {
            *start = end - 1;
            *begin = end;
            *last = end;
            return;
        }
        if self.data[p] == b'?' || self.data[p] == b'#' {
            *start = p - 1;
            *begin = p;
            *last = p;
            return;
        }
        if self.data[p] == b'\\' {
            self.err(p, E::InvalidReverseSoldius);
        }
        *start = p;
        *begin = p + 1;
        *last = *begin;
    }

    #[allow(clippy::too_many_lines)]
    fn path_slow_path(&mut self, url: &mut Url, mut p: usize, end: usize, bqs: bool) -> usize {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let special = url.is_special();
        let mut sbuf: Vec<u8> = std::mem::take(&mut url.path);
        sbuf.push(b'/');
        let mut last = sbuf.len();
        let mut count = url.path_len;
        while p < end {
            let c = self.data[p];
            if c >= 0x80 {
                let tmp = p;
                let cp = decode_valid_utf8_single(&self.data[..end], &mut p);
                if !is_url_codepoint(cp) {
                    self.err(tmp, E::InvalidUrlUnit);
                }
                for &b in &self.data[tmp..p] {
                    sbuf.push(b'%');
                    sbuf.push(HEX[usize::from(b >> 4)]);
                    sbuf.push(HEX[usize::from(b & 15)]);
                }
                continue;
            }
            if c == b'/' || (c == b'\\' && special) {
                if c == b'\\' {
                    self.err(p, E::InvalidReverseSoldius);
                }
                sbuf.push(b'/');
                fix_windows_drive(url.scheme_type, &mut sbuf, last, count);
                count += 1;
                last = sbuf.len();
            } else if (c == b'?' || c == b'#') && bqs {
                break;
            } else if URL_MAP[usize::from(c)] & MAP_PATH != 0 {
                sbuf.push(b'%');
                sbuf.push(HEX[usize::from(c >> 4)]);
                sbuf.push(HEX[usize::from(c & 15)]);
                self.err(p, E::InvalidUrlUnit);
            } else if c == b'.' {
                if last == sbuf.len() {
                    let tmp = self.path_dot_count(url, p, &mut sbuf, &mut last, &mut count, bqs);
                    if tmp != p {
                        p = tmp;
                        if p < end && self.data[p] != b'?' && self.data[p] != b'#' {
                            p += 1;
                        }
                        continue;
                    }
                }
                sbuf.push(b'.');
            } else if c == b'%' {
                if end - p < 3 || !is_hex(self.data[p + 1]) || !is_hex(self.data[p + 2]) {
                    self.err(p, E::InvalidUrlUnit);
                } else if self.data[p + 1] == b'2'
                    && matches!(self.data[p + 2], b'e' | b'E')
                    && last == sbuf.len()
                {
                    let tmp = self.path_dot_count(url, p, &mut sbuf, &mut last, &mut count, bqs);
                    if tmp != p {
                        p = tmp;
                        if p < end && self.data[p] != b'?' && self.data[p] != b'#' {
                            p += 1;
                        }
                        continue;
                    }
                }
                sbuf.push(b'%');
            } else {
                if !is_ascii_url_cp(c) {
                    self.err(p, E::InvalidUrlUnit);
                }
                sbuf.push(c);
            }
            p += 1;
        }
        let len = sbuf.len();
        fix_windows_drive(url.scheme_type, &mut sbuf, last, count);
        debug_assert_eq!(len, sbuf.len());
        url.path_len = count + 1;
        url.path = sbuf;
        p
    }

    fn path_dot_count(
        &mut self,
        url: &Url,
        start: usize,
        sbuf: &mut Vec<u8>,
        last: &mut usize,
        path_count: &mut usize,
        bqs: bool,
    ) -> usize {
        let end = self.end;
        let special = url.is_special();
        let mut count = 0;
        let mut p = start;
        while p < end {
            let c = self.data[p];
            if c == b'/' || (c == b'\\' && special) || ((c == b'?' || c == b'#') && bqs) {
                break;
            } else if c == b'.' {
                count += 1;
            } else if c == b'%' {
                if p + 3 <= end
                    && self.data[p + 1] == b'2'
                    && matches!(self.data[p + 2], b'e' | b'E')
                {
                    count += 1;
                } else {
                    return start;
                }
                p += 2;
            } else {
                return start;
            }
            p += 1;
        }
        if count == 0 || count > 2 {
            return start;
        }
        if p < end && self.data[p] == b'\\' {
            self.err(p, E::InvalidReverseSoldius);
        }
        if url.scheme_type == SchemeType::File
            && *path_count == 1
            && *last >= 2
            && is_normalized_windows_drive_letter(&sbuf[1..*last - 1])
        {
            return p;
        }
        if count == 2 && *path_count > 0 {
            *path_count -= 1;
            let mut last_p = *last - 1;
            while last_p > 0 {
                last_p -= 1;
                if sbuf[last_p] == b'/' {
                    sbuf.truncate(last_p + 1);
                    break;
                }
            }
            *last = sbuf.len();
        }
        p
    }
}

/// `lxb_url_path_fix_windows_drive`: `C|` becomes `C:` in the first segment
/// of file URLs.
fn fix_windows_drive(scheme: SchemeType, sbuf: &mut [u8], seg_start: usize, count: usize) {
    let seg_end = sbuf.len();
    let len = seg_end - seg_start;
    if scheme == SchemeType::File
        && count == 0
        && ((len == 3 && sbuf[seg_end - 1] == b'/') || len == 2)
        && is_windows_drive_letter(&sbuf[seg_start..seg_end])
    {
        sbuf[seg_start + 1] = b':';
    }
}

/// `lxb_url_path_part_by_index`: the `index`-th `/`-terminated segment.
fn path_part_by_index(path: &[u8], mut index: usize) -> Option<(usize, usize)> {
    let mut begin = 0;
    for (i, &c) in path.iter().enumerate() {
        if c == b'/' {
            if index == 0 {
                return Some((begin, i - begin));
            }
            index -= 1;
            begin = i + 1;
        }
    }
    None
}
