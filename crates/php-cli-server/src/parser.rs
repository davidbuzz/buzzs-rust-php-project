// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A port of the request side of `sapi/cli/php_http_parser.c` (the
//! joyent `http_parser` fork the built-in server uses).
//!
//! The parser is incremental: [`Parser::execute`] consumes one buffer at a
//! time and reports the pieces of the request through a [`Handler`].  A
//! callback returning non-zero stops parsing; [`Parser::execute`] then
//! returns fewer bytes than it was given, which the server treats as a
//! malformed request (exactly as `php_cli_server_client_read_request`
//! compares `nbytes_consumed` with `nbytes_read`).

/// `PHP_HTTP_MAX_HEADER_SIZE`: the request line and headers may not be
/// longer than this.
pub const MAX_HEADER_SIZE: u32 = 80 * 1024;

/// `enum php_http_method`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[allow(missing_docs)]
pub enum Method {
    Delete,
    #[default]
    Get,
    Head,
    Post,
    Put,
    Patch,
    Query,
    Connect,
    Options,
    Trace,
    Copy,
    Lock,
    Mkcol,
    Move,
    Mkcalendar,
    Propfind,
    Proppatch,
    Search,
    Unlock,
    Report,
    Mkactivity,
    Checkout,
    Merge,
    MSearch,
    Notify,
    Subscribe,
    Unsubscribe,
    /// An unknown method (answered with 501).
    NotImplemented,
}

impl Method {
    /// `php_http_method_str()`.
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Delete => "DELETE",
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Query => "QUERY",
            Method::Connect => "CONNECT",
            Method::Options => "OPTIONS",
            Method::Trace => "TRACE",
            Method::Copy => "COPY",
            Method::Lock => "LOCK",
            Method::Mkcol => "MKCOL",
            Method::Move => "MOVE",
            Method::Mkcalendar => "MKCALENDAR",
            Method::Propfind => "PROPFIND",
            Method::Proppatch => "PROPPATCH",
            Method::Search => "SEARCH",
            Method::Unlock => "UNLOCK",
            Method::Report => "REPORT",
            Method::Mkactivity => "MKACTIVITY",
            Method::Checkout => "CHECKOUT",
            Method::Merge => "MERGE",
            Method::MSearch => "M-SEARCH",
            Method::Notify => "NOTIFY",
            Method::Subscribe => "SUBSCRIBE",
            Method::Unsubscribe => "UNSUBSCRIBE",
            Method::NotImplemented => "NOTIMPLEMENTED",
        }
    }
}

/// Parser states (`enum state`, request states only).  The order matters:
/// every state up to [`State::HeadersAlmostDone`] counts towards the
/// header size limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(missing_docs)]
pub enum State {
    Dead,
    StartReq,
    ReqMethod,
    ReqSpacesBeforeUrl,
    ReqSchema,
    ReqSchemaSlash,
    ReqSchemaSlashSlash,
    ReqHost,
    ReqPort,
    ReqPath,
    ReqQueryStringStart,
    ReqQueryString,
    ReqFragmentStart,
    ReqFragment,
    ReqHttpStart,
    ReqHttpH,
    ReqHttpHT,
    ReqHttpHTT,
    ReqHttpHTTP,
    ReqFirstHttpMajor,
    ReqHttpMajor,
    ReqFirstHttpMinor,
    ReqHttpMinor,
    ReqLineAlmostDone,
    HeaderFieldStart,
    HeaderField,
    HeaderValueStart,
    HeaderValue,
    HeaderAlmostDone,
    HeadersAlmostDone,
    ChunkSizeStart,
    ChunkSize,
    ChunkSizeAlmostDone,
    ChunkParameters,
    ChunkData,
    ChunkDataAlmostDone,
    ChunkDataDone,
    BodyIdentity,
    BodyIdentityEof,
}

/// `enum header_states`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HState {
    General,
    C,
    Co,
    Con,
    MatchingConnection,
    MatchingProxyConnection,
    MatchingContentLength,
    MatchingTransferEncoding,
    MatchingUpgrade,
    Connection,
    ContentLength,
    TransferEncoding,
    Upgrade,
    MatchingTransferEncodingChunked,
    MatchingConnectionKeepAlive,
    MatchingConnectionClose,
    TransferEncodingChunked,
    ConnectionKeepAlive,
    ConnectionClose,
}

const F_CHUNKED: u8 = 1 << 0;
const F_CONNECTION_KEEP_ALIVE: u8 = 1 << 1;
const F_CONNECTION_CLOSE: u8 = 1 << 2;
const F_TRAILING: u8 = 1 << 3;
const F_UPGRADE: u8 = 1 << 4;
const F_SKIPBODY: u8 = 1 << 5;

const CR: u8 = b'\r';
const LF: u8 = b'\n';

/// `tokens[]`: header-name characters, lower-cased (0 = invalid).
fn token(c: u8) -> u8 {
    match c {
        b' '
        | b'!'
        | b'"'
        | b'#'
        | b'$'
        | b'%'
        | b'&'
        | b'\''
        | b'*'
        | b'+'
        | b'-'
        | b'.'
        | b'/'
        | b'0'..=b'9'
        | b'a'..=b'z'
        | b'^'
        | b'_'
        | b'`'
        | b'|'
        | b'}'
        | b'~' => c,
        b'A'..=b'Z' => c | 0x20,
        _ => 0,
    }
}

/// `normal_url_char[]`.
fn normal_url_char(c: u8) -> bool {
    matches!(c, 0x21..=0x7e) && c != b'#' && c != b'?'
}

/// `unhex[]`.
fn unhex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `LOWER()`.
fn lower(c: u8) -> u8 {
    c | 0x20
}

/// The read-only parser fields callbacks may consult.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    /// The request method.
    pub method: Method,
    /// `http_major`.
    pub http_major: u16,
    /// `http_minor`.
    pub http_minor: u16,
    /// `content_length` (-1 when absent).
    pub content_length: i64,
}

/// The callbacks (`php_http_parser_settings`).  Data callbacks may be
/// called several times for one element (once per buffer it spans).
#[allow(unused_variables)]
pub trait Handler {
    /// `on_message_begin`.
    fn on_message_begin(&mut self) -> i32 {
        0
    }
    /// `on_path` (the path part of the URL).
    fn on_path(&mut self, data: &[u8]) -> i32 {
        0
    }
    /// `on_query_string`.
    fn on_query_string(&mut self, data: &[u8]) -> i32 {
        0
    }
    /// `on_url` (the whole request target).
    fn on_url(&mut self, info: &Info, data: &[u8]) -> i32 {
        0
    }
    /// `on_fragment`.
    fn on_fragment(&mut self, data: &[u8]) -> i32 {
        0
    }
    /// `on_header_field`.
    fn on_header_field(&mut self, data: &[u8]) -> i32 {
        0
    }
    /// `on_header_value`.
    fn on_header_value(&mut self, data: &[u8]) -> i32 {
        0
    }
    /// `on_headers_complete`: 1 = no body follows, other non-zero values
    /// are errors.
    fn on_headers_complete(&mut self, info: &Info) -> i32 {
        0
    }
    /// `on_body`.
    fn on_body(&mut self, info: &Info, data: &[u8]) -> i32 {
        0
    }
    /// `on_message_complete`.
    fn on_message_complete(&mut self, info: &Info) -> i32 {
        0
    }
}

/// `php_http_parser` in request mode.
#[derive(Debug, Clone)]
pub struct Parser {
    flags: u8,
    state: State,
    header_state: HState,
    index: u32,
    nread: u32,
    content_length: i64,
    /// `http_major`.
    pub http_major: u16,
    /// `http_minor`.
    pub http_minor: u16,
    /// The request method.
    pub method: Method,
    /// Set for `CONNECT` and `Upgrade:` requests.
    pub upgrade: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

/// Which element a pending mark belongs to.
#[derive(Debug, Clone, Copy, Default)]
struct Marks {
    header_field: Option<usize>,
    header_value: Option<usize>,
    fragment: Option<usize>,
    query_string: Option<usize>,
    path: Option<usize>,
    url: Option<usize>,
}

/// The method matcher string (`method_strings[]`) as bytes.
fn method_bytes(m: Method) -> &'static [u8] {
    m.as_str().as_bytes()
}

impl Parser {
    /// `php_http_parser_init(PHP_HTTP_REQUEST)`.
    pub fn new() -> Self {
        Parser {
            flags: 0,
            state: State::StartReq,
            header_state: HState::General,
            index: 0,
            nread: 0,
            content_length: -1,
            http_major: 0,
            http_minor: 0,
            method: Method::Delete,
            upgrade: false,
        }
    }

    /// The current state (`parser->state`).
    pub fn state(&self) -> State {
        self.state
    }

    fn info(&self) -> Info {
        Info {
            method: self.method,
            http_major: self.http_major,
            http_minor: self.http_minor,
            content_length: self.content_length,
        }
    }

    /// `php_http_should_keep_alive()`.
    pub fn should_keep_alive(&self) -> bool {
        if self.http_major > 0 && self.http_minor > 0 {
            self.flags & F_CONNECTION_CLOSE == 0
        } else {
            self.flags & F_CONNECTION_KEEP_ALIVE != 0
        }
    }

    /// `php_http_parser_execute()`: parses `data` and returns the number of
    /// bytes consumed (less than `data.len()` on error).
    #[allow(
        clippy::too_many_lines,
        clippy::cognitive_complexity,
        clippy::many_single_char_names
    )]
    pub fn execute<H: Handler + ?Sized>(&mut self, h: &mut H, data: &[u8]) -> usize {
        let len = data.len();
        let mut state = self.state;
        let mut hs = self.header_state;
        let mut index = self.index;
        let mut nread = self.nread;
        let mut m = Marks::default();

        if len == 0 {
            if state == State::BodyIdentityEof {
                let info = self.info();
                h.on_message_complete(&info);
            }
            return 0;
        }

        match state {
            State::HeaderField => m.header_field = Some(0),
            State::HeaderValue => m.header_value = Some(0),
            State::ReqFragment => m.fragment = Some(0),
            State::ReqQueryString => m.query_string = Some(0),
            State::ReqPath => m.path = Some(0),
            _ => {}
        }
        if matches!(
            state,
            State::ReqPath
                | State::ReqSchema
                | State::ReqSchemaSlash
                | State::ReqSchemaSlashSlash
                | State::ReqPort
                | State::ReqQueryStringStart
                | State::ReqQueryString
                | State::ReqHost
                | State::ReqFragmentStart
                | State::ReqFragment
        ) {
            m.url = Some(0);
        }

        // Callback helpers mirroring the C macros.
        macro_rules! cb_data {
            ($mark:ident, $method:ident, $p:expr) => {
                if let Some(start) = m.$mark.take() {
                    if 0 != h.$method(&data[start..$p]) {
                        return $p;
                    }
                }
            };
        }
        macro_rules! cb_url {
            ($p:expr) => {
                if let Some(start) = m.url.take() {
                    let info = self.info();
                    if 0 != h.on_url(&info, &data[start..$p]) {
                        return $p;
                    }
                }
            };
        }
        macro_rules! error {
            ($p:expr) => {{
                self.state = State::Dead;
                return $p;
            }};
        }

        let mut p = 0usize;
        while p < len {
            let ch = data[p];
            if state <= State::HeadersAlmostDone && self.flags & F_TRAILING == 0 {
                nread += 1;
                if nread > MAX_HEADER_SIZE {
                    error!(p);
                }
            }

            // `goto header_almost_done` / `goto headers_almost_done`
            // re-dispatch the same character in another state.
            'dispatch: loop {
                match state {
                    State::Dead => error!(p),
                    State::StartReq => {
                        if ch == CR || ch == LF {
                            break 'dispatch;
                        }
                        self.flags = 0;
                        self.content_length = -1;
                        if 0 != h.on_message_begin() {
                            return p;
                        }
                        if !ch.is_ascii_uppercase() {
                            error!(p);
                        }
                        index = 1;
                        self.method = match ch {
                            b'C' => Method::Connect,
                            b'D' => Method::Delete,
                            b'G' => Method::Get,
                            b'H' => Method::Head,
                            b'L' => Method::Lock,
                            b'M' => Method::Mkcol,
                            b'N' => Method::Notify,
                            b'O' => Method::Options,
                            b'P' => Method::Post,
                            b'Q' => Method::Query,
                            b'R' => Method::Report,
                            b'S' => Method::Subscribe,
                            b'T' => Method::Trace,
                            b'U' => Method::Unlock,
                            _ => Method::NotImplemented,
                        };
                        state = State::ReqMethod;
                    }
                    State::ReqMethod => {
                        if ch == 0 {
                            error!(p);
                        }
                        let matcher = method_bytes(self.method);
                        let idx = index as usize;
                        let at = matcher.get(idx).copied().unwrap_or(0);
                        if ch == b' ' {
                            if self.method != Method::NotImplemented && at != 0 {
                                self.method = Method::NotImplemented;
                            }
                            state = State::ReqSpacesBeforeUrl;
                        } else if self.method == Method::NotImplemented || ch == at {
                            // nada
                        } else if self.method == Method::Connect {
                            self.method = match (index, ch) {
                                (1, b'H') => Method::Checkout,
                                (2, b'P') => Method::Copy,
                                _ => Method::NotImplemented,
                            };
                        } else if self.method == Method::Mkcol {
                            self.method = match (index, ch) {
                                (1, b'O') => Method::Move,
                                (3, b'A') => Method::Mkcalendar,
                                (1, b'E') => Method::Merge,
                                (1, b'-') => Method::MSearch,
                                (2, b'A') => Method::Mkactivity,
                                _ => Method::NotImplemented,
                            };
                        } else if index == 1 && self.method == Method::Post && ch == b'R' {
                            self.method = Method::Propfind;
                        } else if index == 1 && self.method == Method::Post && ch == b'U' {
                            self.method = Method::Put;
                        } else if index == 1 && self.method == Method::Post && ch == b'A' {
                            self.method = Method::Patch;
                        } else if index == 1 && self.method == Method::Subscribe && ch == b'E' {
                            self.method = Method::Search;
                        } else if index == 2 && self.method == Method::Unlock && ch == b'S' {
                            self.method = Method::Unsubscribe;
                        } else if index == 4 && self.method == Method::Propfind && ch == b'P' {
                            self.method = Method::Proppatch;
                        } else {
                            self.method = Method::NotImplemented;
                        }
                        index += 1;
                    }
                    State::ReqSpacesBeforeUrl => {
                        if ch == b' ' {
                            break 'dispatch;
                        }
                        if ch == b'/' || ch == b'*' {
                            m.url = Some(p);
                            m.path = Some(p);
                            state = State::ReqPath;
                            break 'dispatch;
                        }
                        if lower(ch).is_ascii_lowercase() {
                            m.url = Some(p);
                            state = State::ReqSchema;
                            break 'dispatch;
                        }
                        error!(p);
                    }
                    State::ReqSchema => {
                        if lower(ch).is_ascii_lowercase() {
                            break 'dispatch;
                        }
                        if ch == b':' {
                            state = State::ReqSchemaSlash;
                        } else if ch == b'.' || ch.is_ascii_digit() {
                            state = State::ReqHost;
                        } else {
                            error!(p);
                        }
                    }
                    State::ReqSchemaSlash => state = State::ReqSchemaSlashSlash,
                    State::ReqSchemaSlashSlash => state = State::ReqHost,
                    State::ReqHost => {
                        if lower(ch).is_ascii_lowercase()
                            || ch.is_ascii_digit()
                            || ch == b'.'
                            || ch == b'-'
                        {
                            break 'dispatch;
                        }
                        match ch {
                            b':' => state = State::ReqPort,
                            b'/' => {
                                m.path = Some(p);
                                state = State::ReqPath;
                            }
                            b' ' => {
                                cb_url!(p);
                                state = State::ReqHttpStart;
                            }
                            _ => error!(p),
                        }
                    }
                    State::ReqPort => {
                        if ch.is_ascii_digit() {
                            break 'dispatch;
                        }
                        match ch {
                            b'/' => {
                                m.path = Some(p);
                                state = State::ReqPath;
                            }
                            b' ' => {
                                cb_url!(p);
                                state = State::ReqHttpStart;
                            }
                            _ => error!(p),
                        }
                    }
                    State::ReqPath => {
                        if normal_url_char(ch) {
                            break 'dispatch;
                        }
                        match ch {
                            b' ' => {
                                cb_url!(p);
                                cb_data!(path, on_path, p);
                                state = State::ReqHttpStart;
                            }
                            CR | LF => {
                                cb_url!(p);
                                cb_data!(path, on_path, p);
                                self.http_major = 0;
                                self.http_minor = 9;
                                state = if ch == CR {
                                    State::ReqLineAlmostDone
                                } else {
                                    State::HeaderFieldStart
                                };
                            }
                            b'?' => {
                                cb_data!(path, on_path, p);
                                state = State::ReqQueryStringStart;
                            }
                            b'#' => {
                                cb_data!(path, on_path, p);
                                state = State::ReqFragmentStart;
                            }
                            _ => error!(p),
                        }
                    }
                    State::ReqQueryStringStart => {
                        if normal_url_char(ch) {
                            m.query_string = Some(p);
                            state = State::ReqQueryString;
                            break 'dispatch;
                        }
                        match ch {
                            b'?' => {}
                            b' ' => {
                                cb_url!(p);
                                state = State::ReqHttpStart;
                            }
                            CR | LF => {
                                cb_url!(p);
                                self.http_major = 0;
                                self.http_minor = 9;
                                state = if ch == CR {
                                    State::ReqLineAlmostDone
                                } else {
                                    State::HeaderFieldStart
                                };
                            }
                            b'#' => state = State::ReqFragmentStart,
                            _ => error!(p),
                        }
                    }
                    State::ReqQueryString => {
                        if normal_url_char(ch) {
                            break 'dispatch;
                        }
                        match ch {
                            b'?' => {}
                            b' ' => {
                                cb_url!(p);
                                cb_data!(query_string, on_query_string, p);
                                state = State::ReqHttpStart;
                            }
                            CR | LF => {
                                cb_url!(p);
                                cb_data!(query_string, on_query_string, p);
                                self.http_major = 0;
                                self.http_minor = 9;
                                state = if ch == CR {
                                    State::ReqLineAlmostDone
                                } else {
                                    State::HeaderFieldStart
                                };
                            }
                            b'#' => {
                                cb_data!(query_string, on_query_string, p);
                                state = State::ReqFragmentStart;
                            }
                            _ => error!(p),
                        }
                    }
                    State::ReqFragmentStart => {
                        if normal_url_char(ch) {
                            m.fragment = Some(p);
                            state = State::ReqFragment;
                            break 'dispatch;
                        }
                        match ch {
                            b' ' => {
                                cb_url!(p);
                                state = State::ReqHttpStart;
                            }
                            CR | LF => {
                                cb_url!(p);
                                self.http_major = 0;
                                self.http_minor = 9;
                                state = if ch == CR {
                                    State::ReqLineAlmostDone
                                } else {
                                    State::HeaderFieldStart
                                };
                            }
                            b'?' => {
                                m.fragment = Some(p);
                                state = State::ReqFragment;
                            }
                            b'#' => {}
                            _ => error!(p),
                        }
                    }
                    State::ReqFragment => {
                        if normal_url_char(ch) {
                            break 'dispatch;
                        }
                        match ch {
                            b' ' => {
                                cb_url!(p);
                                cb_data!(fragment, on_fragment, p);
                                state = State::ReqHttpStart;
                            }
                            CR | LF => {
                                cb_url!(p);
                                cb_data!(fragment, on_fragment, p);
                                self.http_major = 0;
                                self.http_minor = 9;
                                state = if ch == CR {
                                    State::ReqLineAlmostDone
                                } else {
                                    State::HeaderFieldStart
                                };
                            }
                            b'?' | b'#' => {}
                            _ => error!(p),
                        }
                    }
                    State::ReqHttpStart => match ch {
                        b'H' => state = State::ReqHttpH,
                        b' ' => {}
                        _ => error!(p),
                    },
                    State::ReqHttpH => state = State::ReqHttpHT,
                    State::ReqHttpHT => state = State::ReqHttpHTT,
                    State::ReqHttpHTT => state = State::ReqHttpHTTP,
                    State::ReqHttpHTTP => state = State::ReqFirstHttpMajor,
                    State::ReqFirstHttpMajor => {
                        if !(b'1'..=b'9').contains(&ch) {
                            error!(p);
                        }
                        self.http_major = u16::from(ch - b'0');
                        state = State::ReqHttpMajor;
                    }
                    State::ReqHttpMajor => {
                        if ch == b'.' {
                            state = State::ReqFirstHttpMinor;
                            break 'dispatch;
                        }
                        if !ch.is_ascii_digit() {
                            error!(p);
                        }
                        self.http_major = self.http_major * 10 + u16::from(ch - b'0');
                        if self.http_major > 999 {
                            error!(p);
                        }
                    }
                    State::ReqFirstHttpMinor => {
                        if !ch.is_ascii_digit() {
                            error!(p);
                        }
                        self.http_minor = u16::from(ch - b'0');
                        state = State::ReqHttpMinor;
                    }
                    State::ReqHttpMinor => {
                        if ch == CR {
                            state = State::ReqLineAlmostDone;
                            break 'dispatch;
                        }
                        if ch == LF {
                            state = State::HeaderFieldStart;
                            break 'dispatch;
                        }
                        if !ch.is_ascii_digit() {
                            error!(p);
                        }
                        self.http_minor = self.http_minor * 10 + u16::from(ch - b'0');
                        if self.http_minor > 999 {
                            error!(p);
                        }
                    }
                    State::ReqLineAlmostDone => {
                        if ch != LF {
                            error!(p);
                        }
                        state = State::HeaderFieldStart;
                    }
                    State::HeaderFieldStart => {
                        if ch == CR {
                            state = State::HeadersAlmostDone;
                            break 'dispatch;
                        }
                        if ch == LF {
                            state = State::HeadersAlmostDone;
                            continue 'dispatch;
                        }
                        let c = token(ch);
                        if c == 0 {
                            error!(p);
                        }
                        m.header_field = Some(p);
                        index = 0;
                        state = State::HeaderField;
                        hs = match c {
                            b'c' => HState::C,
                            b'p' => HState::MatchingProxyConnection,
                            b't' => HState::MatchingTransferEncoding,
                            b'u' => HState::MatchingUpgrade,
                            _ => HState::General,
                        };
                    }
                    State::HeaderField => {
                        let c = token(ch);
                        if c != 0 {
                            hs = header_field_step(hs, &mut index, c, ch);
                            break 'dispatch;
                        }
                        if ch == b':' {
                            cb_data!(header_field, on_header_field, p);
                            state = State::HeaderValueStart;
                            break 'dispatch;
                        }
                        if ch == CR {
                            state = State::HeaderAlmostDone;
                            cb_data!(header_field, on_header_field, p);
                            break 'dispatch;
                        }
                        if ch == LF {
                            cb_data!(header_field, on_header_field, p);
                            state = State::HeaderFieldStart;
                            break 'dispatch;
                        }
                        error!(p);
                    }
                    State::HeaderValueStart => {
                        if ch == b' ' {
                            break 'dispatch;
                        }
                        m.header_value = Some(p);
                        state = State::HeaderValue;
                        index = 0;
                        let c = lower(ch);
                        if ch == CR {
                            cb_data!(header_value, on_header_value, p);
                            hs = HState::General;
                            state = State::HeaderAlmostDone;
                            break 'dispatch;
                        }
                        if ch == LF {
                            cb_data!(header_value, on_header_value, p);
                            state = State::HeaderFieldStart;
                            break 'dispatch;
                        }
                        match hs {
                            HState::Upgrade => {
                                self.flags |= F_UPGRADE;
                                hs = HState::General;
                            }
                            HState::TransferEncoding => {
                                hs = if c == b'c' {
                                    HState::MatchingTransferEncodingChunked
                                } else {
                                    HState::General
                                };
                            }
                            HState::ContentLength => {
                                if !ch.is_ascii_digit() {
                                    error!(p);
                                }
                                self.content_length = i64::from(ch - b'0');
                            }
                            HState::Connection => {
                                hs = match c {
                                    b'k' => HState::MatchingConnectionKeepAlive,
                                    b'c' => HState::MatchingConnectionClose,
                                    _ => HState::General,
                                };
                            }
                            _ => hs = HState::General,
                        }
                    }
                    State::HeaderValue => {
                        let c = lower(ch);
                        if ch == CR {
                            cb_data!(header_value, on_header_value, p);
                            state = State::HeaderAlmostDone;
                            break 'dispatch;
                        }
                        if ch == LF {
                            cb_data!(header_value, on_header_value, p);
                            state = State::HeaderAlmostDone;
                            continue 'dispatch;
                        }
                        match hs {
                            HState::General => {}
                            HState::ContentLength => {
                                if ch == b' ' {
                                    break 'dispatch;
                                }
                                if !ch.is_ascii_digit() {
                                    error!(p);
                                }
                                let d = i64::from(ch - b'0');
                                if self.content_length > (i64::MAX - d) / 10 {
                                    error!(p);
                                }
                                self.content_length = self.content_length * 10 + d;
                            }
                            HState::MatchingTransferEncodingChunked => {
                                hs = match_word(
                                    &mut index,
                                    c,
                                    b"chunked",
                                    hs,
                                    HState::TransferEncodingChunked,
                                );
                            }
                            HState::MatchingConnectionKeepAlive => {
                                hs = match_word(
                                    &mut index,
                                    c,
                                    b"keep-alive",
                                    hs,
                                    HState::ConnectionKeepAlive,
                                );
                            }
                            HState::MatchingConnectionClose => {
                                hs = match_word(
                                    &mut index,
                                    c,
                                    b"close",
                                    hs,
                                    HState::ConnectionClose,
                                );
                            }
                            HState::TransferEncodingChunked
                            | HState::ConnectionKeepAlive
                            | HState::ConnectionClose => {
                                if ch != b' ' {
                                    hs = HState::General;
                                }
                            }
                            _ => {
                                state = State::HeaderValue;
                                hs = HState::General;
                            }
                        }
                    }
                    State::HeaderAlmostDone => {
                        state = State::HeaderFieldStart;
                        match hs {
                            HState::ConnectionKeepAlive => self.flags |= F_CONNECTION_KEEP_ALIVE,
                            HState::ConnectionClose => self.flags |= F_CONNECTION_CLOSE,
                            HState::TransferEncodingChunked => self.flags |= F_CHUNKED,
                            _ => {}
                        }
                    }
                    State::HeadersAlmostDone => {
                        if self.flags & F_TRAILING != 0 {
                            let info = self.info();
                            if 0 != h.on_message_complete(&info) {
                                return p;
                            }
                            state = State::StartReq;
                            break 'dispatch;
                        }
                        nread = 0;
                        if self.flags & F_UPGRADE != 0 || self.method == Method::Connect {
                            self.upgrade = true;
                        }
                        let info = self.info();
                        match h.on_headers_complete(&info) {
                            0 => {}
                            1 => self.flags |= F_SKIPBODY,
                            _ => error!(p),
                        }
                        if self.flags & F_SKIPBODY != 0 {
                            let info = self.info();
                            if 0 != h.on_message_complete(&info) {
                                return p;
                            }
                            state = State::StartReq;
                        } else if self.flags & F_CHUNKED != 0 {
                            state = State::ChunkSizeStart;
                        } else if self.content_length == 0 {
                            let info = self.info();
                            if 0 != h.on_message_complete(&info) {
                                return p;
                            }
                            state = State::StartReq;
                        } else if self.content_length > 0 {
                            state = State::BodyIdentity;
                        } else {
                            // A request without Content-Length has no body.
                            let info = self.info();
                            if 0 != h.on_message_complete(&info) {
                                return p;
                            }
                            state = State::StartReq;
                        }
                    }
                    State::BodyIdentity => {
                        let avail = len - p;
                        let to_read =
                            avail.min(usize::try_from(self.content_length).unwrap_or(usize::MAX));
                        if to_read > 0 {
                            let info = self.info();
                            if 0 != h.on_body(&info, &data[p..p + to_read]) {
                                error!(p);
                            }
                            p += to_read - 1;
                            self.content_length -= i64::try_from(to_read).unwrap_or(0);
                            if self.content_length == 0 {
                                let info = self.info();
                                if 0 != h.on_message_complete(&info) {
                                    return p;
                                }
                                state = State::StartReq;
                            }
                        }
                    }
                    State::BodyIdentityEof => {
                        let to_read = len - p;
                        if to_read > 0 {
                            let info = self.info();
                            if 0 != h.on_body(&info, &data[p..]) {
                                error!(p);
                            }
                            p += to_read - 1;
                        }
                    }
                    State::ChunkSizeStart => {
                        let Some(c) = unhex(ch) else { error!(p) };
                        self.content_length = i64::from(c);
                        state = State::ChunkSize;
                    }
                    State::ChunkSize => {
                        if ch == CR {
                            state = State::ChunkSizeAlmostDone;
                            break 'dispatch;
                        }
                        let Some(c) = unhex(ch) else {
                            if ch == b';' || ch == b' ' {
                                state = State::ChunkParameters;
                                break 'dispatch;
                            }
                            error!(p);
                        };
                        let c = i64::from(c);
                        if self.content_length > (i64::MAX - c) / 16 {
                            error!(p);
                        }
                        self.content_length = self.content_length * 16 + c;
                    }
                    State::ChunkParameters => {
                        if ch == CR {
                            state = State::ChunkSizeAlmostDone;
                        }
                    }
                    State::ChunkSizeAlmostDone => {
                        if self.content_length == 0 {
                            self.flags |= F_TRAILING;
                            state = State::HeaderFieldStart;
                        } else {
                            state = State::ChunkData;
                        }
                    }
                    State::ChunkData => {
                        let avail = len - p;
                        let cl = usize::try_from(self.content_length).unwrap_or(usize::MAX);
                        let to_read = avail.min(cl);
                        if to_read > 0 {
                            let info = self.info();
                            if 0 != h.on_body(&info, &data[p..p + to_read]) {
                                error!(p);
                            }
                            p += to_read - 1;
                        }
                        if to_read == cl {
                            state = State::ChunkDataAlmostDone;
                        }
                        self.content_length -= i64::try_from(to_read).unwrap_or(0);
                    }
                    State::ChunkDataAlmostDone => state = State::ChunkDataDone,
                    State::ChunkDataDone => state = State::ChunkSizeStart,
                }
                break 'dispatch;
            }
            p += 1;
        }

        // CALLBACK_NOCLEAR for elements spanning the buffer end.
        let end = len;
        if let Some(s) = m.header_field {
            if 0 != h.on_header_field(&data[s..end]) {
                return end;
            }
        }
        if let Some(s) = m.header_value {
            if 0 != h.on_header_value(&data[s..end]) {
                return end;
            }
        }
        if let Some(s) = m.fragment {
            if 0 != h.on_fragment(&data[s..end]) {
                return end;
            }
        }
        if let Some(s) = m.query_string {
            if 0 != h.on_query_string(&data[s..end]) {
                return end;
            }
        }
        if let Some(s) = m.path {
            if 0 != h.on_path(&data[s..end]) {
                return end;
            }
        }
        if let Some(s) = m.url {
            let info = self.info();
            if 0 != h.on_url(&info, &data[s..end]) {
                return end;
            }
        }

        self.state = state;
        self.header_state = hs;
        self.index = index;
        self.nread = nread;
        len
    }
}

/// One step of matching a header name against the special names.
fn header_field_step(hs: HState, index: &mut u32, c: u8, ch: u8) -> HState {
    match hs {
        HState::C => {
            *index += 1;
            if c == b'o' {
                HState::Co
            } else {
                HState::General
            }
        }
        HState::Co => {
            *index += 1;
            if c == b'n' {
                HState::Con
            } else {
                HState::General
            }
        }
        HState::Con => {
            *index += 1;
            match c {
                b'n' => HState::MatchingConnection,
                b't' => HState::MatchingContentLength,
                _ => HState::General,
            }
        }
        HState::MatchingConnection => match_word(index, c, b"connection", hs, HState::Connection),
        HState::MatchingProxyConnection => {
            match_word(index, c, b"proxy-connection", hs, HState::Connection)
        }
        HState::MatchingContentLength => {
            match_word(index, c, b"content-length", hs, HState::ContentLength)
        }
        HState::MatchingTransferEncoding => {
            match_word(index, c, b"transfer-encoding", hs, HState::TransferEncoding)
        }
        HState::MatchingUpgrade => match_word(index, c, b"upgrade", hs, HState::Upgrade),
        HState::Connection | HState::ContentLength | HState::TransferEncoding | HState::Upgrade => {
            if ch == b' ' {
                hs
            } else {
                HState::General
            }
        }
        _ => HState::General,
    }
}

/// The `h_matching_*` step: advances `index` and compares `c` with
/// `word[index]`; reaching the last character yields `done`.
fn match_word(index: &mut u32, c: u8, word: &[u8], current: HState, done: HState) -> HState {
    *index += 1;
    let i = *index as usize;
    if i > word.len() || word.get(i).copied().unwrap_or(0) != c {
        HState::General
    } else if i == word.len() - 1 {
        done
    } else {
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default, Debug)]
    struct Rec {
        url: Vec<u8>,
        path: Vec<u8>,
        query: Vec<u8>,
        fragment: Vec<u8>,
        headers: Vec<(Vec<u8>, Vec<u8>)>,
        last_field: bool,
        body: Vec<u8>,
        complete: u32,
        method: Option<Method>,
        version: (u16, u16),
    }

    impl Handler for Rec {
        fn on_path(&mut self, d: &[u8]) -> i32 {
            self.path.extend_from_slice(d);
            0
        }
        fn on_query_string(&mut self, d: &[u8]) -> i32 {
            self.query.extend_from_slice(d);
            0
        }
        fn on_url(&mut self, info: &Info, d: &[u8]) -> i32 {
            self.method = Some(info.method);
            self.url.extend_from_slice(d);
            0
        }
        fn on_fragment(&mut self, d: &[u8]) -> i32 {
            self.fragment.extend_from_slice(d);
            0
        }
        fn on_header_field(&mut self, d: &[u8]) -> i32 {
            if self.last_field {
                if let Some(l) = self.headers.last_mut() {
                    l.0.extend_from_slice(d);
                }
            } else {
                self.headers.push((d.to_vec(), Vec::new()));
            }
            self.last_field = true;
            0
        }
        fn on_header_value(&mut self, d: &[u8]) -> i32 {
            if let Some(l) = self.headers.last_mut() {
                l.1.extend_from_slice(d);
            }
            self.last_field = false;
            0
        }
        fn on_body(&mut self, _i: &Info, d: &[u8]) -> i32 {
            self.body.extend_from_slice(d);
            0
        }
        fn on_message_complete(&mut self, info: &Info) -> i32 {
            self.version = (info.http_major, info.http_minor);
            self.complete += 1;
            0
        }
    }

    fn parse_all(chunks: &[&[u8]]) -> (Rec, bool) {
        let mut p = Parser::new();
        let mut r = Rec::default();
        let mut ok = true;
        for c in chunks {
            if p.execute(&mut r, c) != c.len() {
                ok = false;
                break;
            }
        }
        (r, ok)
    }

    #[test]
    fn simple_get() {
        let (r, ok) = parse_all(&[b"GET /a/b?x=1&y#frag HTTP/1.1\r\nHost: h\r\nX-A:  v \r\n\r\n"]);
        assert!(ok);
        assert_eq!(r.method, Some(Method::Get));
        assert_eq!(r.url, b"/a/b?x=1&y#frag");
        assert_eq!(r.path, b"/a/b");
        assert_eq!(r.query, b"x=1&y");
        assert_eq!(r.fragment, b"frag");
        assert_eq!(
            r.headers,
            vec![
                (b"Host".to_vec(), b"h".to_vec()),
                (b"X-A".to_vec(), b"v ".to_vec())
            ]
        );
        assert_eq!(r.complete, 1);
        assert_eq!(r.version, (1, 1));
    }

    #[test]
    fn bare_lf_and_split_buffers() {
        let req: &[u8] = b"POST /p HTTP/1.0\nContent-Length: 3\nHost:x\n\nabc";
        for split in 1..req.len() {
            let (r, ok) = parse_all(&[&req[..split], &req[split..]]);
            assert!(ok, "split at {split}");
            assert_eq!(r.path, b"/p");
            assert_eq!(r.body, b"abc");
            assert_eq!(r.complete, 1);
            assert_eq!(r.headers.len(), 2, "split at {split}: {:?}", r.headers);
            assert_eq!(r.headers[1], (b"Host".to_vec(), b"x".to_vec()));
        }
    }

    #[test]
    fn chunked_body() {
        let (r, ok) = parse_all(&[
            b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n",
        ]);
        assert!(ok);
        assert_eq!(r.body, b"abcde");
        assert_eq!(r.complete, 1);
    }

    #[test]
    fn methods() {
        for (line, m) in [
            (&b"PUT / HTTP/1.1\r\n\r\n"[..], Method::Put),
            (b"PATCH / HTTP/1.1\r\n\r\n", Method::Patch),
            (b"PROPPATCH / HTTP/1.1\r\n\r\n", Method::Proppatch),
            (b"M-SEARCH / HTTP/1.1\r\n\r\n", Method::MSearch),
            (b"SMURF / HTTP/1.1\r\n\r\n", Method::NotImplemented),
            (b"GETX / HTTP/1.1\r\n\r\n", Method::NotImplemented),
            (b"GE / HTTP/1.1\r\n\r\n", Method::NotImplemented),
            (b"QUERY / HTTP/1.1\r\n\r\n", Method::Query),
        ] {
            let (r, ok) = parse_all(&[line]);
            assert!(ok);
            assert_eq!(r.method, Some(m), "{}", String::from_utf8_lossy(line));
        }
    }

    #[test]
    fn malformed() {
        assert!(!parse_all(&[b"get / HTTP/1.1\r\n\r\n"]).1);
        assert!(!parse_all(&[b"GET / HTTP/x\r\n\r\n"]).1);
        assert!(!parse_all(&[b"GET / HTTP/1.1\r\nContent-Length: x\r\n\r\n"]).1);
        assert!(!parse_all(&[b"\x16\x03\x01"]).1);
        assert!(!parse_all(&[b"GET /\x01 HTTP/1.1\r\n\r\n"]).1);
    }

    #[test]
    fn http09_request_line() {
        let (r, ok) = parse_all(&[b"GET /x\r\n\r\n"]);
        assert!(ok);
        assert_eq!(r.version, (0, 9));
    }

    #[test]
    fn absolute_url() {
        let (r, ok) = parse_all(&[b"GET http://example.com:80/x?q HTTP/1.1\r\n\r\n"]);
        assert!(ok);
        assert_eq!(r.url, b"http://example.com:80/x?q");
        assert_eq!(r.path, b"/x");
        assert_eq!(r.query, b"q");
    }

    #[test]
    fn header_size_limit() {
        let mut req = b"GET / HTTP/1.1\r\nX: ".to_vec();
        req.extend(std::iter::repeat_n(b'a', 81 * 1024));
        assert!(!parse_all(&[&req]).1);
    }

    #[test]
    fn keep_alive() {
        let mut p = Parser::new();
        let mut r = Rec::default();
        p.execute(&mut r, b"GET / HTTP/1.1\r\nConnection: close\r\n\r\n");
        assert!(!p.should_keep_alive());
        let mut p = Parser::new();
        p.execute(&mut r, b"GET / HTTP/1.0\r\nConnection: keep-alive\r\n\r\n");
        assert!(p.should_keep_alive());
    }
}
