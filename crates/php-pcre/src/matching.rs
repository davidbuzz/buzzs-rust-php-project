//! `preg_match()` / `preg_match_all()` (`php_pcre_match_impl`).

use std::sync::Arc;

use crate::cache::MatchConfig;
use crate::error::PregError;
use crate::exec::{match_value, offset_pair, pairs, populate_subpat_array, Exec};
use crate::ffi;
use crate::regex::Regex;
use crate::value::{Key, MatchArray, Value};
use crate::{PREG_OFFSET_CAPTURE, PREG_PATTERN_ORDER, PREG_SET_ORDER, PREG_UNMATCHED_AS_NULL};

/// Why a match call returned something other than an int.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchFailure {
    /// `ValueError`: `Argument #4 ($flags) must be a PREG_* constant`.
    InvalidFlags,
    /// `ValueError`: `Argument #5 ($offset) must be greater than -9223372036854775808`.
    InvalidOffset,
    /// The function returns `false` (see [`MatchOutcome::last_error`] / [`MatchOutcome::warning`]).
    Failed,
}

/// Everything `preg_match()` / `preg_match_all()` produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchOutcome {
    /// `Ok(n)` → `int(n)`; `Err(_)` → see [`MatchFailure`].
    pub result: Result<usize, MatchFailure>,
    /// New value of `$matches` when the caller passed it; `None` means PHP
    /// leaves the variable untouched (only for [`MatchFailure::InvalidOffset`]).
    /// Note that it can be partially filled even when `result` is an error.
    pub matches: Option<MatchArray>,
    /// The value to store into `preg_last_error()`, or `None` to leave it as is.
    pub last_error: Option<PregError>,
    /// An `E_WARNING` to emit (`"Get subpatterns list failed"`).
    pub warning: Option<&'static str>,
}

/// `preg_match($pattern, $subject, $matches, $flags, $offset)`.
///
/// `capture` says whether the PHP call passed `$matches` (it changes PHP's
/// behaviour slightly, e.g. named-group tables are only built then).
pub fn match_one(
    re: &Regex,
    cfg: &MatchConfig,
    subject: &[u8],
    capture: bool,
    flags: i64,
    offset: i64,
) -> MatchOutcome {
    match_impl(re, cfg, subject, capture, false, flags, offset)
}

/// `preg_match_all($pattern, $subject, $matches, $flags, $offset)`.
pub fn match_all(
    re: &Regex,
    cfg: &MatchConfig,
    subject: &[u8],
    capture: bool,
    flags: i64,
    offset: i64,
) -> MatchOutcome {
    match_impl(re, cfg, subject, capture, true, flags, offset)
}

fn fail(matches: MatchArray, last_error: Option<PregError>) -> MatchOutcome {
    MatchOutcome {
        result: Err(MatchFailure::Failed),
        matches: Some(matches),
        last_error,
        warning: None,
    }
}

/// Port of `php_pcre_match_impl`.
#[allow(clippy::too_many_lines)]
fn match_impl(
    re: &Regex,
    cfg: &MatchConfig,
    subject: &[u8],
    capture: bool,
    global: bool,
    flags: i64,
    start_offset: i64,
) -> MatchOutcome {
    if start_offset == i64::MIN {
        return MatchOutcome {
            result: Err(MatchFailure::InvalidOffset),
            matches: None,
            last_error: None,
            warning: None,
        };
    }
    let mut subpats = MatchArray::new();

    let mut order = if global { PREG_PATTERN_ORDER } else { 0 };
    let offset_capture = flags & PREG_OFFSET_CAPTURE != 0;
    let unmatched_as_null = flags & PREG_UNMATCHED_AS_NULL != 0;
    if flags & 0xff != 0 {
        order = flags & 0xff;
        if (global && !(PREG_PATTERN_ORDER..=PREG_SET_ORDER).contains(&order))
            || (!global && order != 0)
        {
            return MatchOutcome {
                result: Err(MatchFailure::InvalidFlags),
                matches: Some(subpats),
                last_error: None,
                warning: None,
            };
        }
    }

    let len = subject.len();
    let start = if start_offset < 0 {
        let back = start_offset.unsigned_abs();
        if back <= len as u64 {
            len - usize::try_from(back).unwrap_or(len)
        } else {
            0
        }
    } else {
        usize::try_from(start_offset).unwrap_or(usize::MAX)
    };
    if start > len {
        return fail(subpats, Some(PregError::Internal));
    }

    let num_subpats = re.capture_count() + 1;
    let names: Option<&[Option<Arc<[u8]>>]> = if capture && re.name_count() > 0 {
        Some(re.group_names())
    } else {
        None
    };

    let mut matched = 0usize;
    let mut error = PregError::NoError;

    let Some(mut ex) = Exec::new(re, cfg, subject) else {
        return fail(subpats, Some(PregError::Internal));
    };

    let mut match_sets: Option<Vec<MatchArray>> =
        (global && capture && order == PREG_PATTERN_ORDER)
            .then(|| vec![MatchArray::new(); num_subpats]);
    let mut marks: Option<MatchArray> = None;

    let mut pos = start;
    let mut count = ex.run(pos, ex.first_options());

    'outer: loop {
        if count >= 0 {
            // `matched:` label — re-entered after a successful NOTEMPTY retry.
            loop {
                matched += 1;
                if capture {
                    let (s, e) = ex.span();
                    if e < s {
                        return MatchOutcome {
                            result: Err(MatchFailure::Failed),
                            matches: Some(subpats),
                            last_error: Some(error),
                            warning: Some("Get subpatterns list failed"),
                        };
                    }
                    let n = pairs(count, num_subpats);
                    if global {
                        if let Some(sets) = match_sets.as_mut() {
                            let ov = ex.ov();
                            for (i, set) in sets.iter_mut().enumerate().take(n) {
                                let (s, e) = (ov[2 * i], ov[2 * i + 1]);
                                set.push(if offset_capture {
                                    offset_pair(subject, s, e, unmatched_as_null)
                                } else {
                                    match_value(subject, s, e, unmatched_as_null)
                                });
                            }
                            if let Some(mark) = ex.mark() {
                                marks.get_or_insert_with(MatchArray::new).set(
                                    Key::Index(crate::exec::off(matched - 1)),
                                    Value::Str(mark),
                                );
                            }
                            for set in sets.iter_mut().skip(n) {
                                set.push(if offset_capture {
                                    offset_pair(
                                        subject,
                                        ffi::PCRE2_UNSET,
                                        ffi::PCRE2_UNSET,
                                        unmatched_as_null,
                                    )
                                } else if unmatched_as_null {
                                    Value::Null
                                } else {
                                    Value::Str(Vec::new())
                                });
                            }
                        } else {
                            let set = populate_subpat_array(
                                subject,
                                ex.ov(),
                                names,
                                num_subpats,
                                n,
                                ex.mark(),
                                flags,
                            );
                            subpats.push(Value::Array(set));
                        }
                    } else {
                        subpats = populate_subpat_array(
                            subject,
                            ex.ov(),
                            names,
                            num_subpats,
                            n,
                            ex.mark(),
                            flags,
                        );
                        break 'outer;
                    }
                }

                let (s, e) = ex.span();
                pos = e;
                if pos == s {
                    count = ex.retry_not_empty(pos);
                    if count >= 0 {
                        if global {
                            continue;
                        }
                        break 'outer;
                    } else if count == ffi::PCRE2_ERROR_NOMATCH {
                        if pos < len {
                            pos += ex.unit_length(pos);
                        } else {
                            break 'outer;
                        }
                    } else {
                        error = PregError::from_pcre2(count);
                        break 'outer;
                    }
                }
                break;
            }
        } else if count == ffi::PCRE2_ERROR_NOMATCH {
            break;
        } else {
            error = PregError::from_pcre2(count);
            break;
        }

        if !global {
            break;
        }
        count = ex.next(pos);
    }

    if let Some(sets) = match_sets {
        for (i, set) in sets.into_iter().enumerate() {
            if let Some(name) = names.and_then(|n| n[i].as_ref()) {
                subpats.set(Key::Name(Arc::clone(name)), Value::Array(set.clone()));
            }
            subpats.push(Value::Array(set));
        }
        if let Some(marks) = marks {
            subpats.set(Key::name(b"MARK"), Value::Array(marks));
        }
    }

    MatchOutcome {
        result: if error == PregError::NoError {
            Ok(matched)
        } else {
            Err(MatchFailure::Failed)
        },
        matches: Some(subpats),
        last_error: Some(error),
        warning: None,
    }
}
