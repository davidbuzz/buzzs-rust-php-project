// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Safe binding of the system Oniguruma library (`libonig`), the regex
//! engine behind `mb_ereg*` (ref: `ext/mbstring/php_mbregex.c`; API of
//! `oniguruma.h` 6.9.x).
//!
//! PHP calls Oniguruma directly on the subject bytes in the regex encoding
//! (`mb_regex_encoding()`), so offsets are byte offsets into the original
//! string and no transcoding takes place. This module exposes exactly the
//! calls `php_mbregex.c` makes:
//!
//! * [`Regex::new`] — `onig_new` with PHP's option bits
//!   ([`crate::mbregex::OPTION_IGNORECASE`] …), an encoding from
//!   [`crate::mbregex::ENC_NAME_MAP`] (by index) and a [`Syntax`]; errors
//!   carry `onig_error_code_to_str`'s text;
//! * [`Regex::search`] / [`Regex::match_at`] — `onig_search_with_param` /
//!   `onig_match_with_param` with a match-stack and retry limit
//!   (`mbstring.regex_stack_limit` / `mbstring.regex_retry_limit`);
//! * [`Regex::names`] — `onig_foreach_name` (Oniguruma's own name order);
//! * [`Regex::options`], [`Regex::noname_group_capture_is_active`].
//!
//! The development headers are not needed: the few declarations used are
//! written out in the private `ffi` module (checked against `oniguruma.h`
//! 6.9.9/6.9.10 and the symbols `libonig.so.5` exports). `build.rs` links
//! the library.

use std::ffi::{c_int, c_void, CStr};
use std::fmt;
use std::ptr::{self, NonNull};
use std::sync::Once;

use crate::mbregex::Syntax;

/// Raw declarations from `oniguruma.h` (6.9.x).
///
/// `unsafe` is needed to call into C; every call site in this file states
/// why its arguments are valid. The encoding and syntax tables are
/// declared `safe`: only their addresses are taken (the types are opaque
/// to Rust), which never reads them.
#[allow(non_camel_case_types, non_upper_case_globals, dead_code)]
mod ffi {
    use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void};

    /// `OnigEncodingType` (opaque here).
    #[repr(C)]
    pub struct OnigEncodingType {
        _private: [u8; 0],
    }

    /// `OnigSyntaxType` (opaque here).
    #[repr(C)]
    pub struct OnigSyntaxType {
        _private: [u8; 0],
    }

    /// `struct re_pattern_buffer` / `regex_t` (opaque).
    #[repr(C)]
    pub struct OnigRegexType {
        _private: [u8; 0],
    }

    /// `OnigMatchParam` (opaque).
    #[repr(C)]
    pub struct OnigMatchParam {
        _private: [u8; 0],
    }

    /// `struct re_registers` / `OnigRegion`.
    #[repr(C)]
    pub struct OnigRegion {
        pub allocated: c_int,
        pub num_regs: c_int,
        pub beg: *mut c_int,
        pub end: *mut c_int,
        /// `OnigCaptureTreeNode* history_root`.
        pub history_root: *mut c_void,
    }

    /// `OnigErrorInfo`.
    #[repr(C)]
    pub struct OnigErrorInfo {
        pub enc: *mut OnigEncodingType,
        pub par: *mut u8,
        pub par_end: *mut u8,
    }

    /// `ONIG_NORMAL`.
    pub const ONIG_NORMAL: c_int = 0;
    /// `ONIG_MISMATCH`.
    pub const ONIG_MISMATCH: c_int = -1;
    /// `ONIGERR_MEMORY`.
    pub const ONIGERR_MEMORY: c_int = -5;
    /// `ONIG_MAX_ERROR_MESSAGE_LEN`.
    pub const ONIG_MAX_ERROR_MESSAGE_LEN: usize = 90;

    /// The `onig_foreach_name` callback.
    pub type NameFunc = extern "C" fn(
        name: *const u8,
        name_end: *const u8,
        ngroup_num: c_int,
        group_nums: *mut c_int,
        reg: *mut OnigRegexType,
        arg: *mut c_void,
    ) -> c_int;

    unsafe extern "C" {
        pub safe static OnigEncodingASCII: OnigEncodingType;
        pub safe static OnigEncodingISO_8859_1: OnigEncodingType;
        pub safe static OnigEncodingISO_8859_2: OnigEncodingType;
        pub safe static OnigEncodingISO_8859_3: OnigEncodingType;
        pub safe static OnigEncodingISO_8859_4: OnigEncodingType;
        pub safe static OnigEncodingISO_8859_5: OnigEncodingType;
        pub safe static OnigEncodingISO_8859_6: OnigEncodingType;
        pub safe static OnigEncodingISO_8859_7: OnigEncodingType;
        pub safe static OnigEncodingISO_8859_8: OnigEncodingType;
        pub safe static OnigEncodingISO_8859_9: OnigEncodingType;
        pub safe static OnigEncodingISO_8859_10: OnigEncodingType;
        pub safe static OnigEncodingISO_8859_11: OnigEncodingType;
        pub safe static OnigEncodingISO_8859_13: OnigEncodingType;
        pub safe static OnigEncodingISO_8859_14: OnigEncodingType;
        pub safe static OnigEncodingISO_8859_15: OnigEncodingType;
        pub safe static OnigEncodingISO_8859_16: OnigEncodingType;
        pub safe static OnigEncodingUTF8: OnigEncodingType;
        pub safe static OnigEncodingUTF16_BE: OnigEncodingType;
        pub safe static OnigEncodingUTF16_LE: OnigEncodingType;
        pub safe static OnigEncodingUTF32_BE: OnigEncodingType;
        pub safe static OnigEncodingUTF32_LE: OnigEncodingType;
        pub safe static OnigEncodingEUC_JP: OnigEncodingType;
        pub safe static OnigEncodingEUC_TW: OnigEncodingType;
        pub safe static OnigEncodingEUC_KR: OnigEncodingType;
        pub safe static OnigEncodingEUC_CN: OnigEncodingType;
        pub safe static OnigEncodingSJIS: OnigEncodingType;
        pub safe static OnigEncodingKOI8_R: OnigEncodingType;
        pub safe static OnigEncodingBIG5: OnigEncodingType;

        pub safe static OnigSyntaxPosixBasic: OnigSyntaxType;
        pub safe static OnigSyntaxPosixExtended: OnigSyntaxType;
        pub safe static OnigSyntaxEmacs: OnigSyntaxType;
        pub safe static OnigSyntaxGrep: OnigSyntaxType;
        pub safe static OnigSyntaxGnuRegex: OnigSyntaxType;
        pub safe static OnigSyntaxJava: OnigSyntaxType;
        pub safe static OnigSyntaxPerl: OnigSyntaxType;
        pub safe static OnigSyntaxRuby: OnigSyntaxType;

        pub fn onig_init() -> c_int;
        pub fn onig_version() -> *const c_char;
        pub fn onig_error_code_to_str(s: *mut u8, err_code: c_int, ...) -> c_int;
        pub fn onig_new(
            reg: *mut *mut OnigRegexType,
            pattern: *const u8,
            pattern_end: *const u8,
            option: c_uint,
            enc: *mut OnigEncodingType,
            syntax: *mut OnigSyntaxType,
            einfo: *mut OnigErrorInfo,
        ) -> c_int;
        pub fn onig_free(reg: *mut OnigRegexType);
        pub fn onig_search_with_param(
            reg: *mut OnigRegexType,
            str: *const u8,
            end: *const u8,
            start: *const u8,
            range: *const u8,
            region: *mut OnigRegion,
            option: c_uint,
            mp: *mut OnigMatchParam,
        ) -> c_int;
        pub fn onig_match_with_param(
            reg: *mut OnigRegexType,
            str: *const u8,
            end: *const u8,
            at: *const u8,
            region: *mut OnigRegion,
            option: c_uint,
            mp: *mut OnigMatchParam,
        ) -> c_int;
        pub fn onig_region_new() -> *mut OnigRegion;
        pub fn onig_region_free(region: *mut OnigRegion, free_self: c_int);
        pub fn onig_foreach_name(
            reg: *mut OnigRegexType,
            func: NameFunc,
            arg: *mut c_void,
        ) -> c_int;
        pub fn onig_number_of_names(reg: *mut OnigRegexType) -> c_int;
        pub fn onig_noname_group_capture_is_active(reg: *mut OnigRegexType) -> c_int;
        pub fn onig_get_options(reg: *mut OnigRegexType) -> c_uint;
        pub fn onig_new_match_param() -> *mut OnigMatchParam;
        pub fn onig_free_match_param(p: *mut OnigMatchParam);
        pub fn onig_initialize_match_param(mp: *mut OnigMatchParam) -> c_int;
        pub fn onig_set_match_stack_limit_size_of_match_param(
            param: *mut OnigMatchParam,
            limit: c_uint,
        ) -> c_int;
        pub fn onig_set_retry_limit_in_match_of_match_param(
            param: *mut OnigMatchParam,
            limit: c_ulong,
        ) -> c_int;
    }
}

/// `PHP_MINIT_FUNCTION(mb_regex)`: `onig_init()`, once per process.
fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // SAFETY: `onig_init` takes no arguments and only sets up the
        // library's global tables; `Once` runs it before any other call.
        unsafe {
            ffi::onig_init();
        }
    });
}

/// The linked library's version (`onig_version()`, e.g. `"6.9.9"`), as
/// `MB_ONIGURUMA_VERSION` reports it.
pub fn version() -> String {
    // SAFETY: `onig_version` returns a pointer to a static NUL-terminated
    // string (or, defensively, null).
    let p = unsafe { ffi::onig_version() };
    if p.is_null() {
        return String::new();
    }
    // SAFETY: non-null, NUL-terminated and static (see above).
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// The `OnigEncoding` of an [`crate::mbregex::ENC_NAME_MAP`] index.
fn encoding(mbctype: usize) -> *mut ffi::OnigEncodingType {
    let enc: *const ffi::OnigEncodingType = match mbctype {
        0 => &raw const ffi::OnigEncodingEUC_JP,
        1 => &raw const ffi::OnigEncodingUTF8,
        2 => &raw const ffi::OnigEncodingUTF16_BE,
        3 => &raw const ffi::OnigEncodingUTF16_LE,
        4 => &raw const ffi::OnigEncodingUTF32_BE,
        5 => &raw const ffi::OnigEncodingUTF32_LE,
        6 => &raw const ffi::OnigEncodingSJIS,
        7 => &raw const ffi::OnigEncodingBIG5,
        8 => &raw const ffi::OnigEncodingEUC_CN,
        9 => &raw const ffi::OnigEncodingEUC_TW,
        10 => &raw const ffi::OnigEncodingEUC_KR,
        11 => &raw const ffi::OnigEncodingKOI8_R,
        12 => &raw const ffi::OnigEncodingISO_8859_1,
        13 => &raw const ffi::OnigEncodingISO_8859_2,
        14 => &raw const ffi::OnigEncodingISO_8859_3,
        15 => &raw const ffi::OnigEncodingISO_8859_4,
        16 => &raw const ffi::OnigEncodingISO_8859_5,
        17 => &raw const ffi::OnigEncodingISO_8859_6,
        18 => &raw const ffi::OnigEncodingISO_8859_7,
        19 => &raw const ffi::OnigEncodingISO_8859_8,
        20 => &raw const ffi::OnigEncodingISO_8859_9,
        21 => &raw const ffi::OnigEncodingISO_8859_10,
        22 => &raw const ffi::OnigEncodingISO_8859_11,
        23 => &raw const ffi::OnigEncodingISO_8859_13,
        24 => &raw const ffi::OnigEncodingISO_8859_14,
        25 => &raw const ffi::OnigEncodingISO_8859_15,
        26 => &raw const ffi::OnigEncodingISO_8859_16,
        // 27: "ASCII".
        _ => &raw const ffi::OnigEncodingASCII,
    };
    enc.cast_mut()
}

/// The `OnigSyntaxType` of a [`Syntax`].
fn syntax_type(syntax: Syntax) -> *mut ffi::OnigSyntaxType {
    let s: *const ffi::OnigSyntaxType = match syntax {
        Syntax::Java => &raw const ffi::OnigSyntaxJava,
        Syntax::GnuRegex => &raw const ffi::OnigSyntaxGnuRegex,
        Syntax::Grep => &raw const ffi::OnigSyntaxGrep,
        Syntax::Emacs => &raw const ffi::OnigSyntaxEmacs,
        Syntax::Ruby => &raw const ffi::OnigSyntaxRuby,
        Syntax::Perl => &raw const ffi::OnigSyntaxPerl,
        Syntax::PosixBasic => &raw const ffi::OnigSyntaxPosixBasic,
        Syntax::PosixExtended => &raw const ffi::OnigSyntaxPosixExtended,
    };
    s.cast_mut()
}

/// `onig_error_code_to_str` (with the compile-time error info when given).
fn error_message(code: c_int, einfo: Option<&mut ffi::OnigErrorInfo>) -> Vec<u8> {
    // One spare byte: Oniguruma NUL-terminates within
    // ONIG_MAX_ERROR_MESSAGE_LEN, the extra byte only guards the search.
    let mut buf = [0u8; ffi::ONIG_MAX_ERROR_MESSAGE_LEN + 1];
    match einfo {
        // SAFETY: `buf` holds ONIG_MAX_ERROR_MESSAGE_LEN bytes as the API
        // requires; `e` is the error info `onig_new` filled, whose `par`
        // range points into the pattern the caller still holds.
        Some(e) => unsafe {
            ffi::onig_error_code_to_str(buf.as_mut_ptr(), code, ptr::from_mut(e));
        },
        // SAFETY: as above; codes reported by searches take no parameter.
        None => unsafe {
            ffi::onig_error_code_to_str(buf.as_mut_ptr(), code);
        },
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf[..len].to_vec()
}

/// An Oniguruma error code returned by a search (`<= -2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnigError(pub i32);

impl OnigError {
    /// `ONIGERR_MATCH_STACK_LIMIT_OVER`.
    pub const MATCH_STACK_LIMIT_OVER: Self = Self(-15);
    /// `ONIGERR_RETRY_LIMIT_IN_MATCH_OVER`.
    pub const RETRY_LIMIT_IN_MATCH_OVER: Self = Self(-17);
    /// `ONIG_NO_SUPPORT_CONFIG` (`err = -2` in `mb_split`).
    pub const NO_SUPPORT_CONFIG: Self = Self(-2);

    /// The error text (`onig_error_code_to_str`), e.g.
    /// `"match-stack limit over"`.
    pub fn message(self) -> Vec<u8> {
        error_message(self.0, None)
    }
}

/// Per-search limits (`OnigMatchParam`); `None` keeps the library default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MatchLimits {
    /// `onig_set_match_stack_limit_size_of_match_param` (0 = unlimited).
    pub stack: Option<u32>,
    /// `onig_set_retry_limit_in_match_of_match_param` (0 = unlimited).
    pub retry: Option<u32>,
}

/// Match registers: `(beg, end)` byte offsets per group, `None` for a
/// group that did not participate (`ONIG_REGION_NOTPOS`).
pub type Captures = Vec<Option<(usize, usize)>>;

/// An owned `OnigMatchParam`.
struct MatchParam(NonNull<ffi::OnigMatchParam>);

impl MatchParam {
    fn new(limits: MatchLimits) -> Result<Self, OnigError> {
        // SAFETY: no arguments; the result is checked for null.
        let raw = unsafe { ffi::onig_new_match_param() };
        let mp = NonNull::new(raw).ok_or(OnigError(ffi::ONIGERR_MEMORY))?;
        // SAFETY: `mp` is a live match param owned by this function (and
        // then by the returned value, which frees it on drop).
        unsafe {
            ffi::onig_initialize_match_param(mp.as_ptr());
            if let Some(n) = limits.stack {
                ffi::onig_set_match_stack_limit_size_of_match_param(mp.as_ptr(), n);
            }
            if let Some(n) = limits.retry {
                ffi::onig_set_retry_limit_in_match_of_match_param(mp.as_ptr(), n.into());
            }
        }
        Ok(Self(mp))
    }
}

impl Drop for MatchParam {
    fn drop(&mut self) {
        // SAFETY: allocated by `onig_new_match_param`, freed exactly once.
        unsafe { ffi::onig_free_match_param(self.0.as_ptr()) }
    }
}

/// An owned `OnigRegion`.
struct Region(NonNull<ffi::OnigRegion>);

impl Region {
    fn new() -> Result<Self, OnigError> {
        // SAFETY: no arguments; the result is checked for null.
        let raw = unsafe { ffi::onig_region_new() };
        NonNull::new(raw)
            .map(Self)
            .ok_or(OnigError(ffi::ONIGERR_MEMORY))
    }

    /// The registers after a successful search.
    fn captures(&self) -> Captures {
        // SAFETY: the region is live and only read here.
        let r = unsafe { self.0.as_ref() };
        let n = usize::try_from(r.num_regs).unwrap_or(0);
        if n == 0 || r.beg.is_null() || r.end.is_null() {
            return Vec::new();
        }
        // SAFETY: after a match Oniguruma sizes `beg`/`end` to at least
        // `num_regs` entries.
        let (beg, end) = unsafe {
            (
                std::slice::from_raw_parts(r.beg, n),
                std::slice::from_raw_parts(r.end, n),
            )
        };
        beg.iter()
            .zip(end)
            .map(|(&b, &e)| match (usize::try_from(b), usize::try_from(e)) {
                (Ok(b), Ok(e)) => Some((b, e)),
                _ => None,
            })
            .collect()
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        // SAFETY: allocated by `onig_region_new`, freed exactly once
        // together with its register arrays (`free_self = 1`).
        unsafe { ffi::onig_region_free(self.0.as_ptr(), 1) }
    }
}

/// Group names in `onig_foreach_name` order with their group numbers.
pub type NameList = Vec<(Vec<u8>, Vec<usize>)>;

/// `onig_foreach_name` callback: appends one name to the `NameList`
/// behind `arg`.
extern "C" fn collect_name(
    name: *const u8,
    name_end: *const u8,
    ngroup_num: c_int,
    group_nums: *mut c_int,
    _reg: *mut ffi::OnigRegexType,
    arg: *mut c_void,
) -> c_int {
    if arg.is_null() {
        return 0;
    }
    // SAFETY: `arg` is the `&mut NameList` that `Regex::names` passes and
    // that outlives the `onig_foreach_name` call.
    let out = unsafe { &mut *arg.cast::<NameList>() };
    let len = if name.is_null() || name_end.is_null() {
        0
    } else {
        // SAFETY: `name..name_end` is one name inside the regex's name
        // table (same allocation).
        usize::try_from(unsafe { name_end.offset_from(name) }).unwrap_or(0)
    };
    let name = if len == 0 {
        Vec::new()
    } else {
        // SAFETY: `len` bytes from `name` are the name (see above).
        unsafe { std::slice::from_raw_parts(name, len) }.to_vec()
    };
    let n = usize::try_from(ngroup_num).unwrap_or(0);
    let groups = if n == 0 || group_nums.is_null() {
        Vec::new()
    } else {
        // SAFETY: Oniguruma passes the name's `ngroup_num` group numbers.
        unsafe { std::slice::from_raw_parts(group_nums, n) }
            .iter()
            .filter_map(|&g| usize::try_from(g).ok())
            .collect()
    };
    out.push((name, groups));
    0
}

/// A compiled Oniguruma regex (`php_mb_regex_t`).
pub struct Regex {
    raw: NonNull<ffi::OnigRegexType>,
    mbctype: usize,
    syntax: Syntax,
}

impl fmt::Debug for Regex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Regex")
            .field("mbctype", &self.mbctype)
            .field("syntax", &self.syntax)
            .field("options", &self.options())
            .finish_non_exhaustive()
    }
}

impl Drop for Regex {
    fn drop(&mut self) {
        // SAFETY: created by `onig_new`, freed exactly once.
        unsafe { ffi::onig_free(self.raw.as_ptr()) }
    }
}

impl Regex {
    /// `onig_new(pattern, options, encoding, syntax)`; `Err` is the
    /// `onig_error_code_to_str` message PHP prints after
    /// `"mbregex compile err: "`. `mbctype` indexes
    /// [`crate::mbregex::ENC_NAME_MAP`].
    pub fn new(
        pattern: &[u8],
        options: u32,
        mbctype: usize,
        syntax: Syntax,
    ) -> Result<Self, Vec<u8>> {
        init();
        let mut raw: *mut ffi::OnigRegexType = ptr::null_mut();
        let mut einfo = ffi::OnigErrorInfo {
            enc: ptr::null_mut(),
            par: ptr::null_mut(),
            par_end: ptr::null_mut(),
        };
        let range = pattern.as_ptr_range();
        // SAFETY: `range` spans the live pattern slice; `raw` and `einfo`
        // are valid out-pointers; encoding and syntax point to the
        // library's static tables.
        let r = unsafe {
            ffi::onig_new(
                &mut raw,
                range.start,
                range.end,
                options,
                encoding(mbctype),
                syntax_type(syntax),
                &mut einfo,
            )
        };
        if r != ffi::ONIG_NORMAL {
            return Err(error_message(r, Some(&mut einfo)));
        }
        NonNull::new(raw)
            .map(|raw| Self {
                raw,
                mbctype,
                syntax,
            })
            .ok_or_else(|| error_message(ffi::ONIGERR_MEMORY, None))
    }

    /// The [`crate::mbregex::ENC_NAME_MAP`] index it was compiled for
    /// (`onig_get_encoding`).
    pub fn mbctype(&self) -> usize {
        self.mbctype
    }

    /// The syntax it was compiled with (`onig_get_syntax`).
    pub fn syntax(&self) -> Syntax {
        self.syntax
    }

    /// `onig_get_options`: the compile options, including the syntax's
    /// default options that `onig_new` merges in.
    pub fn options(&self) -> u32 {
        // SAFETY: `raw` is a live regex.
        unsafe { ffi::onig_get_options(self.raw.as_ptr()) }
    }

    /// `onig_noname_group_capture_is_active`: whether plain `(...)` groups
    /// capture (they do not once the pattern has named groups).
    pub fn noname_group_capture_is_active(&self) -> bool {
        // SAFETY: `raw` is a live regex.
        unsafe { ffi::onig_noname_group_capture_is_active(self.raw.as_ptr()) != 0 }
    }

    /// `onig_foreach_name`: every group name with its group numbers, in
    /// the order Oniguruma visits them (empty without named groups).
    pub fn names(&self) -> NameList {
        let mut out = NameList::new();
        // SAFETY: `raw` is a live regex.
        if unsafe { ffi::onig_number_of_names(self.raw.as_ptr()) } <= 0 {
            return out;
        }
        // SAFETY: `collect_name` only uses `arg` as the `NameList` passed
        // here, which outlives the call.
        unsafe {
            ffi::onig_foreach_name(
                self.raw.as_ptr(),
                collect_name,
                ptr::from_mut(&mut out).cast::<c_void>(),
            );
        }
        out
    }

    /// `onig_search_with_param(reg, str, end, str + start, end, region, 0,
    /// mp)`: `Ok(None)` is `ONIG_MISMATCH` (also for `start > len`, which
    /// Oniguruma reports as a mismatch), `Err` an error such as
    /// [`OnigError::MATCH_STACK_LIMIT_OVER`].
    pub fn search(
        &self,
        s: &[u8],
        start: usize,
        limits: MatchLimits,
    ) -> Result<Option<Captures>, OnigError> {
        if start > s.len() {
            return Ok(None);
        }
        let region = Region::new()?;
        let mp = MatchParam::new(limits)?;
        let range = s.as_ptr_range();
        let at = s[start..].as_ptr();
        // SAFETY: `range` spans the live subject and `at` lies within it
        // (or at its end); region and match param are live and owned here.
        let r = unsafe {
            ffi::onig_search_with_param(
                self.raw.as_ptr(),
                range.start,
                range.end,
                at,
                range.end,
                region.0.as_ptr(),
                0,
                mp.0.as_ptr(),
            )
        };
        match r {
            ffi::ONIG_MISMATCH => Ok(None),
            r if r < 0 => Err(OnigError(r)),
            _ => Ok(Some(region.captures())),
        }
    }

    /// `onig_match_with_param(reg, str, end, str + at, NULL, 0, mp)`: the
    /// match length at `at`, `Ok(None)` on a mismatch.
    pub fn match_at(
        &self,
        s: &[u8],
        at: usize,
        limits: MatchLimits,
    ) -> Result<Option<usize>, OnigError> {
        if at > s.len() {
            return Ok(None);
        }
        let mp = MatchParam::new(limits)?;
        let range = s.as_ptr_range();
        let start = s[at..].as_ptr();
        // SAFETY: as in `search`; a null region is allowed.
        let r = unsafe {
            ffi::onig_match_with_param(
                self.raw.as_ptr(),
                range.start,
                range.end,
                start,
                ptr::null_mut(),
                0,
                mp.0.as_ptr(),
            )
        };
        match r {
            ffi::ONIG_MISMATCH => Ok(None),
            r if r < 0 => Err(OnigError(r)),
            n => Ok(usize::try_from(n).ok()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mbregex::{name2mbctype, OPTION_IGNORECASE, OPTION_MULTILINE, OPTION_SINGLELINE};

    const DEFAULT: u32 = OPTION_MULTILINE | OPTION_SINGLELINE;

    fn utf8() -> usize {
        name2mbctype(b"UTF-8").unwrap()
    }

    fn re(p: &str) -> Regex {
        Regex::new(p.as_bytes(), DEFAULT, utf8(), Syntax::Ruby).unwrap()
    }

    #[test]
    fn encoding_table_follows_enc_name_map() {
        use crate::mbregex::ENC_NAME_MAP;
        assert_eq!(ENC_NAME_MAP.len(), 28);
        for (name, idx) in [
            ("EUC-JP", 0),
            ("UTF-8", 1),
            ("UTF-16", 2),
            ("UTF-16LE", 3),
            ("UCS-4", 4),
            ("UCS-4LE", 5),
            ("SJIS", 6),
            ("BIG5", 7),
            ("EUC-CN", 8),
            ("EUC-TW", 9),
            ("EUC-KR", 10),
            ("KOI8-R", 11),
            ("ISO-8859-1", 12),
            ("ISO-8859-11", 22),
            ("ISO-8859-13", 23),
            ("ISO-8859-16", 26),
            ("ASCII", 27),
        ] {
            assert_eq!(name2mbctype(name.as_bytes()), Some(idx), "{name}");
        }
        // UTF-16LE: "a" is 61 00; a big-endian pattern would not match it.
        let le = Regex::new(b"a\0", DEFAULT, 3, Syntax::Ruby).unwrap();
        assert!(le
            .search(b"a\0", 0, MatchLimits::default())
            .unwrap()
            .is_some());
    }

    #[test]
    fn reports_version() {
        assert!(version().starts_with("6."), "{}", version());
    }

    #[test]
    fn searches_with_byte_offsets() {
        let r = re("(b)(x)?");
        let caps = r
            .search(b"abc", 0, MatchLimits::default())
            .unwrap()
            .unwrap();
        assert_eq!(caps, vec![Some((1, 2)), Some((1, 2)), None]);
        assert_eq!(r.search(b"abc", 2, MatchLimits::default()), Ok(None));
        assert_eq!(r.search(b"abc", 4, MatchLimits::default()), Ok(None));
        assert!(r.noname_group_capture_is_active());
        assert!(r.names().is_empty());
    }

    #[test]
    fn matches_in_the_regex_encoding() {
        let euc = name2mbctype(b"EUC-JP").unwrap();
        // HIRAGANA A in EUC-JP is A4 A2; `.` consumes the whole character.
        let r = Regex::new(b"a.b", DEFAULT, euc, Syntax::Ruby).unwrap();
        let caps = r
            .search(b"xa\xA4\xA2b", 0, MatchLimits::default())
            .unwrap()
            .unwrap();
        assert_eq!(caps, vec![Some((1, 5))]);
        assert_eq!(r.mbctype(), euc);
    }

    #[test]
    fn named_groups_disable_plain_capture() {
        let r = re("(?<x>a)(b)(?<y>c)");
        assert!(!r.noname_group_capture_is_active());
        let mut names = r.names();
        names.sort();
        assert_eq!(
            names,
            vec![(b"x".to_vec(), vec![1]), (b"y".to_vec(), vec![2])]
        );
    }

    #[test]
    fn compile_errors_use_oniguruma_text() {
        let e = Regex::new(b"(", DEFAULT, utf8(), Syntax::Ruby).unwrap_err();
        assert_eq!(e, b"end pattern with unmatched parenthesis");
        let e = Regex::new(b"(?<0>a)", DEFAULT, utf8(), Syntax::Ruby).unwrap_err();
        assert_eq!(e, b"invalid group name <0>");
    }

    #[test]
    fn options_include_syntax_defaults() {
        let r = Regex::new(b"a", OPTION_IGNORECASE, utf8(), Syntax::Ruby).unwrap();
        assert_eq!(r.options() & OPTION_IGNORECASE, OPTION_IGNORECASE);
        // Perl syntax turns SINGLELINE on by default.
        let p = Regex::new(b"a", 0, utf8(), Syntax::Perl).unwrap();
        assert_eq!(p.options() & OPTION_SINGLELINE, OPTION_SINGLELINE);
    }

    #[test]
    fn match_stack_limit_is_enforced() {
        // ext/mbstring/tests/mbregex_stack_limit2.phpt
        let s = vec![b' '; 30000];
        let r = re(r"\s+$");
        let low = MatchLimits {
            stack: Some(10000),
            retry: None,
        };
        let err = r.search(&s, 0, low).unwrap_err();
        assert_eq!(err, OnigError::MATCH_STACK_LIMIT_OVER);
        assert_eq!(err.message(), b"match-stack limit over");
        let high = MatchLimits {
            stack: Some(30001),
            retry: None,
        };
        assert!(r.search(&s, 0, high).unwrap().is_some());
    }

    #[test]
    fn anchored_match() {
        let r = re("ab");
        assert_eq!(r.match_at(b"abc", 0, MatchLimits::default()), Ok(Some(2)));
        assert_eq!(r.match_at(b"cab", 0, MatchLimits::default()), Ok(None));
    }
}
